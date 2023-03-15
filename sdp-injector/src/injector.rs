use crate::deviceid::{DeviceIdProviderResponseProtocol, IdentityStore, RegisteredDeviceId};
use crate::errors::SDPPatchError;
use futures_util::Future;
use http::{Method, StatusCode};
use hyper::body::Bytes;
use hyper::{Body, Request, Response};
use json_patch::PatchOperation::Add;
use json_patch::{AddOperation, Patch};
use k8s_openapi::api::apps::v1::Deployment;
use k8s_openapi::api::core::v1::{
    ConfigMapKeySelector, Container, EnvVar, EnvVarSource, Namespace, ObjectFieldSelector, Pod,
    PodDNSConfig, PodSecurityContext, SecretKeySelector, Service as KubeService, Sysctl, Volume,
};
use k8s_openapi::apimachinery::pkg::apis::meta::v1::Status;
use kube::api::{DynamicObject, ListParams};
use kube::core::admission::{AdmissionRequest, AdmissionResponse, AdmissionReview};
use kube::Api;
use rustls::{Certificate, PrivateKey, ServerConfig};
use rustls_pemfile::{certs, rsa_private_keys};
use sdp_common::annotations::{
    SDP_ANNOTATION_CLIENT_CONFIG, SDP_ANNOTATION_CLIENT_DEVICE_ID, SDP_ANNOTATION_CLIENT_SECRETS,
    SDP_ANNOTATION_DNS_SEARCHES, SDP_INJECTOR_ANNOTATION_CLIENT_VERSION,
    SDP_INJECTOR_ANNOTATION_DISABLE_INIT_CONTAINERS, SDP_INJECTOR_ANNOTATION_ENABLED,
    SDP_INJECTOR_ANNOTATION_STRATEGY,
};
use sdp_common::constants::{MAX_PATCH_ATTEMPTS, SDP_DEFAULT_CLIENT_VERSION_ENV};
use sdp_common::crd::DeviceId;
use sdp_common::errors::SDPServiceError;
use sdp_common::patch_annotation;
use sdp_common::traits::{
    Annotated, Candidate, HasCredentials, MaybeNamespaced, MaybeService, ObjectRequest, Validated,
};
use serde::Deserialize;
use std::collections::hash_map::RandomState;
use std::collections::{HashMap, HashSet};
use std::error::Error;
use std::fs::File;
use std::io::BufReader;
use std::iter::FromIterator;
use std::path::PathBuf;
use std::pin::Pin;
use std::str::from_utf8;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc::{channel, Sender};
use tokio::sync::{Mutex, MutexGuard};
use tokio::time::timeout;
use uuid::Uuid;

use crate::deviceid::DeviceIdProviderRequestProtocol;
use sdp_common::service::{
    containers, init_containers, injection_strategy, security_context, volume_names, volumes,
    SDPInjectionStrategy, ServiceIdentity,
};
use sdp_macros::{logger, sdp_debug, sdp_error, sdp_info, sdp_log, sdp_warn, with_dollar_sign};

logger!("SDPInjector");

const SDP_DNS_SERVICE_NAMES: [&str; 2] = ["kube-dns", "coredns"];
const SDP_SIDECARS_FILE: &str = "/opt/sdp-injector/k8s/sdp-sidecars.json";
const SDP_SIDECARS_FILE_ENV: &str = "SDP_SIDECARS_FILE";
const SDP_CERT_FILE_ENV: &str = "SDP_CERT_FILE";
const SDP_KEY_FILE_ENV: &str = "SDP_KEY_FILE";
const SDP_CERT_FILE: &str = "/opt/sdp-injector/k8s/sdp-injector.crt";
const SDP_KEY_FILE: &str = "/opt/sdp-injector/k8s/sdp-injector.key";
const SDP_SERVICE_CONTAINER_NAME: &str = "sdp-service";

const K8S_VERSION_FOR_SAFE_SYSCTL: u32 = 22;

macro_rules! admission_request {
    ($body:ident, $typ:tt) => {{
        let admission_review: AdmissionReview<$typ> =
            serde_json::from_str(&$body).map_err(SDPServiceError::from_error(&format!(
                "Unable to parse AdmissionReview<{}>",
                stringify!($typ)
            )))?;
        let admission_request: AdmissionRequest<$typ> =
            admission_review
                .try_into()
                .map_err(SDPServiceError::from_error(&format!(
                    "Unable to parse AdmissionReview<{}>",
                    stringify!($typ)
                )))?;
        admission_request
    }};
}

macro_rules! attempt_patch {
    ($admission_request:ident, $patcher:expr, $store:ident, $prefix:literal) => {{
        let admission_id = format!("{}_{}", $prefix, $admission_request.service_id()?);
        if $admission_request.is_candidate() {
            let patched = $patcher.await;
            match patched {
                Ok(SDPPatchResponse::Allow(admission_response)) => {
                    $store.remove(&admission_id);
                    Ok(SDPPatchResponse::Allow(admission_response))
                }
                Ok(r) => Ok(r),
                Err(SDPPatchError::WithResponse(response, e)) => {
                    *$store.entry(admission_id.clone()).or_insert(0) += 1;
                    let attempt = $store.get(&admission_id).unwrap_or(&0);
                    if attempt > &MAX_PATCH_ATTEMPTS {
                        $store.remove(&admission_id);
                        Ok(SDPPatchResponse::MaxRetryExceeded(response, e))
                    } else {
                        Ok(SDPPatchResponse::RetryWithError(response, e, *attempt))
                    }
                }
                Err(e) => Err(e),
            }
        } else {
            Ok(SDPPatchResponse::Allow(Box::new(AdmissionResponse::from(
                &$admission_request,
            ))))
        }
    }};
}

macro_rules! admission_response {
    ($body:ident, $response:ident => $expr:expr) => {{
        let admission_review = $response.into_review();
        let body = serde_json::to_string(&admission_review);
        let r: Result<Response<Body>, hyper::Error> = match body {
            Err(e) => Ok(Response::builder()
                .status(StatusCode::UNPROCESSABLE_ENTITY)
                .body(Body::from(e.to_string()))
                .unwrap()),
            Ok($body) => Ok($expr),
        };
        r
    }};
}

macro_rules! allow_admission_response {
    (response => $response:ident) => {{
        let mut status: Status = Default::default();
        status.code = Some(200);
        $response.allowed = true;
        $response.result = status;
        admission_response!(body, $response => {
            Response::new(Body::from(body))
        })
    }};
}

macro_rules! fail_admission_response {
    (response => $response:ident) => {{
        admission_response!(body, $response => {
            Response::builder()
                .status(StatusCode::UNPROCESSABLE_ENTITY)
                .body(Body::from(body))
                .unwrap()
        })
    }};
    (error => $e:ident) => {{
        let response = AdmissionResponse::invalid($e.to_string());
        fail_admission_response!(response => response)
    }};
}

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

pub enum SDPPatchResponse {
    RetryWithError(Box<AdmissionResponse>, SDPServiceError, u8),
    MaxRetryExceeded(Box<AdmissionResponse>, SDPServiceError),
    Allow(Box<AdmissionResponse>),
}

pub struct KubeIdentityStore {
    pub device_id_q_tx: Sender<DeviceIdProviderRequestProtocol<ServiceIdentity>>,
}

impl IdentityStore<ServiceIdentity> for KubeIdentityStore {
    fn pop_device_id<'a>(
        &'a mut self,
        service_id: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<(ServiceIdentity, Uuid), SDPServiceError>> + Send + '_>>
    {
        let fut = async move {
            let (q_tx, mut q_rx) = channel::<DeviceIdProviderResponseProtocol<ServiceIdentity>>(1);
            self.device_id_q_tx
                .send(DeviceIdProviderRequestProtocol::RequestDeviceId(
                    q_tx,
                    service_id.to_string(),
                ))
                .await
                .map_err(|e| format!("Error sending message to request device id: {}", e))?;
            match timeout(Duration::from_secs(5), q_rx.recv()).await {
                Ok(Some(DeviceIdProviderResponseProtocol::AssignedDeviceId(
                    service_identity,
                    device_id,
                ))) => Ok((service_identity, device_id)),
                Ok(Some(DeviceIdProviderResponseProtocol::NotFound)) | Ok(None) => {
                    Err(SDPServiceError::from_string(format!(
                        "Device id not found for service {}",
                        service_id
                    )))
                }
                Err(e) => Err(SDPServiceError::from_string(format!(
                    "Error getting device id for service {}: {}",
                    service_id, e
                ))),
            }
        };
        Box::pin(fut)
    }

    fn register_service(
        &mut self,
        _service: ServiceIdentity,
    ) -> Pin<Box<dyn Future<Output = Result<Option<ServiceIdentity>, SDPServiceError>> + Send + '_>>
    {
        Box::pin(async move {
            Err(SDPServiceError::from(
                "Registry does not support register of service identities",
            ))
        })
    }

    fn register_device_ids(
        &mut self,
        _device_id: DeviceId,
    ) -> Pin<
        Box<dyn Future<Output = Result<Option<RegisteredDeviceId>, SDPServiceError>> + Send + '_>,
    > {
        Box::pin(async move {
            Err(SDPServiceError::from(
                "Registry does not support register of device ids",
            ))
        })
    }

    fn unregister_device_ids<'a>(
        &'a mut self,
        _device_id: &'a str,
    ) -> Pin<
        Box<
            dyn Future<
                    Output = Result<Option<(ServiceIdentity, RegisteredDeviceId)>, SDPServiceError>,
                > + Send
                + '_,
        >,
    > {
        Box::pin(async move {
            Err(SDPServiceError::from(
                "Registry does not support unregister if device ids",
            ))
        })
    }

    fn unregister_service<'a>(
        &'a mut self,
        _service_id: &'a str,
    ) -> Pin<
        Box<
            dyn Future<
                    Output = Result<Option<(ServiceIdentity, RegisteredDeviceId)>, SDPServiceError>,
                > + Send
                + '_,
        >,
    > {
        Box::pin(async move {
            Err(SDPServiceError::from(
                "Registry does not support deregistration of service identities",
            ))
        })
    }

    fn push_device_id<'a>(
        &'a mut self,
        _service_id: &'a str,
        _uuid: Uuid,
    ) -> Pin<Box<dyn Future<Output = Result<Option<Uuid>, SDPServiceError>> + Send + '_>> {
        Box::pin(async move {
            Err(SDPServiceError::from(
                "Registry does not support registration of device ids",
            ))
        })
    }
}

