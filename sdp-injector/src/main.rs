use futures_util::stream::StreamExt;
use futures_util::Future;
use http::{Method, StatusCode, Uri};
use hyper::body::Bytes;
use hyper::server::accept;
use hyper::server::conn::AddrIncoming;
use hyper::service::{make_service_fn, service_fn};
use hyper::{Body, Request, Response, Server};
use json_patch::PatchOperation::Add;
use json_patch::{AddOperation, Patch};
use k8s_openapi::api::apps::v1::Deployment;
use k8s_openapi::api::core::v1::{
    ConfigMapKeySelector, Container, EnvVar, EnvVarSource, ObjectFieldSelector, Pod, PodDNSConfig,
    SecretKeySelector, Service, Volume,
};
use k8s_openapi::apimachinery::pkg::apis::meta::v1::Status;
use kube::api::{DynamicObject, ListParams};
use kube::core::admission::{AdmissionRequest, AdmissionResponse, AdmissionReview};
use kube::{Api, Client, Config, Resource};
use log::{debug, error, info, warn};
use rustls::{Certificate, PrivateKey, ServerConfig};
use rustls_pemfile::{read_one, Item};
use sdp_common::crd::DeviceId;
use serde::Deserialize;
use std::collections::hash_map::RandomState;
use std::collections::{HashMap, HashSet};
use std::convert::Infallible;
use std::error::Error;
use std::fmt::{Display, Formatter, Result as FResult};
use std::fs::File;
use std::future::ready;
use std::io::{BufReader, Error as IOError, ErrorKind};
use std::iter::FromIterator;
use std::path::PathBuf;
use std::pin::Pin;
use std::str::from_utf8;
use std::sync::Arc;
use tls_listener::TlsListener;
use tokio::sync::Mutex;

use sdp_common::service::{
    is_injection_disabled, Annotated, HasCredentials, ServiceCandidate, ServiceIdentity, Validated,
};

const SDP_K8S_HOST_ENV: &str = "SDP_K8S_HOST";
const SDP_K8S_HOST_DEFAULT: &str = "kubernetes.default.svc";
const SDP_K8S_NO_VERIFY_ENV: &str = "SDP_K8S_NO_VERIFY";
const SDP_SIDECARS_FILE: &str = "/opt/sdp-injector/k8s/sdp-sidecars.json";
const SDP_SIDECARS_FILE_ENV: &str = "SDP_SIDECARS_FILE";
const SDP_CERT_FILE_ENV: &str = "SDP_CERT_FILE";
const SDP_KEY_FILE_ENV: &str = "SDP_KEY_FILE";
const SDP_CERT_FILE: &str = "/opt/sdp-injector/k8s/sdp-injector-crt.pem";
const SDP_KEY_FILE: &str = "/opt/sdp-injector/k8s/sdp-injector-key.pem";
const SDP_SERVICE_CONTAINER_NAME: &str = "sdp-service";
const SDP_ANNOTATION_CLIENT_CONFIG: &str = "sdp-injector-client-config";
const SDP_ANNOTATION_CLIENT_SECRETS: &str = "sdp-injector-client-secrets";
const SDP_ANNOTATION_CLIENT_DEVICE_ID: &str = "sdp-injector-client-device-id";
const SDP_DNS_SERVICE_NAMES: [&str; 2] = ["kube-dns", "coredns"];

macro_rules! env_var {
    (value :: $env_name:expr => $value:expr) => {{
        let mut env: EnvVar = Default::default();
        env.name = $env_name.to_string();
        env.value = Some($value);
        env
    }};

    (configMap :: $env_name:expr => $map_name:expr) => {{
        let mut env: EnvVar = Default::default();
        env.name = $env_name.to_string();
        let mut env_source: EnvVarSource = Default::default();
        env_source.config_map_key_ref = Some(ConfigMapKeySelector {
            key: $env_name.to_lowercase().replace("_", "-"),
            name: Some($map_name.to_string()),
            optional: Some(false),
        });
        env.value_from = Some(env_source);
        env
    }};

    (secrets :: $env_name:expr => ($secret_name:expr, $secret_key:expr)) => {{
        let mut env: EnvVar = Default::default();
        env.name = $env_name.to_string();
        let mut env_source: EnvVarSource = Default::default();
        env_source.secret_key_ref = Some(SecretKeySelector {
            key: $secret_key.to_string(),
            name: Some($secret_name.to_string()),
            optional: Some(false),
        });
        env.value_from = Some(env_source);
        env
    }};

    (secrets :: $env_name:expr => $secret_name:expr) => {{
        env_var!(secrets :: $env_name => ($secret_name, $env_name.to_lowercase().replace("_", "-")))
    }};

    (fieldRef :: $env_name:expr => $field_path:expr) => {{
        let mut env: EnvVar = Default::default();
        env.name = $env_name.to_string();
        let mut env_source: EnvVarSource = Default::default();
        env_source.field_ref = Some(ObjectFieldSelector {
            field_path: $field_path.to_string(),
            api_version: None,
        });
        env.value_from = Some(env_source);
        env
    }};
}

trait IdentityStore {
    fn identity<'a>(
        &'a self,
        pod: &'a Pod,
    ) -> Pin<Box<dyn Future<Output = Option<(ServiceIdentity, DeviceId)>> + Send + '_>>;
}

#[derive(Default)]
struct InMemoryIdentityStore {
    identities: Mutex<HashMap<String, ServiceIdentity>>,
    device_ids: Mutex<HashMap<String, DeviceId>>,
}

impl InMemoryIdentityStore {
    async fn register_service_identity(&self, service_identity: ServiceIdentity) -> () {
        let mut hm = self.identities.lock().await;
        hm.insert(service_identity.service_id(), service_identity);
    }

    async fn register_service_device_ids(&self, service_device_ids: DeviceId) -> () {
        let mut hm = self.device_ids.lock().await;
        hm.insert(service_device_ids.service_id(), service_device_ids);
    }
}

impl IdentityStore for InMemoryIdentityStore {
    fn identity<'a>(
        &'a self,
        pod: &'a Pod,
    ) -> Pin<Box<dyn Future<Output = Option<(ServiceIdentity, DeviceId)>> + Send + '_>> {
        let fut = async move {
            let hm = self.identities.lock().await;
            let sid = hm.get(&pod.service_id());
            let hm = self.device_ids.lock().await;
            let ds = hm.get(&pod.service_id());
            match (sid, ds) {
                (Some(sid), Some(ds)) => Some((sid.clone(), ds.clone())),
                _ => None,
            }
        };
        Box::pin(fut)
    }
}

struct KubeIdentityStore {
    service_identity_api: Api<ServiceIdentity>,
    service_device_ids_api: Api<DeviceId>,
}

impl IdentityStore for KubeIdentityStore {
    fn identity<'a>(
        &'a self,
        pod: &'a Pod,
    ) -> Pin<Box<dyn Future<Output = Option<(ServiceIdentity, DeviceId)>> + Send + '_>> {
        let fut = async move {
            info!("Getting list of ServiceIdentites");
            // List all the service identities
            let ss = self
                .service_identity_api
                .list(&ListParams::default())
                .await
                .map_err(|e| {
                    error!(
                        "Error fetching service identities list for service {}: {:?}",
                        pod.service_id(), e
                    );
                })
                .map(|ss| ss.items)
                .unwrap_or(vec![]);
            // Search the service identity for service_id
            let sid = ss.iter().find(|s| s.service_id().eq(&pod.service_id()));

            info!("Getting list of DeviceIds");
            // List all the service device ids
            let ds = self
                .service_device_ids_api
                .list(&ListParams::default())
                .await
                .map_err(|e| {
                    error!(
                        "Error fetching device ids list for service {}: {:?}",
                        pod.service_id(), e
                    );
                })
                .map(|ds| ds.items)
                .unwrap_or(vec![]);

            let did = ds.iter().find(|d| {
                let owner_refs = d.meta().owner_references.as_ref();
                let owner_ref = owner_refs.and_then(|os| {
                    os.iter()
                        .find(|o| sid.is_some() && o.name == sid.unwrap().service_id())
                });
                owner_ref.is_some()
            });
            match (sid, did) {
                (Some(sid), Some(did)) => {
                    info!("Found service id and device id");
                    Some((sid.clone(), did.clone()))
                }
                _ => {
                    if sid.is_none() {
                        error!("Not found service identity for service: {}", pod.service_id());
                    }
                    if did.is_none() {
                        error!("Not found devide ids for service: {}", pod.service_id());
                    }
                    None
                }
            }
        };
        Box::pin(fut)
    }
}

fn reader_from_cwd(file_name: &str) -> Result<BufReader<File>, Box<dyn Error>> {
    let cwd = std::env::current_dir()?;
    let file = File::open(cwd.join(PathBuf::from(file_name).as_path()))?;
    Ok(BufReader::new(file))
}