async fn dns_service_discover(services_api: &Api<KubeService>) -> Option<KubeService> {
    let names: HashSet<&str, RandomState> = HashSet::from_iter(SDP_DNS_SERVICE_NAMES);
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
            warn!("Unable to discover K8S DNS service {}", e);
            Option::None
        })
}

trait K8SDNSService {
    fn maybe_ip(&self) -> Option<&String>;
}

impl K8SDNSService for Option<KubeService> {
    fn maybe_ip(&self) -> Option<&String> {
        self.as_ref()
            .and_then(|s| s.spec.as_ref())
            .and_then(|s| s.cluster_ip.as_ref())
    }
}
trait Patched {
    fn patch<R: ObjectRequest<Pod> + MaybeNamespaced>(
        &self,
        environment: &mut ServiceEnvironment,
        request: R,
    ) -> Result<Patch, SDPServiceError>;
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
    async fn create<'a, E, R>(
        request: &R,
        store: MutexGuard<'a, E>,
    ) -> Result<Self, SDPServiceError>
    where
        E: IdentityStore<ServiceIdentity>,
        R: ObjectRequest<Pod> + MaybeService + Annotated,
    {
        if let Some(e) = ServiceEnvironment::from_pod(request)? {
            Ok(e)
        } else {
            ServiceEnvironment::from_identity_store(request, store).await
        }
    }

    async fn from_identity_store<
        'a,
        E: IdentityStore<ServiceIdentity>,
        R: ObjectRequest<Pod> + MaybeService + Annotated,
    >(
        request: &R,
        mut store: MutexGuard<'a, E>,
    ) -> Result<Self, SDPServiceError> {
        let service_id = request.service_id()?;
        store.pop_device_id(&service_id).await.map(|(s, d)| {
            let (user_field, password_field, profile_field) =
                s.spec.service_user.secrets_field_names(true);
            let secrets_name = s
                .credentials()
                .secrets_name(&s.spec.service_namespace, &s.spec.service_name);
            let config_name = s
                .credentials()
                .config_name(&s.spec.service_namespace, &s.spec.service_name);
            ServiceEnvironment {
                service_name: service_id,
                client_config: config_name,
                client_secret_name: secrets_name,
                client_secret_controller_url_key: profile_field,
                client_secret_pwd_key: password_field,
                client_secret_user_key: user_field,
                client_device_id: d.to_string(),
                n_containers: "0".to_string(),
                k8s_dns_service_ip: None,
            }
        })
    }

    fn from_pod<R: ObjectRequest<Pod> + MaybeService + Annotated>(
        request: &R,
    ) -> Result<Option<Self>, SDPServiceError> {
        info!("Building service environment from pod");
        let pod = request.object().ok_or("Could not get pod in request")?;
        // Need all three annotation to build service environment from pod
        if let (Some(config), Some(secret), Some(device_id)) = (
            pod.annotation(SDP_ANNOTATION_CLIENT_CONFIG),
            pod.annotation(SDP_ANNOTATION_CLIENT_SECRETS),
            pod.annotation(SDP_ANNOTATION_CLIENT_DEVICE_ID),
        ) {
            let service_id = request.service_id()?;
            Ok(Some(ServiceEnvironment {
                service_name: service_id,
                client_config: config.to_string(),
                client_secret_name: secret.to_string(),
                client_secret_controller_url_key: "service-url".to_string(),
                client_secret_user_key: "service-username".to_string(),
                client_secret_pwd_key: "service-password".to_string(),
                client_device_id: device_id.to_string(),
                n_containers: "0".to_string(),
                k8s_dns_service_ip: None,
            }))
        } else {
            warn!("Missing annotations to infer service environment from pod");
            Ok(None)
        }
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
                    configMap :: "APPGATE_LOGLEVEL" => self.client_config),
                env_var!(
                    value :: "APPGATE_DEVICE_ID" => self.client_device_id.clone()),
                env_var!(
                    secrets :: "APPGATE_PROFILE_URL" => (self.client_secret_name, self.client_secret_controller_url_key)),
                env_var!(
                    secrets :: "APPGATE_USERNAME" => (self.client_secret_name, self.client_secret_user_key)),
                env_var!(
                    secrets :: "APPGATE_PASSWORD" => (self.client_secret_name, self.client_secret_pwd_key)),
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
    sdp_sidecars: Arc<SDPSidecars>,
    k8s_dns_service: Option<KubeService>,
    k8s_server_version: u32,
}

impl SDPPod {
    fn sidecars<'a>(&self, request: &'a dyn ObjectRequest<Pod>) -> Option<Vec<&'a Container>> {
        request.object().and_then(|pod| {
            containers(pod).map(|cs| {
                cs.iter()
                    .filter(|&c| self.sdp_sidecars.container_names().contains(&c.name))
                    .collect()
            })
        })
    }

    fn sidecar_names(&self, request: &dyn ObjectRequest<Pod>) -> Option<Vec<String>> {
        self.sidecars(request)
            .map(|xs| xs.iter().map(|&x| x.name.clone()).collect())
    }

    fn has_any_sidecars(&self, request: &dyn ObjectRequest<Pod>) -> bool {
        self.sidecars(request)
            .map(|xs| xs.len() != 0)
            .unwrap_or(false)
    }

    fn needs_patching(&self, request: &dyn ObjectRequest<Pod>) -> bool {
        request
            .object()
            .map(|pod| containers(pod).unwrap_or(&vec![]).len() > 0)
            .unwrap_or(false)
            && !self.has_any_sidecars(request)
    }
}

impl Patched for SDPPod {
    fn patch<R: ObjectRequest<Pod> + MaybeNamespaced>(
        &self,
        environment: &mut ServiceEnvironment,
        request: R,
    ) -> Result<Patch, SDPServiceError> {
        let pod = request
            .object()
            .ok_or("Unable to get Pod object in admission request")?;
        let ns = request
            .namespace()
            .ok_or("Unable to get namespace from Pod admission request")?;
        let n_containers = containers(pod).map(|xs| xs.len()).unwrap_or(0);
        environment.n_containers = format!("{}", n_containers);

        // Fill DNS service ip
        environment.k8s_dns_service_ip = self.k8s_dns_service.maybe_ip().map(|s| s.clone());

        let mut patches = vec![];

        if self.needs_patching(&request) {
            let k8s_dns_ip = self
                .k8s_dns_service
                .maybe_ip()
                .ok_or("Unable to get K8S DNS service IP address")?;
            let custom_searches = pod
                .annotation(SDP_ANNOTATION_DNS_SEARCHES)
                .map(|s| searches_string_to_vec(s.to_string()));
            let dns_config = &self.sdp_sidecars.dns_config(&ns, custom_searches);
            if pod.annotations().is_none() || pod.annotations().unwrap().is_empty() {
                patches.push(Add(AddOperation {
                    path: "/metadata/annotations".to_string(),
                    value: serde_json::to_value(HashMap::<String, String>::new())?,
                }));
            }
            let injection_strategy = injection_strategy(pod);
            let injection_enabled = pod
                .annotation(SDP_INJECTOR_ANNOTATION_ENABLED)
                .map(|s| s == "true")
                .unwrap_or(injection_strategy == SDPInjectionStrategy::EnabledByDefault)
                .to_string();
            patches.push(Add(AddOperation {
                path: format!(
                    "/metadata/annotations/{}",
                    patch_annotation!(SDP_INJECTOR_ANNOTATION_STRATEGY)
                ),
                value: serde_json::to_value(injection_strategy.to_string())?,
            }));
            patches.push(Add(AddOperation {
                path: format!(
                    "/metadata/annotations/{}",
                    patch_annotation!(SDP_INJECTOR_ANNOTATION_ENABLED)
                ),
                value: serde_json::to_value(
                    pod.annotation(SDP_INJECTOR_ANNOTATION_ENABLED)
                        .unwrap_or(&injection_enabled),
                )?,
            }));
            patches.push(Add(AddOperation {
                path: format!(
                    "/metadata/annotations/{}",
                    patch_annotation!(SDP_ANNOTATION_CLIENT_DEVICE_ID)
                ),
                value: serde_json::to_value(&environment.client_device_id)?,
            }));

            if std::env::var(SDP_DEFAULT_CLIENT_VERSION_ENV).is_err() {
                panic!("Unable to get default client version environment variable");
            }
            let default_version = std::env::var(SDP_DEFAULT_CLIENT_VERSION_ENV).unwrap();
            let version = pod
                .annotation(SDP_INJECTOR_ANNOTATION_CLIENT_VERSION)
                .unwrap_or(&default_version);
            for c in self.sdp_sidecars.containers.clone().iter_mut() {
                if let Some(image) = &c.image {
                    let tag = image.rsplit(":").collect::<Vec<&str>>()[0];
                    c.image = Some(format!(
                        "{}:{}",
                        image.trim_end_matches(&format!(":{}", tag)),
                        version
                    ));
                }
                c.env = Some(environment.variables(&c.name));
                patches.push(Add(AddOperation {
                    path: "/spec/containers/-".to_string(),
                    value: serde_json::to_value(&c)?,
                }));
            }
            if let Some((xs, false)) = init_containers(pod).map(|cs| {
                let disable_init_containers = pod
                    .annotation(SDP_INJECTOR_ANNOTATION_DISABLE_INIT_CONTAINERS)
                    .map(|s| s.eq_ignore_ascii_case("true"))
                    .unwrap_or(false);
                (cs, disable_init_containers)
            }) {
                let mut init_containers = Vec::new();
                init_containers.extend(xs.iter().map(|c| c.clone()));
                let mut c0 = self.sdp_sidecars.init_containers.0.clone();
                c0.env = Some(vec![
                    env_var!(value :: "K8S_DNS_SERVICE" => k8s_dns_ip.clone()),
                    env_var!(value :: "K8S_DNS_SEARCHES" => dns_config.searches.as_ref().unwrap_or(&vec!()).join(" ")),
                ]);
                let mut c1 = self.sdp_sidecars.init_containers.1.clone();
                c1.env = Some(vec![
                    env_var!(value :: "K8S_DNS_SERVICE" => k8s_dns_ip.clone()),
                    env_var!(value :: "K8S_DNS_SEARCHES" => dns_config.searches.as_ref().unwrap_or(&vec!()).join(" ")),
                ]);
                init_containers.insert(0, c0);
                init_containers.push(c1);
                patches.push(Add(AddOperation {
                    path: "/spec/initContainers".to_string(),
                    value: serde_json::to_value(&init_containers)?,
                }));
            }
            if volumes(pod).is_some() {
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
                value: serde_json::to_value(&dns_config)?,
            }));
            patches.push(Add(AddOperation {
                path: "/spec/dnsPolicy".to_string(),
                value: serde_json::to_value("None".to_string())?,
            }));