async fn dns_service_discover<'a>(k8s_client: &'a Client) -> Option<Service> {
    let names: HashSet<&str, RandomState> = HashSet::from_iter(SDP_DNS_SERVICE_NAMES);
    let services_api: Api<Service> = Api::namespaced(k8s_client.clone(), "kube-system");
    info!("Discovering K8S DNS service");
    let services = services_api.list(&ListParams::default()).await;
    services
        .map(|xs| {
            let mut iter = xs.items.into_iter();
            iter.find(|s| {
                s.metadata
                    .name
                    .as_ref()
                    .map(|ns| names.contains(ns.as_str()))
                    .unwrap_or(false)
            })
        })
        .unwrap_or_else(|e| {
            warn!("Unable to discover k8s DNS service {}", e);
            Option::None
        })
}

trait K8SDNSService {
    fn maybe_ip(&self) -> Option<&String>;
}

impl K8SDNSService for Option<Service> {
    fn maybe_ip(&self) -> Option<&String> {
        self.as_ref()
            .and_then(|s| s.spec.as_ref())
            .and_then(|s| s.cluster_ip.as_ref())
    }
}
trait Patched {
    fn patch(&self, environment: &mut ServiceEnvironment) -> Result<Patch, Box<dyn Error>>;
}

#[derive(Debug, PartialEq)]
struct ServiceEnvironment {
    service_name: String,
    client_config: String,
    client_secret_name: String,
    client_secret_controller_url_key: String,
    client_secret_user_key: String,
    client_secret_pwd_key: String,
    client_device_id: String,
    n_containers: String,
    k8s_dns_service_ip: Option<String>,
}

impl ServiceEnvironment {
    async fn create<E: IdentityStore>(pod: &Pod, store: Arc<E>) -> Option<Self> {
        if let Some(env) = ServiceEnvironment::from_pod(pod) {
            Some(env)
        } else {
            ServiceEnvironment::from_identity_store(pod, store).await
        }
    }

    async fn from_identity_store<E: IdentityStore>(
        pod: &Pod,
        store: Arc<E>,
    ) -> Option<Self> {
        store.identity(pod).await.map(|(s, d)| {
            let (user_field, password_field, profile_field) =
                s.spec.service_user.secrets_field_names(true);
            let service_id = s.service_id();
            let secrets_name = s
                .credentials()
                .secrets_name(&s.spec.service_namespace, &s.spec.service_name);
            let config_name = s
                .credentials()
                .config_name(&s.spec.service_namespace, &s.spec.service_name);
            ServiceEnvironment {
                service_name: service_id.to_string(),
                client_config: config_name,
                client_secret_name: secrets_name,
                client_secret_controller_url_key: profile_field,
                client_secret_pwd_key: password_field,
                client_secret_user_key: user_field,
                client_device_id: d.spec.uuids[0].to_string(),
                n_containers: "0".to_string(),
                k8s_dns_service_ip: None,
            }
        })
    }

    fn from_pod(pod: &Pod) -> Option<Self> {
        let config = pod.annotation(SDP_ANNOTATION_CLIENT_CONFIG);
        let secret = pod.annotation(SDP_ANNOTATION_CLIENT_SECRETS);
        let device_id = pod.annotation(SDP_ANNOTATION_CLIENT_DEVICE_ID);
        let user_field = format!("{}-user", pod.service_id());
        let pwd_field = format!("{}-password", pod.service_id());
        secret.map(|s| ServiceEnvironment {
            service_name: pod.service_id(),
            client_config: config
                .map(|s| s.clone())
                .unwrap_or(format!("{}-service-config", pod.service_id())),
            client_secret_name: s.to_string(),
            client_secret_controller_url_key: pwd_field.to_string(),
            client_secret_pwd_key: pwd_field,
            client_secret_user_key: user_field,
            client_device_id: device_id
                .map(|s| s.clone())
                .unwrap_or(uuid::Uuid::new_v4().to_string()),
            n_containers: "0".to_string(),
            k8s_dns_service_ip: None,
        })
    }

    fn variables(&self, container_name: &str) -> Vec<EnvVar> {
        let k8s_dns = self
            .k8s_dns_service_ip
            .as_ref()
            .map(|s| s.clone())
            .unwrap_or("".to_string());
        let mut envs: Vec<EnvVar> = vec![
            env_var!(value :: "POD_N_CONTAINERS" => self.n_containers.clone()),
            env_var!(value :: "SERVICE_NAME" => self.service_name.clone()),
        ];

        if container_name == SDP_SERVICE_CONTAINER_NAME {
            envs.extend([
                env_var!(
                    configMap :: "CLIENT_LOG_LEVEL" => self.client_config),
                env_var!(
                    value :: "CLIENT_DEVICE_ID" => self.client_device_id.clone()),
                env_var!(
                    secrets :: "CLIENT_CONTROLLER_URL" => (self.client_secret_name, self.client_secret_controller_url_key)),
                env_var!(
                    secrets :: "CLIENT_USERNAME" => (self.client_secret_name, self.client_secret_user_key)),
                env_var!(
                    secrets :: "CLIENT_PASSWORD" => (self.client_secret_name, self.client_secret_pwd_key)),
                env_var!(
                    fieldRef :: "POD_NODE" => "spec.nodeName"),
                env_var!(
                    fieldRef :: "POD_NAME" => "metadata.name"),
                env_var!(
                    fieldRef :: "POD_NAMESPACE" => "metadata.namespace"),
                env_var!(
                    fieldRef :: "POD_SERVICE_ACCOUNT" => "spec.serviceAccountName"),
            ]);
        }
        if self.k8s_dns_service_ip.is_some() {
            envs.insert(0, env_var!(value :: "K8S_DNS_SERVICE" => k8s_dns));
        }
        envs
    }
}

#[derive(Clone)]
struct SDPPod {
    pod: Pod,
    sdp_sidecars: Arc<SDPSidecars>,
    k8s_dns_service: Option<Service>,
}

impl SDPPod {
    fn containers(&self) -> Option<&Vec<Container>> {
        self.pod.spec.as_ref().map(|s| &s.containers)
    }

    fn volumes(&self) -> Option<&Vec<Volume>> {
        self.pod.spec.as_ref().and_then(|s| s.volumes.as_ref())
    }

    fn sidecars(&self) -> Option<Vec<&Container>> {
        self.containers().map(|cs| {
            cs.iter()
                .filter(|&c| self.sdp_sidecars.container_names().contains(&c.name))
                .collect()
        })
    }

    fn sidecar_names(&self) -> Option<Vec<String>> {
        self.sidecars()
            .map(|xs| xs.iter().map(|&x| x.name.clone()).collect())
    }

    fn container_names(&self) -> Option<Vec<String>> {
        self.containers()
            .map(|xs| xs.iter().map(|x| x.name.clone()).collect())
    }

    fn volume_names(&self) -> Option<Vec<String>> {
        self.volumes()
            .map(|vs| vs.iter().map(|v| v.name.clone()).collect())
    }

    fn has_any_sidecars(&self) -> bool {
        self.sidecars().map(|xs| xs.len() != 0).unwrap_or(false)
    }

    fn has_containers(&self) -> bool {
        self.containers().unwrap_or(&vec![]).len() > 0
    }
}

impl ServiceCandidate for SDPPod {
    fn name(&self) -> String {
        ServiceCandidate::name(&self.pod)
    }

    fn namespace(&self) -> String {
        ServiceCandidate::namespace(&self.pod)
    }

    fn labels(&self) -> std::collections::HashMap<String, String> {
        HashMap::from_iter(
            ServiceCandidate::labels(&self.pod)
                .iter()
                .map(|s| (s.0.clone(), s.1.clone())),
        )
    }

    fn is_candidate(&self) -> bool {
        self.has_containers() && !self.has_any_sidecars() && !is_injection_disabled(&self.pod)
    }
}

impl Patched for SDPPod {
    fn patch(&self, environment: &mut ServiceEnvironment) -> Result<Patch, Box<dyn Error>> {
        info!("Patching POD with SDP client");

        // Fill # of contaienrs
        let n_containers = self.containers().map(|xs| xs.len()).unwrap_or(0);
        environment.n_containers = format!("{}", n_containers);

        // Fill DNS service ip
        environment.k8s_dns_service_ip = self.k8s_dns_service.maybe_ip().map(|s| s.clone());

        let mut patches = vec![];
        if self.is_candidate() {
            let k8s_dns_service_ip = self.k8s_dns_service.maybe_ip();
            match k8s_dns_service_ip {
                Some(ip) => {
                    info!("Found K8S DNS service IP: {}", ip);
                }
                None => {
                    warn!("Unable to get K8S service IP");
                }
            };
            for c in self.sdp_sidecars.containers.clone().iter_mut() {
                c.env = Some(environment.variables(&c.name));
                patches.push(Add(AddOperation {
                    path: "/spec/containers/-".to_string(),
                    value: serde_json::to_value(&c)?,
                }));
            }
            if self.volumes().is_some() {
                for v in self.sdp_sidecars.volumes.iter() {
                    patches.push(Add(AddOperation {
                        path: "/spec/volumes/-".to_string(),
                        value: serde_json::to_value(&v)?,
                    }));
                }
            } else {
                patches.push(Add(AddOperation {
                    path: "/spec/volumes".to_string(),
                    value: serde_json::to_value(&self.sdp_sidecars.volumes)?,
                }));
            }
            // Patch DNSConfiguration now
            patches.push(Add(AddOperation {
                path: "/spec/dnsConfig".to_string(),
                value: serde_json::to_value(&self.sdp_sidecars.dns_config())?,
            }));
            patches.push(Add(AddOperation {
                path: "/spec/dnsPolicy".to_string(),
                value: serde_json::to_value("None".to_string())?,
            }))
        }
        if patches.is_empty() {
            debug!("POD does not require patching");
            Ok(Patch(vec![]))
        } else {
            Ok(Patch(patches))
        }
    }
}