            // Patch sysctl securityContext
            if self.k8s_server_version >= K8S_VERSION_FOR_SAFE_SYSCTL {
                let mut sc = security_context(pod)
                    .map(Clone::clone)
                    .unwrap_or(PodSecurityContext::default());
                let mut extra_sysctls = vec![Sysctl {
                    name: "net.ipv4.ip_unprivileged_port_start".to_string(),
                    value: "0".to_string(),
                }];
                if let Some(sysctls) = sc.sysctls {
                    extra_sysctls.extend(sysctls)
                }
                sc.sysctls = Some(extra_sysctls);
                patches.push(Add(AddOperation {
                    path: "/spec/securityContext".to_string(),
                    value: serde_json::to_value(sc)?,
                }));
            }
        }
        debug!("Pod patches: {:?}", patches);
        Ok(Patch(patches))
    }
}

impl Validated for SDPPod {
    fn validate<R: ObjectRequest<Pod>>(&self, request: R) -> Result<(), String> {
        let pod = request
            .object()
            .ok_or_else(|| "Unable to get Pod from admission request")?;
        let expected_volume_names =
            HashSet::from_iter(self.sdp_sidecars.volumes.iter().map(|v| v.name.clone()));
        let expected_container_names =
            HashSet::from_iter(self.sdp_sidecars.containers.iter().map(|c| c.name.clone()));
        let container_names = HashSet::from_iter(
            self.sidecar_names(&request)
                .unwrap_or(vec![])
                .iter()
                .cloned(),
        );
        let volume_names = HashSet::from_iter(volume_names(pod).unwrap_or(vec![]).iter().cloned());
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
                "Pod is missing required containers: {}",
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
                "Pod is missing required volumes: {}",
                missing_volumes.join(", ")
            ));
        }
        if errors.is_empty() {
            Ok(())
        } else {
            Err(format!("Unable to run SDP client on Pod: {}", errors.join("\n")).to_string())
        }
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
pub struct SDPSidecars {
    #[serde(rename = "initContainers")]
    init_containers: Box<(Container, Container)>,
    containers: Box<Vec<Container>>,
    volumes: Box<Vec<Volume>>,
    #[serde(rename = "dnsConfig")]
    dns_config: Box<DNSConfig>,
}

pub struct SDPInjectorContext<E: IdentityStore<ServiceIdentity>> {
    pub(crate) sdp_sidecars: Arc<SDPSidecars>,
    pub(crate) ns_api: Api<Namespace>,
    pub(crate) services_api: Api<KubeService>,
    pub(crate) identity_store: Mutex<E>,
    pub(crate) attempts_store: Mutex<HashMap<String, u8>>,
    pub(crate) server_version: u32,
}

#[derive(Debug)]
struct SDPPatchContext<'a, E: IdentityStore<ServiceIdentity>> {
    sdp_sidecars: Arc<SDPSidecars>,
    identity_store: MutexGuard<'a, E>,
    k8s_dns_service: Option<KubeService>,
    k8s_server_version: u32,
}

fn searches_string_to_vec(searches: String) -> Vec<String> {
    searches.split(" ").map(|s| s.trim().to_string()).collect()
}

impl SDPSidecars {
    fn container_names(&self) -> Vec<String> {
        self.containers.iter().map(|c| c.name.clone()).collect()
    }

    fn dns_config(&self, namespace: &str, searches: Option<Vec<String>>) -> PodDNSConfig {
        let mut searches =
            searches.unwrap_or_else(|| searches_string_to_vec(self.dns_config.searches.clone()));
        if let Some(first_search) = searches.get(0) {
            let first_search_cp = first_search.to_string();
            let mut ss = first_search_cp.split(".").map(|s| s.to_string());
            let s = ss.next();
            let rest = ss.map(|s| s.to_string()).collect::<Vec<String>>().join(".");
            if let Some(prefix) = s {
                if !searches.contains(&rest) {
                    searches.insert(1, rest.clone());
                } else if !prefix.eq_ignore_ascii_case(namespace) {
                    searches.insert(0, format!("{}.{}", namespace, searches[0]));
                }
            }
        }
        PodDNSConfig {
            nameservers: Some(vec!["127.0.0.1".to_string()]),
            options: None,
            searches: Some(searches),
        }
    }
}

pub fn load_sidecar_containers() -> Result<SDPSidecars, Box<dyn Error>> {
    let cwd = std::env::current_dir()?;
    let mut sidecars_path = cwd.join(PathBuf::from(SDP_SIDECARS_FILE).as_path());
    if let Ok(sidecars_file) = std::env::var(SDP_SIDECARS_FILE_ENV) {
        sidecars_path = PathBuf::from(sidecars_file);
    }
    let file = File::open(sidecars_path)?;
    let reader = BufReader::new(file);
    let sdp_sidecars = serde_json::from_reader(reader)?;
    debug!("SDP sidecar: {:?}", sdp_sidecars);
    Ok(sdp_sidecars)
}

async fn patch_deployment(
    admission_request: AdmissionRequest<Deployment>,
    ns: Option<Namespace>,
) -> Result<SDPPatchResponse, SDPPatchError> {
    let admission_response = Box::new(AdmissionResponse::from(&admission_request));
    let mut patches = vec![];
    let deployment = admission_request
        .object()
        .ok_or(SDPServiceError::from(
            "Unable to get Deployment from admission request",
        ))
        .map_err(SDPPatchError::from_admission_response(Box::clone(
            &admission_response,
        )))?;

    // Patch Deployment metadata
    if deployment.annotations().is_none() || deployment.annotations().unwrap().is_empty() {
        patches.push(Add(AddOperation {
            path: "/metadata/annotations".to_string(),
            value: serde_json::to_value(HashMap::<String, String>::new())
                .map_err(|e| SDPServiceError::from(e))
                .map_err(SDPPatchError::from_admission_response(Box::clone(
                    &admission_response,
                )))?,
        }));
    }
    let injection_strategy = ns
        .as_ref()
        .map(|ns| injection_strategy(ns))
        .unwrap_or(SDPInjectionStrategy::EnabledByDefault);
    let injection_enabled = deployment
        .annotation(SDP_INJECTOR_ANNOTATION_ENABLED)
        .or_else(|| {
            ns.as_ref()
                .and_then(|ns| ns.annotation(SDP_INJECTOR_ANNOTATION_ENABLED))
        })
        .map(|s| s == "true")
        .unwrap_or(injection_strategy == SDPInjectionStrategy::EnabledByDefault)
        .to_string();
    let disable_init_containers = deployment
        .annotation(SDP_INJECTOR_ANNOTATION_DISABLE_INIT_CONTAINERS)
        .or_else(|| {
            ns.as_ref()
                .and_then(|ns| ns.annotation(SDP_INJECTOR_ANNOTATION_DISABLE_INIT_CONTAINERS))
        })
        .map(|s| s.eq_ignore_ascii_case("true"))
        .unwrap_or(false)
        .to_string();

    patches.push(Add(AddOperation {
        path: format!(
            "/metadata/annotations/{}",
            patch_annotation!(SDP_INJECTOR_ANNOTATION_STRATEGY)
        ),
        value: serde_json::to_value(injection_strategy.to_string())
            .map_err(|e| SDPServiceError::from(e))
            .map_err(SDPPatchError::from_admission_response(Box::clone(
                &admission_response,
            )))?,
    }));
    patches.push(Add(AddOperation {
        path: format!(
            "/metadata/annotations/{}",
            patch_annotation!(SDP_INJECTOR_ANNOTATION_ENABLED)
        ),
        value: serde_json::to_value(&injection_enabled)
            .map_err(|e| SDPServiceError::from(e))
            .map_err(SDPPatchError::from_admission_response(Box::clone(
                &admission_response,
            )))?,
    }));

    // Patch Deployment template metadata
    if deployment
        .spec
        .as_ref()
        .and_then(|s| {
            s.template
                .metadata
                .as_ref()
                .and_then(|m| m.annotations.as_ref())
        })
        .is_none()
    {
        patches.push(Add(AddOperation {
            path: "/spec/template/metadata/annotations".to_string(),
            value: serde_json::to_value(HashMap::<String, String>::new())
                .map_err(|e| SDPServiceError::from(e))
                .map_err(SDPPatchError::from_admission_response(Box::clone(
                    &admission_response,
                )))?,
        }));
    }
    patches.push(Add(AddOperation {
        path: format!(
            "/spec/template/metadata/annotations/{}",
            patch_annotation!(SDP_INJECTOR_ANNOTATION_STRATEGY)
        ),
        value: serde_json::to_value(injection_strategy.to_string())
            .map_err(|e| SDPServiceError::from(e))
            .map_err(SDPPatchError::from_admission_response(Box::clone(
                &admission_response,
            )))?,
    }));
    patches.push(Add(AddOperation {
        path: format!(
            "/spec/template/metadata/annotations/{}",
            patch_annotation!(SDP_INJECTOR_ANNOTATION_ENABLED)
        ),
        value: serde_json::to_value(injection_enabled)
            .map_err(|e| SDPServiceError::from(e))
            .map_err(SDPPatchError::from_admission_response(Box::clone(
                &admission_response,
            )))?,
    }));
    patches.push(Add(AddOperation {
        path: format!(
            "/spec/template/metadata/annotations/{}",
            patch_annotation!(SDP_INJECTOR_ANNOTATION_DISABLE_INIT_CONTAINERS)
        ),
        value: serde_json::to_value(disable_init_containers)
            .map_err(|e| SDPServiceError::from(e))
            .map_err(SDPPatchError::from_admission_response(Box::clone(
                &admission_response,
            )))?,
    }));
    let client_version = deployment
        .annotation(SDP_INJECTOR_ANNOTATION_CLIENT_VERSION)
        .or_else(|| {
            ns.as_ref()
                .and_then(|ns| ns.annotation(SDP_INJECTOR_ANNOTATION_CLIENT_VERSION))
        });
    if let Some(client_version) = client_version {
        patches.push(Add(AddOperation {
            path: format!(
                "/spec/template/metadata/annotations/{}",
                patch_annotation!(SDP_INJECTOR_ANNOTATION_CLIENT_VERSION)
            ),
            value: serde_json::to_value(client_version)
                .map_err(|e| SDPServiceError::from(e))
                .map_err(SDPPatchError::from_admission_response(Box::clone(
                    &admission_response,
                )))?,
        }));
    }
    debug!("Deployment patches: {:?}", patches);
    let admission_response = Box::clone(&admission_response)
        .with_patch(Patch(patches))
        .map_err(SDPServiceError::from_error("Unable to patch deployment"))
        .map_err(SDPPatchError::from_admission_response(Box::clone(
            &admission_response,
        )))?;
    Ok(SDPPatchResponse::Allow(Box::new(admission_response)))
}