impl Validated for SDPPod {
    fn validate(&self) -> Result<(), String> {
        let expected_volume_names =
            HashSet::from_iter(self.sdp_sidecars.volumes.iter().map(|v| v.name.clone()));
        let expected_container_names =
            HashSet::from_iter(self.sdp_sidecars.containers.iter().map(|c| c.name.clone()));
        let container_names =
            HashSet::from_iter(self.sidecar_names().unwrap_or(vec![]).iter().cloned());
        let volume_names =
            HashSet::from_iter(self.volume_names().unwrap_or(vec![]).iter().cloned());
        let mut errors: Vec<String> = vec![];
        if !expected_container_names.is_subset(&container_names) {
            let container_i: HashSet<String> = container_names
                .intersection(&expected_container_names)
                .cloned()
                .collect();
            let mut missing_containers: Vec<String> = expected_container_names
                .difference(&container_i)
                .cloned()
                .collect();
            missing_containers.sort();
            errors.push(format!(
                "POD is missing needed containers: {}",
                missing_containers.join(", ")
            ));
        }
        if !expected_volume_names.is_subset(&volume_names) {
            let volumes_i: HashSet<String> = volume_names
                .intersection(&expected_volume_names)
                .cloned()
                .collect();
            let mut missing_volumes: Vec<String> = expected_volume_names
                .difference(&volumes_i)
                .cloned()
                .collect();
            missing_volumes.sort();
            errors.push(format!(
                "POD is missing needed volumes: {}",
                missing_volumes.join(", ")
            ));
        }
        if errors.is_empty() {
            Ok(())
        } else {
            Err(format!("Unable to run SDP client on POD: {}", errors.join("\n")).to_string())
        }
    }
}

impl Display for SDPPod {
    fn fmt(&self, f: &mut Formatter<'_>) -> FResult {
        write!(
            f,
            "POD(name:{}, candidate:{}, containers:[{}], volumes: [{}])",
            self.name(),
            self.is_candidate(),
            self.container_names().unwrap_or(vec![]).join(","),
            self.volume_names().unwrap_or(vec![]).join(",")
        )
    }
}

#[derive(Debug, Clone, Deserialize)]
struct DNSConfig {
    searches: String,
}

impl Default for DNSConfig {
    fn default() -> Self {
        DNSConfig {
            searches: "svc.cluster.local cluster.local".to_string(),
        }
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
struct SDPSidecars {
    containers: Box<Vec<Container>>,
    volumes: Box<Vec<Volume>>,
    #[serde(rename = "dnsConfig")]
    dns_config: Box<DNSConfig>,
}

#[derive(Clone)]
struct SDPInjectorContext<E: IdentityStore> {
    sdp_sidecars: Arc<SDPSidecars>,
    k8s_client: Client,
    identity_store: Arc<E>,
}

#[derive(Debug, Clone)]
struct SDPPatchContext<E: IdentityStore> {
    sdp_sidecars: Arc<SDPSidecars>,
    identity_store: Arc<E>,
    k8s_dns_service: Option<Service>,
}

impl SDPSidecars {
    fn container_names(&self) -> Vec<String> {
        self.containers.iter().map(|c| c.name.clone()).collect()
    }

    fn dns_config(&self) -> PodDNSConfig {
        let searches = Some(
            self.dns_config
                .searches
                .split(" ")
                .map(|s| s.trim().to_string())
                .collect(),
        );
        PodDNSConfig {
            nameservers: Some(vec!["127.0.0.1".to_string()]),
            options: None,
            searches: searches,
        }
    }
}

fn load_sidecar_containers() -> Result<SDPSidecars, Box<dyn Error>> {
    debug!("Loading SDP context");
    let cwd = std::env::current_dir()?;
    let mut sidecars_path = cwd.join(PathBuf::from(SDP_SIDECARS_FILE).as_path());
    if let Ok(sidecars_file) = std::env::var(SDP_SIDECARS_FILE_ENV) {
        sidecars_path = PathBuf::from(sidecars_file);
    }
    let file = File::open(sidecars_path)?;
    let reader = BufReader::new(file);
    let sdp_sidecars = serde_json::from_reader(reader)?;
    debug!("SDP context loaded successful");
    Ok(sdp_sidecars)
}

fn load_cert_files() -> Result<(Option<Item>, Option<Item>), Box<dyn Error>> {
    let cert_file = std::env::var(SDP_CERT_FILE_ENV).unwrap_or(SDP_CERT_FILE.to_string());
    let key_file = std::env::var(SDP_KEY_FILE_ENV).unwrap_or(SDP_KEY_FILE.to_string());
    let mut cert_buf = reader_from_cwd(&cert_file)
        .map_err(|e| format!("Unable to read cert file {}: {}", &cert_file, e))?;
    let mut key_buf = reader_from_cwd(&key_file)
        .map_err(|e| format!("Unable to read key file {}: {}", &key_file, e))?;
    Ok((
        read_one(&mut cert_buf).map_err(|e| format!("Error reading cert file: {}", e))?,
        read_one(&mut key_buf).map_err(|e| format!("Error reading key file: {}", e))?,
    ))
}

async fn patch_deployment(
    admission_request: AdmissionRequest<Deployment>,
) -> Result<AdmissionReview<DynamicObject>, Box<dyn Error>> {
    let admission_response = AdmissionResponse::from(&admission_request);
    let mut patches = vec![];
    patches.push(Add(AddOperation {
        path: "/metadata/annotations/sdp-injection".to_string(),
        value: serde_json::to_value("enabled")?,
    }));
    let admission_response = admission_response.with_patch(Patch(patches))?;
    Ok(admission_response.into_review())
}

async fn patch_pod<E: IdentityStore>(
    admission_request: AdmissionRequest<Pod>,
    sdp_patch_context: SDPPatchContext<E>,
) -> Result<AdmissionReview<DynamicObject>, Box<dyn Error>> {
    let pod = admission_request
        .object
        .as_ref()
        .ok_or("Admission request does not contain a POD")?;
    let sdp_pod = SDPPod {
        pod: pod.clone(),
        sdp_sidecars: Arc::clone(&sdp_patch_context.sdp_sidecars),
        k8s_dns_service: sdp_patch_context.k8s_dns_service,
    };
    let mut admission_response = AdmissionResponse::from(&admission_request);

    // Try to get the service environment and use it to patch the POD
    let mut environment =
        ServiceEnvironment::create(pod, Arc::clone(&sdp_patch_context.identity_store)).await;
    match environment.as_mut().map(|mut env| sdp_pod.patch(&mut env)) {
        Some(Ok(patch)) => {
            info!(
                "POD for service {} has {} patches",
                pod.service_id(),
                patch.0.len()
            );
            admission_response = admission_response.with_patch(patch)?
        }
        Some(Err(error)) => {
            warn!(
                "Unable to patch POD for service {}: {}",
                pod.service_id(),
                error.to_string()
            );
            let mut status: Status = Default::default();
            status.code = Some(400);
            status.message = Some(format!("This POD can not be patched {}", error.to_string()));
            admission_response.allowed = false;
            admission_response.result = status;
        }
        None => {
            warn!(
                "Unable to patch POD for service {}. Service is not registered, trying later.",
                pod.service_id()
            );
            let mut status: Status = Default::default();
            status.code = Some(503);
            status.message =
                Some("Unable to find ServiceIdentity for this pod, retry later".to_string());
            admission_response.allowed = false;
            admission_response.result = status;
        }
    }
    Ok(admission_response.into_review())
}

async fn mutate<E: IdentityStore>(
    body: Bytes,
    sdp_injector_context: Arc<SDPInjectorContext<E>>,
) -> Result<Body, Box<dyn Error>> {
    let k8s_dns_service = dns_service_discover(&(sdp_injector_context.k8s_client)).await;
    let sdp_patch_context = SDPPatchContext {
        k8s_dns_service: k8s_dns_service,
        sdp_sidecars: Arc::clone(&sdp_injector_context.sdp_sidecars),
        identity_store: Arc::clone(&sdp_injector_context.identity_store),
    };
    let body = from_utf8(&body).map(|s| s.to_string())?;
    let admission_review = serde_json::from_str::<AdmissionReview<DynamicObject>>(&body)?;
    match admission_review
        .request
        .as_ref()
        .map(|r| r.resource.resource.as_str())
    {
        Some("pods") => {
            let admission_review: AdmissionReview<Pod> = serde_json::from_str(&body)?;
            let admission_request: AdmissionRequest<Pod> = admission_review.try_into()?;
            let service_id = admission_request
                .object
                .as_ref()
                .map(|p| p.service_id())
                .unwrap_or("unknown".to_string());
            info!("Patching POD for service {}", service_id);
            let admission_response = patch_pod(admission_request, sdp_patch_context).await?;
            Ok(Body::from(serde_json::to_string(&admission_response)?))
        }
        Some("deployments") => {
            let admission_review: AdmissionReview<Deployment> = serde_json::from_str(&body)?;
            let admission_request: AdmissionRequest<Deployment> = admission_review.try_into()?;
            let service_id = admission_request
                .object
                .as_ref()
                .map(|d| d.service_id())
                .unwrap_or("unknown".to_string());
            info!("Patching Deployment: {}", service_id);
            let admission_response = patch_deployment(admission_request).await?;
            Ok(Body::from(serde_json::to_string(&admission_response)?))
        }
        _ => {
            let err_str = "Unknown resource to patch";
            error!("{}", err_str);
            let err = Box::<dyn Error>::from(err_str.to_string());
            Err(err)
        }
    }
}

async fn validate(body: Bytes, sdp_sidecars: Arc<SDPSidecars>) -> Result<Body, Box<dyn Error>> {
    let body = from_utf8(&body).map(|s| s.to_string())?;
    let admission_review = serde_json::from_str::<AdmissionReview<Pod>>(&body)?;
    let admission_request: AdmissionRequest<Pod> = admission_review
        .request
        .ok_or("Not found request in admission review")?;
    let pod = admission_request
        .object
        .as_ref()
        .ok_or("Admission review does not contain a POD")?;
    let sdp_pod = SDPPod {
        pod: pod.clone(),
        sdp_sidecars: sdp_sidecars,
        k8s_dns_service: Option::None,
    };
    let mut admission_response = AdmissionResponse::from(&admission_request);
    if let Err(error) = sdp_pod.validate() {
        let mut status: Status = Default::default();
        status.code = Some(40);
        status.message = Some(error);
        admission_response.allowed = false;
        admission_response.result = status;
    }
    Ok(Body::from(serde_json::to_string(&admission_response)?))
}

async fn get_dns_service(k8s_client: &Client) -> Result<Option<Body>, Box<dyn Error>> {
    let k8s_dns_service = dns_service_discover(k8s_client).await;
    match k8s_dns_service {
        Some(s) => Ok(Some(Body::from(serde_json::to_string(&s)?))),
        None => Ok(None),
    }
}

#[cfg(test)]
mod tests {
    use crate::{
        load_sidecar_containers, patch_pod, InMemoryIdentityStore, Patched, SDPPatchContext,
        SDPPod, SDPSidecars, ServiceEnvironment, Validated, SDP_ANNOTATION_CLIENT_CONFIG,
        SDP_ANNOTATION_CLIENT_DEVICE_ID, SDP_ANNOTATION_CLIENT_SECRETS, SDP_SERVICE_CONTAINER_NAME,
        SDP_SIDECARS_FILE_ENV,
    };
    use json_patch::Patch;
    use k8s_openapi::api::core::v1::{Container, Pod, Service, ServiceSpec, ServiceStatus, Volume};
    use kube::core::admission::AdmissionReview;
    use kube::core::ObjectMeta;
    use sdp_common::crd::{DeviceId, DeviceIdSpec, ServiceIdentity, ServiceIdentitySpec};
    use sdp_common::service::{ServiceCandidate, ServiceUser};
    use sdp_macros::{service_device_ids, service_identity, service_user};
    use serde_json::json;
    use std::collections::{BTreeMap, HashMap, HashSet};
    use std::iter::FromIterator;
    use std::rc::Rc;
    use std::sync::Arc;
    use tokio::sync::mpsc::channel;

    fn load_sidecar_containers_env() -> Result<SDPSidecars, String> {
        let manifest_dir =
            std::env::var("CARGO_MANIFEST_DIR").expect("env var CARGO_MANIFEST_DIR not defined!");
        std::env::set_var(
            SDP_SIDECARS_FILE_ENV,
            format!("{}/../tests/sdp-sidecars.json", manifest_dir),
        );
        load_sidecar_containers()
            .map_err(|e| format!("Unable to load the sidecar information {}", e.to_string()))
    }

    macro_rules! set_pod_field {
        ($pod:expr, containers => $cs:expr) => {
            let test_cs: Vec<Container> = $cs
                .iter()
                .map(|x| {
                    let mut c: Container = Default::default();
                    c.name = x.to_string();
                    c
                })
                .collect();
            if let Some(spec) = $pod.spec.as_mut() {
                spec.containers = test_cs;
            }
        };
        ($pod:expr, volumes => $vs:expr) => {
            let volumes: Vec<Volume> = $vs
                .iter()
                .map(|x| {
                    let mut v: Volume = Default::default();
                    v.name = x.to_string();
                    v
                })
                .collect();

            if volumes.len() > 0 {
                if let Some(spec) = $pod.spec.as_mut() {
                    spec.volumes = Some(volumes);
                }
            }
        };
        ($pod:expr, annotations => $cs:expr) => {
            let mut bm = BTreeMap::new();
            for (k, v) in $cs {
                bm.insert(k.to_string(), v.to_string());
            }
            $pod.metadata.annotations = Some(bm);
        };
    }

    macro_rules! test_pod {
        ($n:tt) => {{
            let mut pod: Pod = Default::default();
            pod.spec = Some(Default::default());
            test_pod!($n, pod)
        }};
        ($n:tt, $pod:expr) => {{
            $pod.metadata.namespace = Some(format!("{}{}", stringify!(ns), $n));
            $pod.metadata.name = Some(format!("srv{}-replica{}-testpod{}", stringify!($n), stringify!($n), stringify!($n)));
            $pod
        }};
        ($n:tt, $($fs:ident => $es:expr),*) => {{
            let mut pod: Pod = Default::default();
            pod.spec = Some(Default::default());
            test_pod!($n, pod, $($fs => $es),*)
        }};
        ($n:tt, $pod:expr,$f:ident => $e:expr) => {{
            set_pod_field!($pod, $f => $e);
            test_pod!($n, $pod)
        }};
        ($n:tt, $pod:expr,$f:ident => $e:expr,$($fs:ident => $es:expr),*) => {{
            set_pod_field!($pod, $f => $e);
            test_pod!($n, $pod,$($fs => $es),*)
        }};
    }

    fn assert_tests(test_results: &[TestResult]) -> () {
        let mut test_errors: Vec<(usize, String, String)> = Vec::new();
        let ok = test_results.iter().enumerate().fold(
            true,
            |total, (c, (result, test_description, error_message))| {
                if !result {
                    test_errors.push((c, test_description.clone(), error_message.clone()));
                }
                total && *result
            },
        );
        if !ok {
            let errors: Vec<String> = test_errors
                .iter()
                .map(|x| format!("Test {} [#{}] failed, reason: {}", x.1, x.0, x.2).to_string())
                .collect();
            panic!("Inject test failed:\n{}", errors.join("\n"));
        }
        assert_eq!(true, true);
    }

    #[derive(Debug)]
    struct TestPatch<'a> {
        pod: Pod,
        needs_patching: bool,
        client_config_map: &'a str,
        client_secrets: &'a str,
        envs: Vec<(String, Option<String>)>,
        service: Service,
        dns_searches: Option<Vec<String>>,
    }

    impl Default for TestPatch<'_> {
        fn default() -> Self {
            TestPatch {
                pod: test_pod!(0),
                needs_patching: false,
                client_config_map: "ns0-srv0-service-config",
                client_secrets: "ns0-srv0-service-user",
                envs: vec![],
                service: Service::default(),
                dns_searches: Some(vec![
                    "svc.cluster.local".to_string(),
                    "cluster.local".to_string(),
                ]),
            }
        }
    }

    struct TestValidate {
        pod: Pod,
        validation_errors: Option<String>,
    }

    type TestResult = (bool, String, String);