async fn patch_pod<'a, E: IdentityStore<ServiceIdentity>>(
    admission_request: AdmissionRequest<Pod>,
    sdp_patch_context: SDPPatchContext<'a, E>,
) -> Result<SDPPatchResponse, SDPPatchError> {
    let sdp_pod = SDPPod {
        sdp_sidecars: Arc::clone(&sdp_patch_context.sdp_sidecars),
        k8s_dns_service: sdp_patch_context.k8s_dns_service,
        k8s_server_version: sdp_patch_context.k8s_server_version,
    };
    let admission_response = Box::new(AdmissionResponse::from(&admission_request));
    admission_request
        .service_id()
        .map_err(SDPPatchError::from_admission_response(Box::clone(
            &admission_response,
        )))?;
    // Try to get the service environment and use it to patch the POD
    let mut environment =
        ServiceEnvironment::create(&admission_request, sdp_patch_context.identity_store)
            .await
            .map_err(SDPPatchError::from_admission_response(Box::clone(
                &admission_response,
            )))?;
    let patch = sdp_pod.patch(&mut environment, admission_request).map_err(
        SDPPatchError::from_admission_response(Box::clone(&admission_response)),
    )?;
    let admission_response = Box::clone(&admission_response)
        .with_patch(patch)
        .map_err(SDPServiceError::from_error("Error serializing JSONPatch"))
        .map_err(SDPPatchError::from_admission_response(Box::clone(
            &admission_response,
        )))?;
    Ok(SDPPatchResponse::Allow(Box::new(admission_response)))
}

async fn mutate<'a, E: IdentityStore<ServiceIdentity>>(
    body: Bytes,
    sdp_injector_context: Arc<SDPInjectorContext<E>>,
) -> Result<SDPPatchResponse, SDPPatchError> {
    let k8s_dns_service = dns_service_discover(&(&sdp_injector_context.services_api)).await;
    let sdp_patch_context = SDPPatchContext {
        k8s_dns_service: k8s_dns_service,
        sdp_sidecars: Arc::clone(&sdp_injector_context.sdp_sidecars),
        identity_store: sdp_injector_context.identity_store.lock().await,
        k8s_server_version: sdp_injector_context.server_version,
    };
    let body = from_utf8(&body)
        .map(|s| s.to_string())
        .map_err(SDPServiceError::from_error("Unable to parse request body"))?;
    let admission_review = serde_json::from_str::<AdmissionReview<DynamicObject>>(&body).map_err(
        SDPServiceError::from_error("Unable to parse AdmissionReview<DynamicObject>"),
    )?;
    let mut attempts_store = sdp_injector_context.attempts_store.lock().await;
    match admission_review
        .request
        .as_ref()
        .map(|r| r.resource.resource.as_str())
    {
        Some("pods") => {
            let admission_request = admission_request!(body, Pod);
            attempt_patch!(
                admission_request,
                patch_pod(admission_request, sdp_patch_context),
                attempts_store,
                "pods"
            )
        }
        Some("deployments") => {
            let admission_request = admission_request!(body, Deployment);
            let ns = admission_request.namespace().ok_or_else(|| {
                SDPServiceError::from("Unable to get namespace from Deployment admission request")
            })?;
            let ns = sdp_injector_context.ns_api.get_opt(&ns).await.map_err(
                SDPServiceError::from_error(
                    "Unable to get Namespace object from Deployment admission request",
                ),
            )?;
            attempt_patch!(
                admission_request,
                patch_deployment(admission_request, ns),
                attempts_store,
                "deployments"
            )
        }
        Some(s) => Err(SDPPatchError::WithoutResponse(
            SDPServiceError::from_string(format!("Unknown resource to patch: {}", s)),
        )),
        None => Err(SDPPatchError::WithoutResponse(SDPServiceError::from(
            "Unknown resource to patch",
        ))),
    }
}

async fn validate(body: Bytes, sdp_sidecars: Arc<SDPSidecars>) -> Result<Body, Box<dyn Error>> {
    let body = from_utf8(&body).map(|s| s.to_string())?;
    let admission_review = serde_json::from_str::<AdmissionReview<Pod>>(&body)?;
    let admission_request: AdmissionRequest<Pod> = admission_review
        .request
        .ok_or("Not found request in admission review")?;
    let _ = admission_request
        .object
        .as_ref()
        .ok_or("Admission review does not contain a POD")?;
    let sdp_pod = SDPPod {
        sdp_sidecars: sdp_sidecars,
        k8s_dns_service: Option::None,
        k8s_server_version: 0,
    };
    let mut admission_response = AdmissionResponse::from(&admission_request);
    if let Err(error) = sdp_pod.validate(admission_request) {
        let mut status: Status = Default::default();
        status.code = Some(40);
        status.message = Some(error);
        admission_response.allowed = false;
        admission_response.result = status;
    }
    Ok(Body::from(serde_json::to_string(&admission_response)?))
}

async fn get_dns_service(k8s_client: &Api<KubeService>) -> Result<Option<Body>, Box<dyn Error>> {
    let k8s_dns_service = dns_service_discover(k8s_client).await;
    match k8s_dns_service {
        Some(s) => Ok(Some(Body::from(serde_json::to_string(&s)?))),
        None => Ok(None),
    }
}

#[cfg(test)]
mod tests {
    use crate::deviceid::{IdentityStore, InMemoryIdentityStore};
    use crate::injector::{
        patch_pod, DNSConfig, Patched, SDPPatchContext, SDPPatchResponse, SDPPod,
        ServiceEnvironment, SDP_ANNOTATION_CLIENT_CONFIG, SDP_ANNOTATION_CLIENT_DEVICE_ID,
        SDP_ANNOTATION_CLIENT_SECRETS, SDP_ANNOTATION_DNS_SEARCHES, SDP_SERVICE_CONTAINER_NAME,
        SDP_SIDECARS_FILE_ENV,
    };
    use crate::{load_sidecar_containers, SDPSidecars};
    use json_patch::Patch;
    use k8s_openapi::api::core::v1::{
        Container, LocalObjectReference, Pod, PodSecurityContext, Service as KubeService,
        ServiceSpec, ServiceStatus, Sysctl, Volume,
    };
    use kube::core::admission::AdmissionReview;
    use kube::core::ObjectMeta;
    use sdp_common::annotations::SDP_INJECTOR_ANNOTATION_ENABLED;
    use sdp_common::constants::SDP_DEFAULT_CLIENT_VERSION_ENV;
    use sdp_common::crd::{DeviceId, DeviceIdSpec, ServiceIdentity, ServiceIdentitySpec};
    use sdp_common::service::{
        containers, init_containers, security_context, volume_names, ServiceUser,
    };
    use sdp_common::traits::{
        Annotated, Candidate, MaybeNamespaced, MaybeService, Named, Namespaced, ObjectRequest,
        Validated,
    };
    use sdp_macros::{service_device_ids, service_identity, service_user};
    use sdp_test_macros::{pod, set_pod_field};
    use serde_json::json;
    use std::collections::{BTreeMap, HashMap, HashSet};
    use std::iter::FromIterator;
    use std::sync::Arc;
    use tokio::sync::mpsc::channel;
    use tokio::sync::Mutex;

    macro_rules! dns_service {
        ($ip:expr) => {
            KubeService {
                metadata: ObjectMeta::default(),
                spec: Some(ServiceSpec {
                    cluster_ip: Some(format!("{}", $ip)),
                    ..Default::default()
                }),
                status: Some(ServiceStatus::default()),
            }
        };
    }

    fn load_sidecar_containers_env() -> Result<SDPSidecars, String> {
        let manifest_dir =
            std::env::var("CARGO_MANIFEST_DIR").expect("env var CARGO_MANIFEST_DIR not defined!");
        std::env::set_var(
            SDP_SIDECARS_FILE_ENV,
            format!("{}/../tests/sdp-sidecars.json", manifest_dir),
        );
        std::env::set_var(SDP_DEFAULT_CLIENT_VERSION_ENV, "6.0.1");
        load_sidecar_containers()
            .map_err(|e| format!("Unable to load the sidecar information {}", e.to_string()))
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
        service: KubeService,
        dns_searches: Option<Vec<String>>,
        image_pull_secrets: Option<Vec<&'a str>>,
        sysctls: Option<Vec<Sysctl>>,
        k8s_server_version: u32,
    }