    fn patch_tests(sdp_sidecars: &SDPSidecars) -> Vec<TestPatch> {
        let sdp_sidecar_names = sdp_sidecars.container_names();
        vec![
            TestPatch {
                pod: test_pod!(0),
                envs: vec![
                    ("POD_N_CONTAINERS".to_string(), Some("0".to_string())),
                    ("SERVICE_NAME".to_string(), Some("ns0-srv0".to_string())),
                ],
                service: Service::default(),
                ..Default::default()
            },
            TestPatch {
                pod: test_pod!(1, containers => vec!["random-service"]),
                needs_patching: true,
                envs: vec![
                    ("POD_N_CONTAINERS".to_string(), Some("1".to_string())),
                    ("K8S_DNS_SERVICE".to_string(), Some("".to_string())),
                    ("CLIENT_DEVICE_ID".to_string(), Some("6661".to_string())),
                    ("SERVICE_NAME".to_string(), Some("ns1-srv1".to_string())),
                ],
                client_config_map: "ns1-srv1-service-config",
                client_secrets: "ns1-srv1-service-user",
                ..Default::default()
            },
            TestPatch {
                pod: test_pod!(2, containers => vec!["random-service"],
                                  annotations => vec![("sdp-injector", "false")]),
                envs: vec![
                    ("POD_N_CONTAINERS".to_string(), Some("1".to_string())),
                    ("SERVICE_NAM".to_string(), Some("ns2-srv2".to_string())),
                ],
                client_config_map: "ns2-srv2-service-config",
                client_secrets: "ns2-srv2-service-user",
                ..Default::default()
            },
            TestPatch {
                pod: test_pod!(3, containers => vec!["random-service"],
                                  annotations => vec![("sdp-injector", "who-knows"),
                                                      (SDP_ANNOTATION_CLIENT_SECRETS, "some-secrets"),
                                                      (SDP_ANNOTATION_CLIENT_CONFIG, "some-config-map"),
                                                      (SDP_ANNOTATION_CLIENT_DEVICE_ID, "666-6")]),
                needs_patching: true,
                client_config_map: "some-config-map",
                client_secrets: "some-secrets",
                envs: vec![
                    ("POD_N_CONTAINERS".to_string(), Some("1".to_string())),
                    ("K8S_DNS_SERVICE".to_string(), Some("".to_string())),
                    ("CLIENT_DEVICE_ID".to_string(), Some("666-6".to_string())),
                    ("SERVICE_NAME".to_string(), Some("ns3-srv3".to_string())),
                ],
                ..Default::default()
            },
            TestPatch {
                pod: test_pod!(4, containers => vec!["random-service"],
                                  annotations => vec![("sdp-injector", "true"),
                                                      (SDP_ANNOTATION_CLIENT_DEVICE_ID, "666-10"),
                                                      (SDP_ANNOTATION_CLIENT_SECRETS, "some-secrets")]),
                needs_patching: true,
                client_secrets: "some-secrets",
                client_config_map: "ns4-srv4-service-config",
                envs: vec![
                    ("POD_N_CONTAINERS".to_string(), Some("1".to_string())),
                    ("K8S_DNS_SERVICE".to_string(), Some("".to_string())),
                    ("CLIENT_DEVICE_ID".to_string(), Some("666-10".to_string())),
                    ("SERVICE_NAME".to_string(), Some("ns4-srv4".to_string())),
                ],
                ..Default::default()
            },
            TestPatch {
                pod: test_pod!(5, containers => vec!["some-random-service-1",
                                                     "some-random-service-2"],
                                  annotations => vec![(SDP_ANNOTATION_CLIENT_CONFIG, "some-config-map")]),
                needs_patching: true,
                envs: vec![
                    ("POD_N_CONTAINERS".to_string(), Some("2".to_string())),
                    ("K8S_DNS_SERVICE".to_string(), Some("10.0.0.10".to_string())),
                    ("SERVICE_NAME".to_string(), Some("ns5-srv5".to_string())),
                    ("CLIENT_DEVICE_ID".to_string(), Some("6665".to_string())),
                ],
                service: Service {
                    metadata: ObjectMeta::default(),
                    spec: Some(ServiceSpec {
                        cluster_ip: Some(String::from("10.0.0.10")),
                        ..Default::default()
                    }),
                    status: Some(ServiceStatus::default()),
                },
                client_config_map: "ns5-srv5-service-config",
                client_secrets: "ns5-srv5-service-user",
                ..Default::default()
            },
            TestPatch {
                pod: test_pod!(6, containers => vec![sdp_sidecar_names[0].clone(),
                                                     sdp_sidecar_names[1].clone(),
                                                     "some-random-service".to_string()]),
                envs: vec![
                    ("POD_N_CONTAINERS".to_string(), Some("3".to_string())),
                    ("SERVICE_NAME".to_string(), Some("ns6-srv6".to_string())),
                ],
                client_config_map: "ns6-srv6-service-config",
                client_secrets: "ns6-srv6-service-user",
                ..Default::default()
            },
            TestPatch {
                pod: test_pod!(7, containers => vec![sdp_sidecar_names[0].clone(),
                                                     sdp_sidecar_names[1].clone(),
                                                     "some-random-service".to_string()],
                                  annotations => vec![("sdp-injector", "true")]),
                envs: vec![
                    ("POD_N_CONTAINERS".to_string(), Some("3".to_string())),
                    ("SERVICE_NAME".to_string(), Some("ns7-srv7".to_string())),
                ],
                client_config_map: "ns7-srv7-service-config",
                client_secrets: "ns7-srv7-service-user",
                ..Default::default()
            },
            TestPatch {
                pod: test_pod!(8, containers => vec![sdp_sidecar_names[1].clone(),
                                                     "some-random-service".to_string()]),
                envs: vec![
                    ("POD_N_CONTAINERS".to_string(), Some("2".to_string())),
                    ("SERVICE_NAME".to_string(), Some("ns8-srv8".to_string())),
                ],
                client_config_map: "ns8-srv8-service-config",
                client_secrets: "ns8-srv8-service-user",
                ..Default::default()
            },
            TestPatch {
                pod: test_pod!(9, containers => vec![sdp_sidecar_names[1].clone(),
                                                    "some-random-service".to_string()],
                                  annotations => vec![("sdp-injector", "true")]),
                envs: vec![
                    ("POD_N_CONTAINERS".to_string(), Some("2".to_string())),
                    ("SERVICE_NAME".to_string(), Some("ns9-srv9".to_string())),
                ],
                client_config_map: "ns9-srv9-service-config",
                client_secrets: "ns9-srv9-service-user",
                ..Default::default()
            },
        ]
    }

    fn validation_tests() -> Vec<TestValidate> {
        vec![
            TestValidate {
                pod: test_pod!(0),
                validation_errors: Some(r#"Unable to run SDP client on POD: POD is missing needed containers: sdp-dnsmasq, sdp-driver, sdp-service
POD is missing needed volumes: pod-info, run-sdp-dnsmasq, run-sdp-driver, tun-device"#.to_string()),
            },
            TestValidate {
                pod: test_pod!(1, containers => vec!["sdp-dnsmasq"]),
                validation_errors: Some(r#"Unable to run SDP client on POD: POD is missing needed containers: sdp-driver, sdp-service
POD is missing needed volumes: pod-info, run-sdp-dnsmasq, run-sdp-driver, tun-device"#.to_string()),
            },
            TestValidate {
                pod: test_pod!(2, containers => vec!["sdp-dnsmasq", "sdp-service"]),
                validation_errors: Some(r#"Unable to run SDP client on POD: POD is missing needed containers: sdp-driver
POD is missing needed volumes: pod-info, run-sdp-dnsmasq, run-sdp-driver, tun-device"#.to_string()),
            },
            TestValidate {
                pod: test_pod!(3, containers => vec!["sdp-service", "sdp-dnsmasq", "sdp-driver"]),
                validation_errors: Some(r#"Unable to run SDP client on POD: POD is missing needed volumes: pod-info, run-sdp-dnsmasq, run-sdp-driver, tun-device"#.to_string()),
            },
            TestValidate {
                pod: test_pod!(4, containers => vec!["sdp-service", "sdp-dnsmasq", "sdp-driver"],
                                  volumes => vec!["run-sdp-dnsmasq", "run-sdp-driver"]),
                validation_errors: Some(r#"Unable to run SDP client on POD: POD is missing needed volumes: pod-info, tun-device"#.to_string()),
            },
            TestValidate {
                pod: test_pod!(5, containers => vec!["sdp-service", "sdp-dnsmasq", "sdp-driver"],
                                  volumes => vec!["run-sdp-driver", "run-sdp-dnsmasq"]),
                validation_errors: Some(r#"Unable to run SDP client on POD: POD is missing needed volumes: pod-info, tun-device"#.to_string()),
            },
            TestValidate {
                pod: test_pod!(6, containers => vec!["sdp-driver"],
                                  volumes => vec!["run-sdp-driver", "tun-device", "pod-info", "run-sdp-dnsmasq"]),
                validation_errors: Some(r#"Unable to run SDP client on POD: POD is missing needed containers: sdp-dnsmasq, sdp-service"#.to_string()),
            },
            TestValidate {
                pod: test_pod!(7, containers => vec!["sdp-service", "sdp-dnsmasq", "sdp-driver"],
                                  volumes => vec!["run-sdp-driver", "run-sdp-dnsmasq", "tun-device", "pod-info"]),
                validation_errors: None,
            },
            TestValidate {
                pod: test_pod!(8, containers => vec!["_sdp-service", "_sdp-dnsmasq", "_sdp-driver"],
                                  volumes => vec!["_run-appgate", "_tun-device"]),
                validation_errors: Some(r#"Unable to run SDP client on POD: POD is missing needed containers: sdp-dnsmasq, sdp-driver, sdp-service
POD is missing needed volumes: pod-info, run-sdp-dnsmasq, run-sdp-driver, tun-device"#.to_string()),
            },
        ]
    }

    #[test]
    fn needs_patching() {
        let sdp_sidecars =
            Arc::new(load_sidecar_containers_env().expect("Unable to load test SDPSidecars: "));
        let results: Vec<TestResult> = patch_tests(&sdp_sidecars)
            .iter()
            .map(|t| {
                let sdp_pod = SDPPod {
                    pod: t.pod.clone(),
                    sdp_sidecars: Arc::clone(&sdp_sidecars),
                    k8s_dns_service: Some(t.service.clone()),
                };
                let needs_patching = sdp_pod.is_candidate();
                (
                    t.needs_patching == needs_patching,
                    "Needs patching simple".to_string(),
                    format!("Got {}, expected {}", needs_patching, t.needs_patching),
                )
            })
            .collect();
        assert_tests(&results)
    }

    #[tokio::test]
    async fn test_pod_patch_sidecars() {
        let sdp_sidecars =
            Arc::new(load_sidecar_containers_env().expect("Unable to load sidecars context"));

        let expected_containers = |vs: &[String]| -> HashSet<String> {
            let mut xs: Vec<String> = sdp_sidecars.container_names();
            xs.extend_from_slice(vs);
            HashSet::from_iter(xs.iter().cloned())
        };

        let expected_volumes = |vs: &[String]| -> HashSet<String> {
            let mut xs: Vec<String> = sdp_sidecars
                .volumes
                .iter()
                .map(|c| c.name.clone())
                .collect();
            xs.extend_from_slice(vs);
            HashSet::from_iter(xs.iter().cloned())
        };
        let patch_pod = |sdp_pod: &SDPPod, patch: Patch| -> Result<Pod, String> {
            let mut unpatched_pod = serde_json::to_value(&sdp_pod.pod)
                .map_err(|e| format!("Unable to convert POD to value [{}]", e.to_string()))?;
            json_patch::patch(&mut unpatched_pod, &patch)
                .map_err(|e| format!("Unable to patch POD [{}]", e.to_string()))?;
            let patched_pod: Pod = serde_json::from_value(unpatched_pod)
                .map_err(|e| format!("Unable to convert patched POD [{}]", e.to_string()))?;
            Ok(patched_pod)
        };

        let assert_volumes =
            |patched_sdp_pod: &SDPPod, unpatched_volumes: Vec<String>| -> Result<bool, String> {
                let patched_volumes = HashSet::from_iter(
                    patched_sdp_pod
                        .volume_names()
                        .unwrap_or(vec![])
                        .iter()
                        .cloned(),
                );
                (patched_volumes == expected_volumes(&unpatched_volumes))
                    .then(|| true)
                    .ok_or(format!(
                        "Wrong volumes after patch, got {:?}, expected {:?}",
                        patched_volumes,
                        expected_volumes(&unpatched_volumes)
                    ))
            };

        let assert_containers =
            |patched_sdp_pod: &SDPPod, unpatched_containers: Vec<String>| -> Result<bool, String> {
                let patched_containers = HashSet::from_iter(
                    patched_sdp_pod
                        .container_names()
                        .unwrap_or(vec![])
                        .iter()
                        .cloned(),
                );
                (patched_containers == expected_containers(&unpatched_containers))
                    .then(|| true)
                    .ok_or(format!(
                        "Wrong containers after patch, got {:?}, expected {:?}",
                        patched_containers,
                        expected_containers(&unpatched_containers)
                    ))
            };

        let assert_envs =
            |patched_sdp_pod: &SDPPod, test_patch: &TestPatch| -> Result<bool, String> {
                let css = patched_sdp_pod
                    .containers()
                    .ok_or("Containers not found in POD")?;
                let cs = css
                    .iter()
                    .filter(|c| c.name == SDP_SERVICE_CONTAINER_NAME)
                    .collect::<Vec<&Container>>();
                let c = cs.get(0).ok_or("sdp-service container not found in POD")?;
                let env_vars = c
                    .env
                    .as_ref()
                    .ok_or("Environment not found in sdp-service container")?;
                let mut env_errors: Vec<String> = vec![];
                for env_var in env_vars.iter() {
                    if let Some(env_var_src) = env_var.value_from.as_ref() {
                        match (
                            env_var_src.secret_key_ref.as_ref(),
                            env_var_src.config_map_key_ref.as_ref(),
                        ) {
                            (Some(secrets), None) => {
                                if secrets.name.as_ref().is_none()
                                    || !secrets.name.as_ref().unwrap().eq(test_patch.client_secrets)
                                {
                                    let error = format!(
                                        "EnvVar {} got secret {:?}, expected {}",
                                        env_var.name, secrets.name, test_patch.client_secrets
                                    );
                                    env_errors.insert(0, error);
                                }
                            }
                            (None, Some(configmap)) => {
                                if configmap.name.as_ref().is_none()
                                    || !configmap
                                        .name
                                        .as_ref()
                                        .unwrap()
                                        .eq(test_patch.client_config_map)
                                {
                                    let error = format!(
                                        "EnvVar {} got configMap {:?}, expected {}",
                                        env_var.name, configmap.name, test_patch.client_config_map
                                    );
                                    env_errors.insert(0, error);
                                }
                            }
                            (None, None) => {}
                            _ => {
                                env_errors.insert(
                                    0,
                                    format!("EnvVar {} reached unreachable code!", env_var.name),
                                );
                            }
                        }
                    } else {
                        if !test_patch.envs.contains(&(
                            env_var.name.to_string(),
                            Some(env_var.value.as_ref().unwrap().to_string()),
                        )) {
                            env_errors.insert(
                                0,
                                format!(
                                    "EnvVar {} with value {:?} not expected",
                                    env_var.name, env_var.value
                                ),
                            );
                        }
                    }
                }
                if env_errors.len() > 0 {
                    Err(format!(
                        "Found errors on sdp-service container environments: {}",
                        env_errors.join(", ")
                    ))
                } else {
                    Ok(true)
                }
            };

        let assert_dnsconfig =
            |patched_sdp_pod: &SDPPod, test_patch: &TestPatch| -> Result<bool, String> {
                let expected_ns = Some(vec!["127.0.0.1".to_string()]);
                patched_sdp_pod
                    .pod
                    .spec
                    .as_ref()
                    .ok_or_else(|| "Specification not found in patched POD".to_string())
                    .and_then(|spec| {
                        (spec.dns_policy == Some("None".to_string()))
                            .then(|| spec)
                            .ok_or_else(|| {
                                format!(
                                    "POD spec got dnsPolicy {:?}, expected {:?}",
                                    spec.dns_policy,
                                    Some("None")
                                )
                            })
                    })
                    .and_then(|spec| {
                        spec.dns_config
                            .as_ref()
                            .ok_or_else(|| "DNSConfig not found in patched POD".to_string())
                    })
                    .and_then(|dc| {
                        (dc.nameservers == expected_ns).then(|| dc).ok_or_else(|| {
                            format!(
                                "DNSConfig got nameserver {:?}, expected {:?}",
                                dc.nameservers, expected_ns
                            )
                        })
                    })
                    .and_then(|dc| {
                        (dc.searches == test_patch.dns_searches)
                            .then(|| true)
                            .ok_or_else(|| {
                                format!(
                                    "DNSConfig fot searches {:?}, expected {:?}",
                                    dc.searches, test_patch.dns_searches
                                )
                            })
                    })
            };

        let assert_patch =
            |sdp_pod: &SDPPod, patch: Patch, test_patch: &TestPatch| -> Result<bool, String> {
                let patched_pod = patch_pod(sdp_pod, patch)?;
                let patched_sdp_pod = SDPPod {
                    pod: patched_pod,
                    sdp_sidecars: Arc::clone(&sdp_sidecars),
                    k8s_dns_service: Some(test_patch.service.clone()),
                };
                let unpatched_containers = sdp_pod.container_names().unwrap_or(vec![]);
                let unpatched_volumes = sdp_pod.volume_names().unwrap_or(vec![]);
                assert_volumes(&patched_sdp_pod, unpatched_volumes)?;
                assert_containers(&patched_sdp_pod, unpatched_containers)?;
                assert_envs(&patched_sdp_pod, test_patch)?;
                assert_dnsconfig(&patched_sdp_pod, test_patch)
            };

        let test_description = || "Test patch".to_string();
        let identity_storage = Arc::new(InMemoryIdentityStore::default());

        let mut results: Vec<TestResult> = vec![];
        for (n, test) in patch_tests(&sdp_sidecars).iter().enumerate() {
            let pod = Rc::new(test.pod.clone());
            let sdp_pod = SDPPod {
                pod: test.pod.clone(),
                sdp_sidecars: Arc::clone(&sdp_sidecars),
                k8s_dns_service: Some(test.service.clone()),
            };
            let pod_name = Rc::clone(&pod).name();
            identity_storage
                .register_service_identity(service_identity!(n))
                .await;
            identity_storage
                .register_service_device_ids(service_device_ids!(n))
                .await;
            let identity_storage = Arc::clone(&identity_storage);
            let mut env = ServiceEnvironment::create(&pod, identity_storage)
                .await
                .expect(
                    format!("Unable to create ServiceEnvironment from POD {}", &pod_name).as_str(),
                );
            let needs_patching = sdp_pod.is_candidate();
            let patch = sdp_pod.patch(&mut env);
            if test.needs_patching && needs_patching {
                match patch
                    .map_err(|e| e.to_string())
                    .and_then(|p| assert_patch(&sdp_pod, p, test))
                {
                    Ok(res) => results.push((res, test_description(), "".to_string())),
                    Err(error) => results.push((
                        false,
                        test_description(),
                        format!("Error applying patch for pod {}: {}", &pod_name, error),
                    )),
                }
            } else if test.needs_patching != needs_patching {
                results.push((
                    false,
                    test_description(),
                    format!(
                        "Got needs_patching {}, expected {}",
                        needs_patching, test.needs_patching
                    ),
                ))
            } else {
                results.push((true, test_description(), "".to_string()))
            }
        }
        assert_tests(&results)
    }

    #[tokio::test]
    async fn test_mutate_responses() {
        let sdp_sidecars =
            Arc::new(load_sidecar_containers_env().expect("Unable to load sidecars context"));
        let identity_storage = Arc::new(InMemoryIdentityStore::default());

        for (n, _t) in patch_tests(&sdp_sidecars).iter().enumerate() {
            let storage = Arc::clone(&identity_storage);
            storage
                .register_service_identity(service_identity!(n))
                .await;
            storage
                .register_service_device_ids(service_device_ids!(n))
                .await;
        }

        let mut results: Vec<TestResult> = vec![];
        for test in patch_tests(&sdp_sidecars) {
            let sdp_pod = SDPPod {
                pod: test.pod.clone(),
                sdp_sidecars: Arc::clone(&sdp_sidecars),
                k8s_dns_service: Some(test.service.clone()),
            };
            let pod_value = serde_json::to_value(&sdp_pod.pod)
                .expect(&format!("Unable to parse test input {}", sdp_pod));
            let admission_review_value = json!({
                "apiVersion": "admission.k8s.io/v1",
                "kind": "AdmissionReview",
                "request": {
                    "uid": "705ab4f5-6393-11e8-b7cc-42010a800002",
                    "kind" :{"group":"","version":"v1","kind":"Pod"},
                    "resource":{"group":"","version":"v1","resource":"pods"},
                    "requestKind":{"group":"","version": "v1","kind":"Pod"},
                    "requestResource":{"group":"","version":"v1","resource":"pods"},
                    "name": "my-deployment",
                    "namespace": "my-namespace",
                    "operation": "UPDATE",
                    "userInfo": {
                        "username": "admin",
                        "uid": "014fbff9a07c",
                        "groups": ["system:authenticated","my-admin-group"],
                        "extra": {
                            "some-key":["some-value1", "some-value2"]
                        }
                    }
                },
                "object": pod_value});

            let mut admission_review: AdmissionReview<Pod> =
                serde_json::from_value(admission_review_value)
                    .expect("Unable to parse generated AdmissionReview value");
            // copy the POD
            let copied_pod: Pod = serde_json::to_value(&test.pod)
                .and_then(|v| serde_json::from_value(v))
                .expect("Unable to deserialize POD");
            if let Some(request) = admission_review.request.as_mut() {
                request.object = Some(copied_pod);
            }
            // test the patch_request function now
            // TODO: Test responses not allowing the patch!
            let sdp_sidecars = Arc::clone(&sdp_sidecars);
            let patch_response = {
                let sdp_patch_context = SDPPatchContext {
                    sdp_sidecars: Arc::clone(&sdp_sidecars),
                    identity_store: Arc::clone(&identity_storage),
                    k8s_dns_service: None,
                };
                patch_pod(admission_review.request.unwrap(), sdp_patch_context)
                    .await
                    .map_err(|e| format!("Could not generate a response: {}", e.to_string()))
            };

            match patch_response.map(|r| r.response) {
                Ok(Some(response)) if !response.allowed => {
                    results.push((
                        false,
                        "Injection Containers Test".to_string(),
                        format!(
                            "{} got response not allowed,, expected response allowed",
                            sdp_pod
                        ),
                    ));
                }
                Ok(Some(response)) => {
                    if let Some(ps) = response.patch {
                        match (test.needs_patching, ps.len() > 2) {
                            (true, true) | (false, false) => {
                                results.push((
                                    true,
                                    "Injection Containers Test".to_string(),
                                    "Unknown".to_string(),
                                ));
                            }
                            (true, false) => {
                                results.push((false, "Injection Containers Test".to_string(),
                                format!("Pod {} needs patching but not patches were included in the response",
                                        sdp_pod)));
                            }
                            (false, true) => {
                                results.push((false, "Injection Containers Test".to_string(),
                                    format!("Pod {} does not need patching but patches were included in the response: {:?}", 
                                    sdp_pod, ps)));
                            }
                        }
                    } else {
                        results.push((
                            false,
                            "Could not find a response!".to_string(),
                            "Could not find a response!".to_string(),
                        ));
                    }
                }
                Ok(None) => {
                    results.push((
                        false,
                        format!("Could not find a response"),
                        format!("Could not find a response!"),
                    ));
                }
                Err(error) => {
                    results.push((
                        false,
                        format!("Could not generate a response: {}", error),
                        format!("Could not generate a response: {}", error),
                    ));
                }
            }
        }
        assert_tests(&results);
    }

    #[test]
    fn test_pod_validation() {
        let sdp_sidecars =
            Arc::new(load_sidecar_containers_env().expect("Unable to load sidecars context"));
        let test_description = || "Test POD validation".to_string();
        let service = Service::default();
        let results: Vec<TestResult> = validation_tests()
            .iter()
            .map(|t| {
                let sdp_pod = SDPPod {
                    pod: t.pod.clone(),
                    sdp_sidecars: Arc::clone(&sdp_sidecars),
                    k8s_dns_service: Some(service.clone()),
                };
                let pass_validation = sdp_pod.validate();
                match (pass_validation, t.validation_errors.as_ref()) {
                    (Ok(_), Some(expected_error)) => {
                        let error_message =
                            format!("Got no validation error, expected '{}'", expected_error);
                        (false, test_description(), error_message)
                    }
                    (Ok(_), None) => (true, test_description(), "".to_string()),
                    (Err(error), None) => {
                        let error_message =
                            format!("Got validation error '{}', expected none", error);
                        (false, test_description(), error_message)
                    }
                    (Err(error), Some(expected_error)) if !error.eq(expected_error) => {
                        let error_message = format!(
                            "Got validation error '{}', expected '{}'",
                            error, expected_error
                        );
                        (false, test_description(), error_message)
                    }
                    (Err(_), Some(_)) => (true, test_description(), "".to_string()),
                }
            })
            .collect();
        assert_tests(&results)
    }

    #[tokio::test]
    async fn test_service_environment_from_identity_store() {
        let pod = test_pod!(1);
        let store = Arc::new(InMemoryIdentityStore::default());
        let env =
            ServiceEnvironment::from_identity_store(&pod, Arc::clone(&store)).await;
        assert!(env.is_none());
        let id = service_identity!(1);
        let ds = service_device_ids!(1);
        store.register_service_identity(id).await;
        store.register_service_device_ids(ds).await;
        let env = ServiceEnvironment::from_identity_store(&pod, store).await;
        assert!(env.is_some());
        if let Some(env) = env {
            assert_eq!(env.service_name, pod.service_id());
            assert_eq!(env.client_config, "ns1-srv1-service-config");
            assert_eq!(env.client_secret_name, "ns1-srv1-service-user");
            assert_eq!(
                env.client_secret_controller_url_key,
                "service-url".to_string()
            );
            assert_eq!(env.client_secret_pwd_key, "service-password".to_string());
            assert_eq!(env.client_secret_user_key, "service-username".to_string());
            assert_eq!(env.n_containers, "0".to_string());
            assert_eq!(env.k8s_dns_service_ip, None);
        };
    }

    #[test]
    fn test_service_environment_from_pod() {
        let mut pod = test_pod!(0);
        let mut env = ServiceEnvironment::from_pod(&pod);
        assert!(env.is_none());

        pod = test_pod!(0, annotations => vec![
            (SDP_ANNOTATION_CLIENT_SECRETS, "some-secrets"),
            (SDP_ANNOTATION_CLIENT_CONFIG, "some-config-map")
        ]);
        env = ServiceEnvironment::from_pod(&pod);
        assert!(env.is_some());
        if let Some(env) = env {
            assert_eq!(env.service_name, "ns0-srv0".to_string());
            assert_eq!(env.client_config, "some-config-map".to_string());
            assert_eq!(env.client_secret_name, "some-secrets".to_string());
            assert_eq!(
                env.client_secret_controller_url_key,
                "ns0-srv0-password".to_string()
            );
            assert_eq!(env.client_secret_pwd_key, "ns0-srv0-password".to_string());
            assert_eq!(env.client_secret_user_key, "ns0-srv0-user".to_string());
            assert_eq!(env.n_containers, "0".to_string());
            assert_eq!(env.k8s_dns_service_ip, None);
        };

        pod = test_pod!(1, annotations => vec![
            (SDP_ANNOTATION_CLIENT_SECRETS, "some-secrets"),
        ]);
        env = ServiceEnvironment::from_pod(&pod);
        assert!(env.is_some());
        if let Some(env) = env {
            assert_eq!(env.service_name, "ns1-srv1".to_string());
            assert_eq!(env.client_config, "ns1-srv1-service-config");
            assert_eq!(env.client_secret_name, "some-secrets".to_string());
            assert_eq!(
                env.client_secret_controller_url_key,
                "ns1-srv1-password".to_string()
            );
            assert_eq!(env.client_secret_pwd_key, "ns1-srv1-password".to_string());
            assert_eq!(env.client_secret_user_key, "ns1-srv1-user".to_string());
            assert_eq!(env.n_containers, "0".to_string());
            assert_eq!(env.k8s_dns_service_ip, None);
        };

        pod = test_pod!(1, annotations => vec![
            (SDP_ANNOTATION_CLIENT_CONFIG, "some-config-map")
        ]);
        env = ServiceEnvironment::from_pod(&pod);
        assert!(env.is_none());
    }

    #[tokio::test]
    async fn test_identity_manager_identity_storage_async() {
        let ch = channel::<bool>(1);
        let store = Arc::new(InMemoryIdentityStore::default());
        let mut ch_rx = ch.1;
        let ch_tx = ch.0;
        let store1 = Arc::clone(&store);
        let store2 = Arc::clone(&store);
        let t1 = tokio::spawn(async move {
            let id = service_identity!(1);
            let ds = service_device_ids!(1);
            store1.register_service_identity(id).await;
            store1.register_service_device_ids(ds).await;
            ch_tx
                .send(true)
                .await
                .expect("Error notifying client thread in test");
        });
        let pod2 = test_pod!(2);
        let t2 = tokio::spawn(async move {
            let env =
                ServiceEnvironment::from_identity_store(&pod2, Arc::clone(&store2)).await;
            assert!(env.is_none());
            if ch_rx
                .recv()
                .await
                .expect("Error receiving notification from server thread in test")
            {
                let pod1 = test_pod!(1);
                let env = ServiceEnvironment::from_identity_store(&pod1, store2).await;
                assert!(env.is_some());
                if let Some(env) = env {
                    assert_eq!(env.service_name, "ns1-srv1".to_string());
                    assert_eq!(env.client_config, "ns1-srv1-service-config");
                    assert_eq!(env.client_secret_name, "ns1-srv1-service-user");
                    assert_eq!(env.client_secret_controller_url_key, "service-url");
                    assert_eq!(env.client_secret_pwd_key, "service-password");
                    assert_eq!(env.client_secret_user_key, "service-username");
                    assert_eq!(env.n_containers, "0".to_string());
                    assert_eq!(env.k8s_dns_service_ip, None);
                };
            } else {
                assert!(false, "Server thread in test failed!");
            }
        });
        t1.await.unwrap();
        t2.await.unwrap();
    }
}

fn load_ssl() -> Result<ServerConfig, Box<dyn Error>> {
    let (cert_pem, key_pem) = load_cert_files()?;
    let cert: Option<Certificate>;
    let key: Option<PrivateKey>;
    if let Some(Item::X509Certificate(cs)) = cert_pem {
        cert = Some(Certificate(cs));
    } else {
        return Err(Box::new(IOError::new(
            ErrorKind::InvalidData,
            "Unable to load certificate",
        )));
    }
    match key_pem {
        Some(Item::PKCS8Key(key_vec)) | Some(Item::RSAKey(key_vec)) => {
            key = Some(PrivateKey(key_vec));
            Ok(ServerConfig::builder()
                .with_safe_defaults()
                .with_no_client_auth()
                .with_single_cert(vec![cert.unwrap()], key.unwrap())?)
        }
        _ => Err(Box::new(IOError::new(
            ErrorKind::InvalidData,
            "Unable to load private key",
        ))),
    }
}

async fn injector_handler<E: IdentityStore>(
    req: Request<Body>,
    sdp_context: Arc<SDPInjectorContext<E>>,
) -> Result<Response<Body>, hyper::Error> {
    match (req.method(), req.uri().path()) {
        (&Method::POST, "/mutate") => {
            let bs = hyper::body::to_bytes(req).await?;
            match mutate(bs, sdp_context).await {
                Ok(body) => Ok(Response::new(body)),
                Err(e) => {
                    error!(
                        "Error processing /mutate admission request: {}",
                        e.to_string()
                    );
                    Ok(Response::builder()
                        .status(StatusCode::UNPROCESSABLE_ENTITY)
                        .body(Body::from(e.to_string()))
                        .unwrap())
                }
            }
        }
        (&Method::POST, "/validate") => {
            let bs = hyper::body::to_bytes(req).await?;
            match validate(bs, Arc::clone(&sdp_context.sdp_sidecars)).await {
                Ok(body) => Ok(Response::new(body)),
                Err(e) => Ok(Response::builder()
                    .status(StatusCode::UNPROCESSABLE_ENTITY)
                    .body(Body::from(e.to_string()))
                    .unwrap()),
            }
        }
        (&Method::GET, "/dns-service") => match get_dns_service(&sdp_context.k8s_client).await {
            Ok(Some(body)) => Ok(Response::new(body)),
            Ok(None) => Ok(Response::builder()
                .status(StatusCode::NOT_FOUND)
                .body(Body::empty())
                .unwrap()),
            Err(e) => Ok(Response::builder()
                .status(StatusCode::UNPROCESSABLE_ENTITY)
                .body(Body::from(e.to_string()))
                .unwrap()),
        },
        _ => Ok(Response::builder()
            .status(StatusCode::NOT_FOUND)
            .body(Body::empty())
            .unwrap()),
    }
}

pub type Acceptor = tokio_rustls::TlsAcceptor;

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    env_logger::init();
    info!("Starting sdp-injector!!!!");

    let mut k8s_host = String::from("https://");
    k8s_host.push_str(&std::env::var(SDP_K8S_HOST_ENV).unwrap_or(SDP_K8S_HOST_DEFAULT.to_string()));
    let k8s_uri = k8s_host
        .parse::<Uri>()
        .expect("Unable to parse SDP_K8S_HOST value:");
    let mut k8s_config = Config::infer()
        .await
        .expect("Unable to infer K8S configuration");
    k8s_config.cluster_url = k8s_uri;
    k8s_config.accept_invalid_certs = std::env::var(SDP_K8S_NO_VERIFY_ENV)
        .map(|v| v == "1" || v.to_lowercase() == "true")
        .unwrap_or(false);
    let k8s_client: Client = Client::try_from(k8s_config).expect("Unable to create k8s client");
    let k8s_client_cp = k8s_client.clone();
    let k8s_client_cp2 = k8s_client.clone();
    let service_identity_api: Api<ServiceIdentity> = Api::namespaced(k8s_client, "sdp-system");
    let service_device_ids_api: Api<DeviceId> = Api::namespaced(k8s_client_cp, "sdp-system");
    let store = KubeIdentityStore {
        service_identity_api,
        service_device_ids_api,
    };
    let sdp_sidecars: SDPSidecars =
        load_sidecar_containers().expect("Unable to load the sidecar information");
    let sdp_injector_context = Arc::new(SDPInjectorContext {
        sdp_sidecars: Arc::new(sdp_sidecars),
        k8s_client: k8s_client_cp2,
        identity_store: Arc::new(store),
    });

    let ssl_config = load_ssl()?;
    let addr = ([0, 0, 0, 0], 8443).into();
    let tls_acceptor: Acceptor = Arc::new(ssl_config).into();
    let make_service = {
        make_service_fn(move |_conn| {
            let sdp_injector_context = sdp_injector_context.clone();
            async move {
                let sdp_injector_context = sdp_injector_context.clone();
                Ok::<_, Infallible>(service_fn(move |req| {
                    injector_handler(req, sdp_injector_context.clone())
                }))
            }
        })
    };
    let incoming = TlsListener::new(tls_acceptor, AddrIncoming::bind(&addr)?).filter(|c| {
        if let Err(e) = c {
            error!("Error running injector server: {:?}", e);
            ready(false)
        } else {
            ready(true)
        }
    });
    let server = Server::builder(accept::from_stream(incoming)).serve(make_service);
    server.await?;
    Ok(())
}