    impl Default for TestPatch<'_> {
        fn default() -> Self {
            TestPatch {
                pod: pod!(0),
                needs_patching: false,
                client_config_map: "ns0-srv0-service-config",
                client_secrets: "ns0-srv0-service-user",
                envs: vec![],
                service: dns_service!("10.10.10.10"),
                dns_searches: Some(vec![
                    "svc.cluster.local".to_string(),
                    "cluster.local".to_string(),
                ]),
                image_pull_secrets: None,
                sysctls: None,
                k8s_server_version: 22,
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
                pod: pod!(0),
                envs: vec![
                    ("POD_N_CONTAINERS".to_string(), Some("0".to_string())),
                    ("SERVICE_NAME".to_string(), Some("ns0_srv0".to_string())),
                ],
                service: KubeService::default(),
                dns_searches: Some(vec![
                    "ns0.svc.cluster.local".to_string(),
                    "svc.cluster.local".to_string(),
                    "cluster.local".to_string(),
                ]),
                ..Default::default()
            },
            TestPatch {
                pod: pod!(1,
                containers => vec!["random-service"],
                annotations => vec![
                    (SDP_ANNOTATION_CLIENT_DEVICE_ID, "00000000-0000-0000-0000-000000000001")
                ],
                image_pull_secrets => vec!["secret1","secret2"],
                sysctls => vec![Sysctl {
                    name: "some.sysctl.specified.by.user".to_string(),
                    value: "666".to_string(),
                }]),
                needs_patching: true,
                envs: vec![
                    ("POD_N_CONTAINERS".to_string(), Some("1".to_string())),
                    (
                        "K8S_DNS_SERVICE".to_string(),
                        Some("10.10.10.10".to_string()),
                    ),
                    (
                        "APPGATE_DEVICE_ID".to_string(),
                        Some("00000000-0000-0000-0000-000000000001".to_string()),
                    ),
                    ("SERVICE_NAME".to_string(), Some("ns1_srv1".to_string())),
                ],
                client_config_map: "ns1-srv1-service-config",
                client_secrets: "ns1-srv1-service-user",
                dns_searches: Some(vec![
                    "ns1.svc.cluster.local".to_string(),
                    "svc.cluster.local".to_string(),
                    "cluster.local".to_string(),
                ]),
                image_pull_secrets: Some(vec!["secret1", "secret2"]),
                sysctls: Some(vec![
                    Sysctl {
                        name: "net.ipv4.ip_unprivileged_port_start".to_string(),
                        value: "0".to_string(),
                    },
                    Sysctl {
                        name: "some.sysctl.specified.by.user".to_string(),
                        value: "666".to_string(),
                    },
                ]),
                ..Default::default()
            },
            TestPatch {
                pod: pod!(2,
                    containers => vec!["random-service"],
                    annotations => vec![("sdp-injector", "false")]),
                envs: vec![
                    ("POD_N_CONTAINERS".to_string(), Some("1".to_string())),
                    (
                        "K8S_DNS_SERVICE".to_string(),
                        Some("10.10.10.10".to_string()),
                    ),
                    ("SERVICE_NAME".to_string(), Some("ns2_srv2".to_string())),
                    (
                        "APPGATE_DEVICE_ID".to_string(),
                        Some("00000000-0000-0000-0000-000000000002".to_string()),
                    ),
                ],
                needs_patching: true,
                client_config_map: "ns2-srv2-service-config",
                client_secrets: "ns2-srv2-service-user",
                dns_searches: Some(vec![
                    "ns2.svc.cluster.local".to_string(),
                    "svc.cluster.local".to_string(),
                    "cluster.local".to_string(),
                ]),
                ..Default::default()
            },
            TestPatch {
                pod: pod!(3,
                containers => vec!["random-service"],
                annotations => vec![
                    ("sdp-injector", "who-knows"),
                    (SDP_ANNOTATION_CLIENT_SECRETS, "some-secrets"),
                    (SDP_ANNOTATION_CLIENT_CONFIG, "some-config-map"),
                    (SDP_ANNOTATION_CLIENT_DEVICE_ID, "00000000-0000-0000-0000-000000000003"),
                    (SDP_ANNOTATION_DNS_SEARCHES, "one.svc.local two.svc.local svc.local")
                ]),
                needs_patching: true,
                client_config_map: "some-config-map",
                client_secrets: "some-secrets",
                envs: vec![
                    ("POD_N_CONTAINERS".to_string(), Some("1".to_string())),
                    (
                        "K8S_DNS_SERVICE".to_string(),
                        Some("10.10.10.10".to_string()),
                    ),
                    (
                        "APPGATE_DEVICE_ID".to_string(),
                        Some("00000000-0000-0000-0000-000000000003".to_string()),
                    ),
                    ("SERVICE_NAME".to_string(), Some("ns3_srv3".to_string())),
                ],
                dns_searches: Some(vec![
                    "ns3.one.svc.local".to_string(),
                    "one.svc.local".to_string(),
                    "two.svc.local".to_string(),
                    "svc.local".to_string(),
                ]),
                ..Default::default()
            },
            TestPatch {
                pod: pod!(4,
                containers => vec!["random-service"],
                annotations => vec![("sdp-injector", "true"),
                    (SDP_ANNOTATION_CLIENT_DEVICE_ID, "00000000-0000-0000-0000-000000000004"),
                    (SDP_ANNOTATION_DNS_SEARCHES, "ns4.one.svc.local two.svc.local svc.local"),
                    (SDP_ANNOTATION_CLIENT_SECRETS, "some-secrets"),
                    (SDP_ANNOTATION_CLIENT_CONFIG, "some-config")
                ]),
                needs_patching: true,
                client_secrets: "some-secrets",
                client_config_map: "some-config",
                envs: vec![
                    ("POD_N_CONTAINERS".to_string(), Some("1".to_string())),
                    (
                        "K8S_DNS_SERVICE".to_string(),
                        Some("10.10.10.10".to_string()),
                    ),
                    (
                        "APPGATE_DEVICE_ID".to_string(),
                        Some("00000000-0000-0000-0000-000000000004".to_string()),
                    ),
                    ("SERVICE_NAME".to_string(), Some("ns4_srv4".to_string())),
                ],
                dns_searches: Some(vec![
                    "ns4.one.svc.local".to_string(),
                    "one.svc.local".to_string(),
                    "two.svc.local".to_string(),
                    "svc.local".to_string(),
                ]),
                ..Default::default()
            },
            TestPatch {
                pod: pod!(5,
                containers => vec![
                    "some-random-service-1",
                    "some-random-service-2"
                ],
                annotations => vec![
                    (SDP_ANNOTATION_CLIENT_CONFIG, "some-config-map")
                ]),
                needs_patching: true,
                envs: vec![
                    ("POD_N_CONTAINERS".to_string(), Some("2".to_string())),
                    ("K8S_DNS_SERVICE".to_string(), Some("10.0.0.10".to_string())),
                    ("SERVICE_NAME".to_string(), Some("ns5_srv5".to_string())),
                    (
                        "APPGATE_DEVICE_ID".to_string(),
                        Some("00000000-0000-0000-0000-000000000005".to_string()),
                    ),
                ],
                service: dns_service!("10.0.0.10"),
                client_config_map: "ns5-srv5-service-config",
                client_secrets: "ns5-srv5-service-user",
                dns_searches: Some(vec![
                    "ns5.svc.cluster.local".to_string(),
                    "svc.cluster.local".to_string(),
                    "cluster.local".to_string(),
                ]),
                ..Default::default()
            },
            TestPatch {
                pod: pod!(6,
                containers => vec![
                    sdp_sidecar_names[0].clone(),
                    sdp_sidecar_names[1].clone(),
                    "some-random-service".to_string()
                ]),
                envs: vec![
                    ("POD_N_CONTAINERS".to_string(), Some("3".to_string())),
                    (
                        "K8S_DNS_SERVICE".to_string(),
                        Some("10.10.10.10".to_string()),
                    ),
                    ("SERVICE_NAME".to_string(), Some("ns6_srv6".to_string())),
                ],
                client_config_map: "ns6-srv6-service-config",
                client_secrets: "ns6-srv6-service-user",
                dns_searches: Some(vec![
                    "ns6.svc.cluster.local".to_string(),
                    "svc.cluster.local".to_string(),
                    "cluster.local".to_string(),
                ]),
                ..Default::default()
            },
            TestPatch {
                pod: pod!(7,
                    containers => vec![
                        sdp_sidecar_names[0].clone(),
                        sdp_sidecar_names[1].clone(),
                        "some-random-service".to_string()
                    ],
                    annotations => vec![("sdp-injector", "true")]),
                envs: vec![
                    ("POD_N_CONTAINERS".to_string(), Some("3".to_string())),
                    (
                        "K8S_DNS_SERVICE".to_string(),
                        Some("10.10.10.10".to_string()),
                    ),
                    ("SERVICE_NAME".to_string(), Some("ns7_srv7".to_string())),
                ],
                client_config_map: "ns7-srv7-service-config",
                client_secrets: "ns7-srv7-service-user",
                dns_searches: Some(vec![
                    "ns7.svc.cluster.local".to_string(),
                    "svc.cluster.local".to_string(),
                    "cluster.local".to_string(),
                ]),
                ..Default::default()
            },
            TestPatch {
                pod: pod!(8,
                containers => vec![
                    sdp_sidecar_names[1].clone(),
                    "some-random-service".to_string()
                ]),
                envs: vec![
                    ("POD_N_CONTAINERS".to_string(), Some("2".to_string())),
                    (
                        "K8S_DNS_SERVICE".to_string(),
                        Some("10.10.10.10".to_string()),
                    ),
                    ("SERVICE_NAME".to_string(), Some("ns8_srv8".to_string())),
                ],
                client_config_map: "ns8-srv8-service-config",
                client_secrets: "ns8-srv8-service-user",
                dns_searches: Some(vec![
                    "ns8.svc.cluster.local".to_string(),
                    "svc.cluster.local".to_string(),
                    "cluster.local".to_string(),
                ]),
                ..Default::default()
            },
            TestPatch {
                pod: pod!(9,
                    containers => vec![
                        sdp_sidecar_names[1].clone(),
                        "some-random-service".to_string()
                    ],
                    annotations => vec![("sdp-injector", "true")]),
                envs: vec![
                    ("POD_N_CONTAINERS".to_string(), Some("2".to_string())),
                    (
                        "K8S_DNS_SERVICE".to_string(),
                        Some("10.10.10.10".to_string()),
                    ),
                    ("SERVICE_NAME".to_string(), Some("ns9_srv9".to_string())),
                ],
                client_config_map: "ns9-srv9-service-config",
                client_secrets: "ns9-srv9-service-user",
                dns_searches: Some(vec![
                    "ns9.svc.cluster.local".to_string(),
                    "svc.cluster.local".to_string(),
                    "cluster.local".to_string(),
                ]),
                ..Default::default()
            },
            TestPatch {
                pod: pod!(10,
                    containers => vec!["random-service"],
                    init_containers => vec!["random-init-container"]),
                needs_patching: true,
                envs: vec![
                    ("POD_N_CONTAINERS".to_string(), Some("1".to_string())),
                    (
                        "K8S_DNS_SERVICE".to_string(),
                        Some("10.10.10.10".to_string()),
                    ),
                    (
                        "APPGATE_DEVICE_ID".to_string(),
                        Some("00000000-0000-0000-0000-000000000010".to_string()),
                    ),
                    ("SERVICE_NAME".to_string(), Some("ns10_srv10".to_string())),
                ],
                client_config_map: "ns10-srv10-service-config",
                client_secrets: "ns10-srv10-service-user",
                dns_searches: Some(vec![
                    "ns10.svc.cluster.local".to_string(),
                    "svc.cluster.local".to_string(),
                    "cluster.local".to_string(),
                ]),
                ..Default::default()
            },
            TestPatch {
                pod: pod!(11,
                containers => vec!["random-service"],
                sysctls => vec![Sysctl {
                    name: "some.sysctl.specified.by.user".to_string(),
                    value: "666".to_string()
                }]),
                envs: vec![
                    ("POD_N_CONTAINERS".to_string(), Some("1".to_string())),
                    (
                        "K8S_DNS_SERVICE".to_string(),
                        Some("10.10.10.10".to_string()),
                    ),
                    ("SERVICE_NAME".to_string(), Some("ns11_srv11".to_string())),
                    (
                        "APPGATE_DEVICE_ID".to_string(),
                        Some("00000000-0000-0000-0000-000000000011".to_string()),
                    ),
                ],
                needs_patching: true,
                client_config_map: "ns11-srv11-service-config",
                client_secrets: "ns11-srv11-service-user",
                dns_searches: Some(vec![
                    "ns11.svc.cluster.local".to_string(),
                    "svc.cluster.local".to_string(),
                    "cluster.local".to_string(),
                ]),
                k8s_server_version: 11,
                sysctls: Some(vec![Sysctl {
                    name: "some.sysctl.specified.by.user".to_string(),
                    value: "666".to_string(),
                }]),
                ..Default::default()
            },
            TestPatch {
                pod: pod!(12, containers => vec!["random-service"]),
                envs: vec![
                    ("POD_N_CONTAINERS".to_string(), Some("1".to_string())),
                    (
                        "K8S_DNS_SERVICE".to_string(),
                        Some("10.10.10.10".to_string()),
                    ),
                    ("SERVICE_NAME".to_string(), Some("ns12_srv12".to_string())),
                    (
                        "APPGATE_DEVICE_ID".to_string(),
                        Some("00000000-0000-0000-0000-000000000012".to_string()),
                    ),
                ],
                needs_patching: true,
                client_config_map: "ns12-srv12-service-config",
                client_secrets: "ns12-srv12-service-user",
                dns_searches: Some(vec![
                    "ns12.svc.cluster.local".to_string(),
                    "svc.cluster.local".to_string(),
                    "cluster.local".to_string(),
                ]),
                sysctls: Some(vec![Sysctl {
                    name: "net.ipv4.ip_unprivileged_port_start".to_string(),
                    value: "0".to_string(),
                }]),
                ..Default::default()
            },
        ]
    }

    fn validation_tests() -> Vec<TestValidate> {
        vec![
            TestValidate {
                pod: pod!(0),
                validation_errors: Some(r#"Unable to run SDP client on Pod: Pod is missing required containers: sdp-dnsmasq, sdp-driver, sdp-service
Pod is missing required volumes: pod-info, run-sdp-dnsmasq, run-sdp-driver, tun-device"#.to_string()),
            },
            TestValidate {
                pod: pod!(1, containers => vec!["sdp-dnsmasq"]),
                validation_errors: Some(r#"Unable to run SDP client on Pod: Pod is missing required containers: sdp-driver, sdp-service
Pod is missing required volumes: pod-info, run-sdp-dnsmasq, run-sdp-driver, tun-device"#.to_string()),
            },
            TestValidate {
                pod: pod!(2, containers => vec!["sdp-dnsmasq", "sdp-service"]),
                validation_errors: Some(r#"Unable to run SDP client on Pod: Pod is missing required containers: sdp-driver
Pod is missing required volumes: pod-info, run-sdp-dnsmasq, run-sdp-driver, tun-device"#.to_string()),
            },
            TestValidate {
                pod: pod!(3, containers => vec!["sdp-service", "sdp-dnsmasq", "sdp-driver"]),
                validation_errors: Some(r#"Unable to run SDP client on Pod: Pod is missing required volumes: pod-info, run-sdp-dnsmasq, run-sdp-driver, tun-device"#.to_string()),
            },
            TestValidate {
                pod: pod!(4, containers => vec!["sdp-service", "sdp-dnsmasq", "sdp-driver"],
                                  volumes => vec!["run-sdp-dnsmasq", "run-sdp-driver"]),
                validation_errors: Some(r#"Unable to run SDP client on Pod: Pod is missing required volumes: pod-info, tun-device"#.to_string()),
            },
            TestValidate {
                pod: pod!(5, containers => vec!["sdp-service", "sdp-dnsmasq", "sdp-driver"],
                                  volumes => vec!["run-sdp-driver", "run-sdp-dnsmasq"]),
                validation_errors: Some(r#"Unable to run SDP client on Pod: Pod is missing required volumes: pod-info, tun-device"#.to_string()),
            },
            TestValidate {
                pod: pod!(6, containers => vec!["sdp-driver"],
                                  volumes => vec!["run-sdp-driver", "tun-device", "pod-info", "run-sdp-dnsmasq"]),
                validation_errors: Some(r#"Unable to run SDP client on Pod: Pod is missing required containers: sdp-dnsmasq, sdp-service"#.to_string()),
            },
            TestValidate {
                pod: pod!(7, containers => vec!["sdp-service", "sdp-dnsmasq", "sdp-driver"],
                                  volumes => vec!["run-sdp-driver", "run-sdp-dnsmasq", "tun-device", "pod-info"]),
                validation_errors: None,
            },
            TestValidate {
                pod: pod!(8, containers => vec!["_sdp-service", "_sdp-dnsmasq", "_sdp-driver"],
                                  volumes => vec!["_run-appgate", "_tun-device"]),
                validation_errors: Some(r#"Unable to run SDP client on Pod: Pod is missing required containers: sdp-dnsmasq, sdp-driver, sdp-service
Pod is missing required volumes: pod-info, run-sdp-dnsmasq, run-sdp-driver, tun-device"#.to_string()),
            },
        ]
    }

    #[test]
    fn test_needs_patching() {
        let sdp_sidecars =
            Arc::new(load_sidecar_containers_env().expect("Unable to load test SDPSidecars: "));
        let results: Vec<TestResult> = patch_tests(&sdp_sidecars)
            .iter()
            .map(|t| {
                let sdp_pod = SDPPod {
                    sdp_sidecars: Arc::clone(&sdp_sidecars),
                    k8s_dns_service: Some(t.service.clone()),
                    k8s_server_version: 22,
                };
                let request = TestObjectRequest::new(t.pod.clone());
                let needs_patching = sdp_pod.needs_patching(&request);
                (
                    t.needs_patching == needs_patching,
                    "Needs patching simple".to_string(),
                    format!("Got {}, expected {}", needs_patching, t.needs_patching),
                )
            })
            .collect();
        assert_tests(&results)
    }

    #[test]
    fn test_is_candidate_pod() {
        let pod = pod!(0);
        assert!(pod.is_candidate());
        let pod = pod!(0, annotations => vec![(SDP_INJECTOR_ANNOTATION_ENABLED, "false")]);
        assert!(!pod.is_candidate());
        let pod = pod!(0, annotations => vec![(SDP_INJECTOR_ANNOTATION_ENABLED, "true")]);
        assert!(pod.is_candidate());
    }

    struct TestObjectRequest {
        pod: Pod,
    }

    impl TestObjectRequest {
        fn new(pod: Pod) -> Self {
            TestObjectRequest { pod: pod }
        }
    }

    impl ObjectRequest<Pod> for TestObjectRequest {
        fn object(&self) -> Option<&Pod> {
            Some(&self.pod)
        }
    }

    impl Candidate for TestObjectRequest {
        fn is_candidate(&self) -> bool {
            self.pod.is_candidate()
        }
    }

    impl Named for TestObjectRequest {
        fn name(&self) -> String {
            self.pod.name()
        }
    }

    impl MaybeNamespaced for TestObjectRequest {
        fn namespace(&self) -> Option<String> {
            self.pod.namespace()
        }
    }

    impl MaybeService for TestObjectRequest {}

    impl Annotated for TestObjectRequest {
        fn annotations(&self) -> Option<&BTreeMap<String, String>> {
            self.pod.annotations()
        }
    }

    fn container_names(xs: &Vec<Container>) -> Vec<String> {
        xs.iter().map(|x| x.name.clone()).collect()
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

        let expected_init_containers = |xs: &[String]| -> Vec<String> {
            let mut xs = Vec::from(xs);
            xs.insert(0, "sdp-init-container-0".to_string());
            xs.push("sdp-init-container-f".to_string());
            xs
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
        let assert_patch = |pod: &Pod, patch: Patch| -> Result<Pod, String> {
            let mut unpatched_pod = serde_json::to_value(pod)
                .map_err(|e| format!("Unable to convert Pod to value [{}]", e.to_string()))?;
            json_patch::patch(&mut unpatched_pod, &patch)
                .map_err(|e| format!("Unable to patch Pod [{}]", e.to_string()))?;
            let patched_pod: Pod = serde_json::from_value(unpatched_pod)
                .map_err(|e| format!("Unable to convert patched Pod [{}]", e.to_string()))?;
            Ok(patched_pod)
        };

        let assert_volumes = |pod: &Pod, unpatched_volumes: Vec<String>| -> Result<bool, String> {
            let patched_volumes =
                HashSet::from_iter(volume_names(pod).unwrap_or(vec![]).iter().cloned());
            (patched_volumes == expected_volumes(&unpatched_volumes))
                .then(|| true)
                .ok_or(format!(
                    "Wrong volumes after patch, got {:?}, expected {:?}",
                    patched_volumes,
                    expected_volumes(&unpatched_volumes)
                ))
        };

        let assert_containers =
            |pod: &Pod, unpatched_containers: Vec<String>| -> Result<bool, String> {
                let patched_containers = HashSet::from_iter(container_names(
                    containers(pod).expect("Not found containers"),
                ));
                (patched_containers == expected_containers(&unpatched_containers))
                    .then(|| true)
                    .ok_or(format!(
                        "Wrong containers after patch, got {:?}, expected {:?}",
                        patched_containers,
                        expected_containers(&unpatched_containers)
                    ))
            };

        let assert_init_containers =
            |pod: &Pod, unpatched_containers: Vec<String>| -> Result<bool, String> {
                let init_containers = Vec::from_iter(container_names(
                    init_containers(pod).expect("Not found init containers"),
                ));
                (init_containers == expected_init_containers(&unpatched_containers))
                    .then(|| true)
                    .ok_or(format!(
                        "Wrong init containers after patch, got {:?}, expected {:?}",
                        init_containers,
                        expected_init_containers(&unpatched_containers)
                    ))
            };

        let assert_envs = |pod: &Pod, test_patch: &TestPatch| -> Result<bool, String> {
            let css = containers(pod).ok_or("Containers not found in POD")?;
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

        let assert_dnsconfig = |pod: &Pod, test_patch: &TestPatch| -> Result<bool, String> {
            let expected_ns = Some(vec!["127.0.0.1".to_string()]);
            pod.spec
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
                                "DNSConfig got searches {:?}, expected {:?}",
                                dc.searches, test_patch.dns_searches
                            )
                        })
                })
        };

        let assert_image_pull_secrets = |pod: &Pod, test_patch: &TestPatch| {
            let expected = test_patch.image_pull_secrets.clone();
            let got: Option<Vec<String>> = pod
                .spec
                .as_ref()
                .and_then(|ps| ps.image_pull_secrets.as_ref())
                .map(|image_pull_secrets| {
                    image_pull_secrets
                        .iter()
                        .map(|r| r.name.as_ref())
                        .filter(|r| r.is_some())
                        .map(|s| s.unwrap().clone())
                        .collect()
                });
            match (expected, got) {
                (Some(expected), None) => Err(format!(
                    "image_pull_secrets got None, expected {:?}",
                    expected
                )),
                (None, Some(got)) => {
                    Err(format!("image_pull_secrets got {:?}, expected None", got))
                }
                (None, None) => Ok(true),
                (Some(expected), Some(got)) => {
                    let a = (got == expected).then(|| true).ok_or_else(|| {
                        format!("image_pull_secrets: got {:?}, expected {:?}", got, expected)
                    });
                    a
                }
            }
        };

        let assert_security_context = |pod: &Pod, test_patch: &TestPatch| {
            if let Some(expected) = &test_patch.sysctls {
                match security_context(pod).and_then(|sc| sc.sysctls.as_ref()) {
                    Some(sc) => (sc == expected).then(|| true).ok_or_else(|| {
                        format!("securityContext: expected {:?} but got {:?}", expected, sc)
                    }),
                    None => Err(format!(
                        "securityContext.sysctls: expected {:?} but got None",
                        expected
                    )),
                }
            } else {
                Ok(true)
            }
        };

        let assert_patch =
            |pod: &Pod, patch: Patch, test_patch: &TestPatch| -> Result<bool, String> {
                let patched_pod = assert_patch(pod, patch)?;
                let unpatched_containers =
                    container_names(containers(pod).expect("Unable to get containers"));
                let init_containers = init_containers(pod);
                let unpatched_volumes = volume_names(pod).unwrap_or(vec![]);
                assert_volumes(&patched_pod, unpatched_volumes)?;
                assert_containers(&patched_pod, unpatched_containers)?;
                if init_containers.is_some() {
                    let unpatched_init_containers = container_names(init_containers.unwrap());
                    assert_init_containers(&patched_pod, unpatched_init_containers)?;
                }
                assert_envs(&patched_pod, test_patch)?;
                assert_dnsconfig(&patched_pod, test_patch)?;
                assert_image_pull_secrets(&patched_pod, test_patch)?;
                assert_security_context(&patched_pod, test_patch)
            };

        let test_description = || "Test patch".to_string();
        let identity_storage = Mutex::new(InMemoryIdentityStore::default());

        let mut results: Vec<TestResult> = vec![];
        for (n, test) in patch_tests(&sdp_sidecars).iter().enumerate() {
            let pod = test.pod.clone();
            let sdp_pod = SDPPod {
                sdp_sidecars: Arc::clone(&sdp_sidecars),
                k8s_dns_service: Some(test.service.clone()),
                k8s_server_version: test.k8s_server_version,
            };
            let pod_name = pod.name();
            identity_storage
                .lock()
                .await
                .register_service(service_identity!(n))
                .await
                .expect("Unable to register service identity");
            identity_storage
                .lock()
                .await
                .register_device_ids(service_device_ids!(n))
                .await
                .expect("Unable to register device id");

            let request = TestObjectRequest::new(pod.clone());
            let mut env = ServiceEnvironment::create(&request, identity_storage.lock().await)
                .await
                .expect(
                    format!("Unable to create ServiceEnvironment from POD {}", &pod_name).as_str(),
                );
            let needs_patching = sdp_pod.needs_patching(&request);
            let patch = sdp_pod.patch(&mut env, request);
            if test.needs_patching && needs_patching {
                match patch
                    .map_err(|e| e.to_string())
                    .and_then(|p| assert_patch(&pod, p, test))
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
        let identity_storage = Mutex::new(InMemoryIdentityStore::default());

        for (n, _t) in patch_tests(&sdp_sidecars).iter().enumerate() {
            identity_storage
                .lock()
                .await
                .register_service(service_identity!(n))
                .await
                .expect("Unable to register service identity");
            identity_storage
                .lock()
                .await
                .register_device_ids(service_device_ids!(n))
                .await
                .expect("Unable to register device id");
        }

        let mut results: Vec<TestResult> = vec![];
        for (n, test) in patch_tests(&sdp_sidecars).iter().enumerate() {
            let pod_value =
                serde_json::to_value(&test.pod).expect(&format!("Unable to parse test input"));
            let admission_review_value = json!({
                "apiVersion": "admission.k8s.io/v1",
                "kind": "AdmissionReview",
                "request": {
                    "uid": "705ab4f5-6393-11e8-b7cc-42010a800002",
                    "kind" :{"group":"","version":"v1","kind":"Pod"},
                    "resource":{"group":"","version":"v1","resource":"pods"},
                    "requestKind":{"group":"","version": "v1","kind":"Pod"},
                    "requestResource":{"group":"","version":"v1","resource":"pods"},
                    "name": service_identity!(n).name(),
                    "namespace": service_identity!(n).namespace(),
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

            let admission_review: AdmissionReview<Pod> =
                serde_json::from_value(admission_review_value)
                    .expect("Unable to parse generated AdmissionReview value");
            // copy the POD
            let copied_pod: Pod = serde_json::to_value(&test.pod)
                .and_then(|v| serde_json::from_value(v))
                .expect("Unable to deserialize POD");
            let mut request = admission_review.request.expect("Error getting request");
            request.object = Some(copied_pod.clone());
            // test the patch_request function now
            // TODO: Test responses not allowing the patch!
            let sdp_sidecars = Arc::clone(&sdp_sidecars);
            let patch_response = {
                let sdp_patch_context = SDPPatchContext {
                    sdp_sidecars: Arc::clone(&sdp_sidecars),
                    identity_store: identity_storage.lock().await,
                    k8s_dns_service: Some(test.service.clone()),
                    k8s_server_version: 22,
                };
                patch_pod(request, sdp_patch_context)
                    .await
                    .map_err(|e| format!("Could not generate a response: {:?}", e))
            };
            match patch_response {
                Ok(SDPPatchResponse::Allow(response)) if !response.allowed => {
                    results.push((
                        false,
                        "Injection Containers Test".to_string(),
                        format!(
                            "{} got response not allowed,, expected response allowed",
                            &copied_pod.name()
                        ),
                    ));
                }
                Ok(SDPPatchResponse::Allow(response)) => {
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
                                              format!(
                                                  "Pod {} needs patching but not patches were included in the response",
                                                  copied_pod.name()
                                              )));
                            }
                            (false, true) => {
                                results.push((false, "Injection Containers Test".to_string(),
                                              format!("Pod {} does not need patching but patches were included in the response: {:?}",
                                                      copied_pod.name(), ps)));
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
                Ok(SDPPatchResponse::RetryWithError(_, _, _))
                | Ok(SDPPatchResponse::MaxRetryExceeded(_, _)) => {
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
        let results: Vec<TestResult> = validation_tests()
            .iter()
            .map(|t| {
                let sdp_pod = SDPPod {
                    sdp_sidecars: Arc::clone(&sdp_sidecars),
                    k8s_dns_service: Some(dns_service!("10.10.10.10")),
                    k8s_server_version: 22,
                };
                let request = TestObjectRequest::new(t.pod.clone());
                let pass_validation = sdp_pod.validate(request);
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
        let pod = pod!(1);
        let store = Mutex::new(InMemoryIdentityStore::default());
        let request = TestObjectRequest::new(pod.clone());
        let env = ServiceEnvironment::from_identity_store(&request, store.lock().await).await;
        assert!(env.is_err());
        if let Err(e) = env {
            assert_eq!(
                e.error,
                "ServiceIdentity and/or DeviceId is missing for service ns1_srv1"
            );
        }
        let id = service_identity!(1);
        let ds = service_device_ids!(1);
        store
            .lock()
            .await
            .register_service(id)
            .await
            .expect("Unable to register service identity");
        store
            .lock()
            .await
            .register_device_ids(ds)
            .await
            .expect("Unable to register device ids");

        let env = ServiceEnvironment::from_identity_store(&request, store.lock().await).await;
        assert!(env.is_ok());
        if let Ok(env) = env {
            assert_eq!(
                env.service_name,
                pod.service_id().expect("Unable to get service id from Pod")
            );
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
    fn test_prefix_searches_1() {
        let sdp_sidecars = SDPSidecars {
            containers: Box::new(vec![]),
            volumes: Box::new(vec![]),
            dns_config: Box::new(DNSConfig {
                searches: "svc.cluster.local cluster.local".to_string(),
            }),
            init_containers: Box::new((Container::default(), Container::default())),
        };

        assert_eq!(
            sdp_sidecars.dns_config(&"ns1", None).searches.unwrap(),
            vec!(
                "ns1.svc.cluster.local",
                "svc.cluster.local",
                "cluster.local"
            )
        )
    }

    #[test]
    fn test_prefix_searches_2() {
        let sdp_sidecars = SDPSidecars {
            containers: Box::new(vec![]),
            volumes: Box::new(vec![]),
            dns_config: Box::new(DNSConfig {
                searches: "ns1.svc.cluster.local cluster.local".to_string(),
            }),
            init_containers: Box::new((Container::default(), Container::default())),
        };

        assert_eq!(
            sdp_sidecars.dns_config(&"ns1", None).searches.unwrap(),
            vec!(
                "ns1.svc.cluster.local",
                "svc.cluster.local",
                "cluster.local"
            )
        )
    }

    #[test]
    fn test_prefix_searches_3() {
        let sdp_sidecars = SDPSidecars {
            containers: Box::new(vec![]),
            volumes: Box::new(vec![]),
            dns_config: Box::new(DNSConfig {
                searches: "ns1.svc.cluster.local cluster.local".to_string(),
            }),
            init_containers: Box::new((Container::default(), Container::default())),
        };

        let custom_searches = vec!["svc.test.local".to_string(), "test.local".to_string()];

        assert_eq!(
            sdp_sidecars
                .dns_config(&"ns1", Some(custom_searches))
                .searches
                .unwrap(),
            vec!("ns1.svc.test.local", "svc.test.local", "test.local")
        )
    }

    #[test]
    fn test_prefix_searches_4() {
        let sdp_sidecars = SDPSidecars {
            containers: Box::new(vec![]),
            volumes: Box::new(vec![]),
            dns_config: Box::new(DNSConfig {
                searches: "ns1.svc.cluster.local cluster.local".to_string(),
            }),
            init_containers: Box::new((Container::default(), Container::default())),
        };

        let custom_searches = vec!["ns1.svc.test.local".to_string(), "test.local".to_string()];
        assert_eq!(
            sdp_sidecars
                .dns_config(&"ns1", Some(custom_searches))
                .searches
                .unwrap(),
            vec!("ns1.svc.test.local", "svc.test.local", "test.local")
        )
    }

    #[test]
    fn test_service_environment_from_pod() {
        // Missing all annotation
        let mut pod = pod!(0);
        let request = TestObjectRequest::new(pod);
        let mut env = ServiceEnvironment::from_pod(&request);
        assert!(env.is_ok());
        assert!(env.as_ref().unwrap().is_none());

        // Contains all three necessary annotations
        pod = pod!(0, annotations => vec![
            (SDP_ANNOTATION_CLIENT_SECRETS, "some-secrets"),
            (SDP_ANNOTATION_CLIENT_CONFIG, "some-config-map"),
            (SDP_ANNOTATION_CLIENT_DEVICE_ID, "00000000-0000-0000-0000-000000000001"),
        ]);
        let request = TestObjectRequest::new(pod);
        env = ServiceEnvironment::from_pod(&request);
        assert!(env.is_ok());
        assert!(env.as_ref().unwrap().is_some());
        if let Ok(Some(env)) = env {
            assert_eq!(env.service_name, "ns0_srv0".to_string());
            assert_eq!(env.client_config, "some-config-map".to_string());
            assert_eq!(env.client_secret_name, "some-secrets".to_string());
            assert_eq!(
                env.client_secret_controller_url_key,
                "service-url".to_string()
            );
            assert_eq!(env.client_secret_pwd_key, "service-password".to_string());
            assert_eq!(env.client_secret_user_key, "service-username".to_string());
            assert_eq!(env.n_containers, "0".to_string());
            assert_eq!(env.k8s_dns_service_ip, None);
            assert_eq!(env.client_device_id, "00000000-0000-0000-0000-000000000001");
        };

        // Missing config and device_id annotation
        pod = pod!(1, annotations => vec![
            (SDP_ANNOTATION_CLIENT_SECRETS, "some-secrets"),
        ]);
        let request = TestObjectRequest::new(pod);
        env = ServiceEnvironment::from_pod(&request);
        assert!(env.is_ok());
        assert!(env.as_ref().unwrap().is_none());

        // Missing secret and device_id annotation
        pod = pod!(1, annotations => vec![
            (SDP_ANNOTATION_CLIENT_CONFIG, "some-config-map")
        ]);
        let request = TestObjectRequest::new(pod);
        env = ServiceEnvironment::from_pod(&request);
        assert!(env.is_ok());
        assert!(env.as_ref().unwrap().is_none());

        // Missing secret and config annotation
        pod = pod!(1, annotations => vec![
            (SDP_ANNOTATION_CLIENT_DEVICE_ID, "00000000-0000-0000-0000-000000000001")
        ]);
        let request = TestObjectRequest::new(pod);
        env = ServiceEnvironment::from_pod(&request);
        assert!(env.is_ok());
        assert!(env.as_ref().unwrap().is_none());

        // Missing secret annotation
        pod = pod!(1, annotations => vec![
            (SDP_ANNOTATION_CLIENT_CONFIG, "some-config-map"),
            (SDP_ANNOTATION_CLIENT_DEVICE_ID, "00000000-0000-0000-0000-000000000001")
        ]);
        let request = TestObjectRequest::new(pod);
        env = ServiceEnvironment::from_pod(&request);
        assert!(env.is_ok());
        assert!(env.as_ref().unwrap().is_none());

        // Missing config annotation
        pod = pod!(1, annotations => vec![
            (SDP_ANNOTATION_CLIENT_SECRETS, "some-secrets"),
            (SDP_ANNOTATION_CLIENT_DEVICE_ID, "00000000-0000-0000-0000-000000000001")
        ]);
        let request = TestObjectRequest::new(pod);
        env = ServiceEnvironment::from_pod(&request);
        assert!(env.is_ok());
        assert!(env.as_ref().unwrap().is_none());

        // Missing device_id annotation
        pod = pod!(1, annotations => vec![
            (SDP_ANNOTATION_CLIENT_SECRETS, "some-secrets"),
            (SDP_ANNOTATION_CLIENT_CONFIG, "some-config-map"),
        ]);
        let request = TestObjectRequest::new(pod);
        env = ServiceEnvironment::from_pod(&request);
        assert!(env.is_ok());
        assert!(env.as_ref().unwrap().is_none());
    }

    #[tokio::test]
    async fn test_identity_manager_identity_storage_async() {
        let ch = channel::<bool>(1);
        let store = Arc::new(Mutex::new(InMemoryIdentityStore::default()));
        let store2 = Arc::clone(&store);
        let store3 = Arc::clone(&store);
        let mut ch_rx = ch.1;
        let ch_tx = ch.0;
        let t1 = tokio::spawn(async move {
            let id = service_identity!(1);
            let ds = service_device_ids!(1);
            Arc::clone(&store)
                .lock()
                .await
                .register_service(id)
                .await
                .expect("Unable to register service identity");
            Arc::clone(&store)
                .lock()
                .await
                .register_device_ids(ds)
                .await
                .expect("Unable to register device ids");
            ch_tx
                .send(true)
                .await
                .expect("Error notifying client thread in test");
        });
        let pod2 = pod!(2);

        let t2 = tokio::spawn(async move {
            let request = TestObjectRequest::new(pod2);
            let env = ServiceEnvironment::from_identity_store(&request, store2.lock().await).await;
            assert!(env.is_err());
            if let Err(ref e) = env {
                assert_eq!(
                    e.error,
                    "ServiceIdentity and/or DeviceId is missing for service ns2_srv2"
                );
            }
            if ch_rx
                .recv()
                .await
                .expect("Error receiving notification from server thread in test")
            {
                let pod1 = pod!(1);
                let request = TestObjectRequest::new(pod1);
                let env =
                    ServiceEnvironment::from_identity_store(&request, store3.lock().await).await;
                assert!(env.is_ok());
                if let Ok(env) = env {
                    assert_eq!(env.service_name, "ns1_srv1".to_string());
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

pub fn get_cert_path() -> String {
    std::env::var(SDP_CERT_FILE_ENV).unwrap_or(SDP_CERT_FILE.to_string())
}

pub fn get_key_path() -> String {
    std::env::var(SDP_KEY_FILE_ENV).unwrap_or(SDP_KEY_FILE.to_string())
}

pub fn load_ssl() -> Result<ServerConfig, SDPServiceError> {
    let cert_file = get_cert_path();
    let key_file = get_key_path();

    let mut cert_reader = BufReader::new(
        File::open(cert_file).map_err(|e| format!("Unable to open cert file: {}", e))?,
    );
    let mut key_reader = BufReader::new(
        File::open(key_file).map_err(|e| format!("Unable to open key file: {}", e))?,
    );

    let raw_certs =
        certs(&mut cert_reader).map_err(|e| format!("Unable to load certificates: {}", e))?;
    let certs: Vec<Certificate> = raw_certs.into_iter().map(Certificate).collect();
    let raw_keys =
        rsa_private_keys(&mut key_reader).map_err(|e| format!("Unable to load keys: {}", e))?;
    let keys: Vec<PrivateKey> = raw_keys.into_iter().map(PrivateKey).collect();

    Ok(ServerConfig::builder()
        .with_safe_defaults()
        .with_no_client_auth()
        .with_single_cert(certs, keys[0].clone())
        .map_err(|e| format!("Unable to create ServerConfig with TLS certificate: {}", e))?)
}

pub async fn injector_handler<E: IdentityStore<ServiceIdentity>>(
    req: Request<Body>,
    sdp_context: Arc<SDPInjectorContext<E>>,
) -> Result<Response<Body>, hyper::Error> {
    match (req.method(), req.uri().path()) {
        (&Method::POST, "/mutate") => {
            let bs = hyper::body::to_bytes(req).await?;
            match mutate(bs, sdp_context).await {
                // Object properly patched and allowed
                Ok(SDPPatchResponse::Allow(mut response)) => {
                    info!(
                        "Resource patched with {} patches",
                        response.patch.as_ref().map(|xs| xs.len()).unwrap_or(0)
                    );
                    // Object properly patched and allowed
                    allow_admission_response!(response => response)
                }
                Ok(SDPPatchResponse::MaxRetryExceeded(mut response, error)) => {
                    error!(
                        "Patch failed. Max retry exceeded [{}]. Accepting resource without patching: {}",
                        MAX_PATCH_ATTEMPTS,
                        error.to_string()
                    );
                    // Object patch failed, too many attempts so we accept the resource as it is
                    allow_admission_response!(response => response)
                }
                Ok(SDPPatchResponse::RetryWithError(response, error, attempts)) => {
                    error!(
                        "Patch failed [{}/{}]. Retrying: {}",
                        attempts,
                        MAX_PATCH_ATTEMPTS,
                        error.to_string()
                    );
                    // Object patch failed, don't accept the resource so k8s can retry
                    fail_admission_response!(response => response)
                }
                Err(SDPPatchError::WithResponse(response, e)) => {
                    error!("Patch failed: {}", e);
                    // We got an error with a response, fail with that response
                    fail_admission_response!(response => response)
                }
                Err(SDPPatchError::WithoutResponse(e)) => {
                    error!("Patch failed: {}", e);
                    // We got an error without a response, fail
                    fail_admission_response!(error => e)
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
        (&Method::GET, "/dns-service") => match get_dns_service(&sdp_context.services_api).await {
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
