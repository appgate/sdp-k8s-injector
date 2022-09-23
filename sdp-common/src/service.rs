use crate::constants::IDENTITY_MANAGER_SECRET_NAME;
pub use crate::crd::ServiceIdentity;
use crate::sdp::{auth::SDPUser, system::ClientProfileUrl};
use json_patch::PatchOperation::Remove;
use json_patch::{Patch, RemoveOperation};
use k8s_openapi::api::apps::v1::Deployment;
use k8s_openapi::api::core::v1::{ConfigMap, Pod};
use k8s_openapi::{api::core::v1::Secret, ByteString};
use kube::api::{DeleteParams, Patch as KubePatch, PatchParams, PostParams};
use kube::{Api, ResourceExt};
use log::{error, info, warn};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::{collections::BTreeMap, error::Error};

pub const SDP_INJECTOR_ANNOTATION: &str = "sdp-injector";

struct ServiceConfigFields {
    configmap_name: String,
    log_level: String,
}

impl ServiceConfigFields {
    fn new(service_ns: &str, service_name: &str) -> Self {
        ServiceConfigFields {
            configmap_name: format!("{}-{}-service-config", service_ns, service_name),
            log_level: "client-log-level".to_string(),
        }
    }
}

#[derive(Clone, JsonSchema, Debug, Serialize, Deserialize, PartialEq)]
pub struct ServiceUser {
    pub id: String,
    pub name: String,
    pub password: String,
    pub profile_url: String,
}

fn bytes_to_string(bs: &ByteString) -> Option<String> {
    match String::from_utf8(bs.0.clone()) {
        Err(e) => {
            error!("Unable to read secret: {}", e.to_string());
            None
        }
        Ok(v) => Some(v),
    }
}

fn is_secrets_namespaced(secrets_name: &str) -> bool {
    if secrets_name == IDENTITY_MANAGER_SECRET_NAME {
        false
    } else {
        true
    }
}

impl ServiceUser {
    pub fn from_sdp_user(
        sdp_user: &SDPUser,
        client_profile_url: &ClientProfileUrl,
        password: Option<&str>,
    ) -> Option<Self> {
        password
            .or_else(|| sdp_user.password.as_ref().map(|s| s.as_str()))
            .map(|pwd| Self {
                id: sdp_user.id.clone(),
                name: sdp_user.name.clone(),
                password: pwd.to_string(),
                profile_url: client_profile_url.url.clone(),
            })
    }

    pub fn secrets_field_names(&self, namespaced: bool) -> (String, String, String) {
        if namespaced {
            (
                "service-username".to_string(),
                "service-password".to_string(),
                "service-url".to_string(),
            )
        } else {
            (
                format!("{}-service-username", self.id),
                format!("{}-service-password", self.id),
                format!("{}-service-url", self.id),
            )
        }
    }

    pub fn secrets_name(&self, service_ns: &str, service_name: &str) -> String {
        format!("{}-{}-service-user", service_ns, service_name)
    }

    pub fn config_name(&self, service_ns: &str, service_name: &str) -> String {
        format!("{}-{}-service-config", service_ns, service_name)
    }

    pub async fn get_secrets_fields(
        &self,
        api: &Api<Secret>,
        secrets_name: &str,
    ) -> (Option<String>, Option<String>, Option<String>) {
        let namespaced = is_secrets_namespaced(secrets_name);
        let (user_field, pw_field, url_field) = self.secrets_field_names(namespaced);
        if let Ok(secret) = api.get(&secrets_name).await {
            secret
                .data
                .and_then(|data| {
                    Some((
                        data.get(&user_field).and_then(bytes_to_string),
                        data.get(&pw_field).and_then(bytes_to_string),
                        data.get(&url_field).and_then(bytes_to_string),
                    ))
                })
                .unwrap_or((None, None, None))
        } else {
            error!("Error getting ServiceUser with name {}", &self.name);
            (None, None, None)
        }
    }

    pub async fn has_secrets_fields(
        &self,
        api: &Api<Secret>,
        secrets_name: &str,
    ) -> (bool, bool, bool) {
        let namespaced = is_secrets_namespaced(secrets_name);
        let (user, pwd, url) = self.get_secrets_fields(api, secrets_name).await;
        if namespaced {
            (user.is_some(), pwd.is_some(), url.is_some())
        } else {
            (true, pwd.is_some(), true)
        }
    }

    pub async fn restore(&self, api: Api<Secret>, secrets_name: &str) -> Option<Self> {
        let (_, pwd, _) = self.get_secrets_fields(&api, secrets_name).await;
        pwd.map(|pwd| Self {
            id: self.id.clone(),
            name: self.name.clone(),
            password: pwd.clone(),
            profile_url: self.profile_url.clone(),
        })
    }

    pub async fn delete_secrets(
        &self,
        api: Api<Secret>,
        service_ns: &str,
        service_name: &str,
    ) -> Result<(), Box<dyn Error>> {
        let secret_name = self.secrets_name(service_ns, service_name);
        if let Some(_) = api.get_opt(&secret_name).await? {
            api.delete(&secret_name, &DeleteParams::default())
                .await?
                .map_left(|_| info!("Deleted secret {} [{}]", secret_name, service_ns))
                .map_right(|s| {
                    info!(
                        "Deleting secret {} [{}]: {}",
                        secret_name, service_ns, s.status
                    )
                });
            Ok(())
        } else {
            warn!(
                "Secret {} [{}] does not exists, could not delete it.",
                secret_name, service_ns
            );
            Ok(())
        }
    }

    pub async fn delete_config(
        &self,
        api: Api<ConfigMap>,
        service_ns: &str,
        service_name: &str,
    ) -> Result<(), Box<dyn Error>> {
        let service_config_fields = ServiceConfigFields::new(&service_ns, &service_name);
        let config_map_name = service_config_fields.configmap_name;
        if let Some(_) = api.get_opt(&config_map_name).await? {
            api.delete(&config_map_name, &DeleteParams::default())
                .await?
                .map_left(|_| info!("Deleted secret {} [{}]", config_map_name, service_ns))
                .map_right(|s| {
                    info!(
                        "Deleting config {} [{}]: {}",
                        config_map_name, service_ns, s.status
                    )
                });
            Ok(())
        } else {
            warn!(
                "Config {} [{}] does not exists, could not delete it.",
                config_map_name, service_ns
            );
            Ok(())
        }
    }

    pub async fn delete_secrets_fields(
        &self,
        api: Api<Secret>,
        secrets_name: &str,
    ) -> Result<(), Box<dyn Error>> {
        let namespaced = is_secrets_namespaced(secrets_name);
        let (user_field, pw_field, url_field) = self.secrets_field_names(namespaced);
        let secret = api.get(secrets_name).await?;
        let mut patches = vec![];
        if let Some(data) = secret.data {
            if data.contains_key(&user_field) {
                info!(
                    "Deleting user field entry for ServiceUser {} in {}",
                    &self.name, secrets_name
                );
                patches.push(Remove(RemoveOperation {
                    path: format!("/data/{}", user_field),
                }));
            }
            if data.contains_key(&pw_field) {
                info!(
                    "Deleting user field entry for ServiceUser {} in {}",
                    &self.name, secrets_name
                );
                patches.push(Remove(RemoveOperation {
                    path: format!("/data/{}", pw_field),
                }));
            }
            if data.contains_key(&url_field) {
                info!(
                    "Deleting user field entry for ServiceUser {} in {}",
                    &self.name, secrets_name
                );
                patches.push(Remove(RemoveOperation {
                    path: format!("/data/{}", url_field),
                }));
            }
            let patch: KubePatch<Secret> = KubePatch::Json(Patch(patches));
            api.patch(&secrets_name, &PatchParams::default(), &patch)
                .await?;
        }
        Ok(())
    }

    pub async fn update_secrets_fields(
        &self,
        api: Api<Secret>,
        secrets_name: &str,
    ) -> Result<(), Box<dyn Error>> {
        let namespaced = is_secrets_namespaced(secrets_name);
        let (user_field, pw_field, url_field) = self.secrets_field_names(namespaced);
        let (user_field_exists, passwd_field_exists, url_field_exists) =
            self.has_secrets_fields(&api, secrets_name).await;
        let mut secret = Secret::default();
        let mut data = BTreeMap::new();
        if !user_field_exists {
            info!(
                "Username entry update for ServiceUser {} is required",
                &self.name
            );
            data.insert(user_field, ByteString(self.name.as_bytes().to_vec()));
        }
        if !passwd_field_exists {
            info!(
                "Password entry update for ServiceUser {} is required",
                &self.name
            );
            data.insert(pw_field, ByteString(self.password.as_bytes().to_vec()));
        }
        if !url_field_exists {
            info!(
                "Client profile url entry update for ServiceUser {} is required",
                &self.name
            );
            data.insert(url_field, ByteString(self.profile_url.as_bytes().to_vec()));
        }
        if data.len() > 0 {
            secret.data = Some(data);
            info!("Creating secrets in K8S for ServiceUer: {}", self.name);
            let patch = KubePatch::Merge(secret);
            api.patch(&secrets_name, &PatchParams::default(), &patch)
                .await?;
        }
        Ok(())
    }

    pub async fn create_secrets(
        &self,
        api: Api<Secret>,
        service_ns: &str,
        service_name: &str,
    ) -> Result<(), Box<dyn Error>> {
        let (user_field, pw_field, url_field) = self.secrets_field_names(true);
        let secret_name = self.secrets_name(service_ns, service_name);
        if let Some(_) = api.get_opt(&secret_name).await? {
            self.update_secrets_fields(api, &secret_name).await
        } else {
            let mut secret = Secret::default();
            secret.data = Some(BTreeMap::from([
                (user_field, ByteString(self.name.as_bytes().to_vec())),
                (pw_field, ByteString(self.password.as_bytes().to_vec())),
                (url_field, ByteString(self.profile_url.as_bytes().to_vec())),
            ]));
            secret.metadata.name = Some(secret_name);
            secret.metadata.namespace = Some(service_ns.to_string());
            api.create(&PostParams::default(), &secret)
                .await
                .map_err(|e| Box::new(e))?;
            Ok(())
        }
    }

    // TODO: Get values from config
    // TODO: Do it in its own struct (same for secrets)
    pub async fn create_config(
        &self,
        api: Api<ConfigMap>,
        service_ns: &str,
        service_name: &str,
    ) -> Result<(), Box<dyn Error>> {
        let service_config_fields = ServiceConfigFields::new(&service_ns, &service_name);
        if let Some(_cm) = api
            .get_opt(&service_config_fields.configmap_name.as_str())
            .await?
        {
            warn!(
                "Found old config for service {} [{}]. Deleting it.",
                service_name, service_ns
            );
            api.delete(
                &service_config_fields.configmap_name,
                &DeleteParams::default(),
            )
            .await?;
        }
        let mut cm = ConfigMap::default();
        cm.metadata.name = Some(service_config_fields.configmap_name);
        cm.data = Some(BTreeMap::from_iter([(
            service_config_fields.log_level.to_string(),
            "INFO".to_string(),
        )]));
        api.create(&PostParams::default(), &cm).await?;
        Ok(())
    }
}

pub fn is_injection_disabled<A: Annotated>(entity: &A) -> bool {
    entity
        .annotation(SDP_INJECTOR_ANNOTATION)
        .map(|v| v.to_lowercase() == "false" || v == "0")
        .unwrap_or(false)
}

pub trait Annotated {
    fn annotations(&self) -> &BTreeMap<String, String>;

    fn annotation(&self, annotation: &str) -> Option<&String> {
        self.annotations().get(annotation)
    }
}

pub trait Validated {
    fn validate(&self) -> Result<(), String>;
}

pub fn generate_service_id(namespace: &str, name: &str, internal: bool) -> String {
    if internal {
        format!("{}_{}", namespace, name)
    } else {
        format!("{}-{}", namespace, name)
    }
}

/// Trait that defines entities that are candidates to be services
/// Basically a service candidate needs to be able to define :
///  - namespace
///  - name
/// and the combination of both needs to be unique
pub trait ServiceCandidate {
    fn name(&self) -> String;
    fn namespace(&self) -> String;
    fn labels(&self) -> HashMap<String, String>;
    fn is_candidate(&self) -> bool;
    fn service_id(&self) -> String {
        generate_service_id(&self.namespace(), &self.name(), false)
    }
    fn service_id_key(&self) -> String {
        generate_service_id(&self.namespace(), &self.name(), true)
    }
}

impl Annotated for Pod {
    fn annotations(&self) -> &BTreeMap<String, String> {
        ResourceExt::annotations(self)
    }
}

impl Annotated for Deployment {
    fn annotations(&self) -> &BTreeMap<String, String> {
        ResourceExt::annotations(self)
    }
}

/// Deployment are the main source of ServiceCandidate
/// Final ServiceIdentity are created from Deployments
impl ServiceCandidate for Deployment {
    fn name(&self) -> String {
        ResourceExt::name_any(self)
    }

    fn namespace(&self) -> String {
        ResourceExt::namespace(self).unwrap_or("default".to_string())
    }

    fn labels(&self) -> HashMap<String, String> {
        let mut labels = HashMap::from_iter(
            ResourceExt::labels(self)
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string())),
        );
        labels.extend([
            ("namespace".to_string(), ServiceCandidate::namespace(self)),
            ("name".to_string(), ServiceCandidate::name(self)),
        ]);
        labels
    }

    fn is_candidate(&self) -> bool {
        Annotated::annotation(self, "sdp-injection")
            .map(|v| v.eq("enabled"))
            .unwrap_or(false)
    }
}

/// Pod are the main source of ServiceCandidate
/// Final ServiceIdentity are created from Pod
impl ServiceCandidate for Pod {
    fn name(&self) -> String {
        let name: Option<String> = self
            .metadata
            .name
            .as_ref()
            .map(|n| (n.split("-").collect::<Vec<&str>>(), 2))
            .or_else(|| {
                let os = self.owner_references();
                (os.len() > 0).then_some((os[0].name.split("-").collect(), 1))
            })
            .or_else(|| {
                self.metadata
                    .generate_name
                    .as_ref()
                    .map(|s| (s.split("-").collect(), 2))
            })
            .map(|(xs, n)| xs[0..(xs.len() - n)].join("-"));
        if let None = name {
            error!("Unable to find service name for Pod, ignoring it");
        }
        // A ServiceCandidate needs always a name
        name.expect("Found POD without service information")
    }

    fn namespace(&self) -> String {
        ResourceExt::namespace(self).unwrap_or("default".to_string())
    }

    fn labels(&self) -> HashMap<String, String> {
        HashMap::default()
    }

    fn is_candidate(&self) -> bool {
        false
    }
}

pub trait HasCredentials {
    fn credentials<'a>(&'a self) -> &'a ServiceUser;
}

impl HasCredentials for ServiceIdentity {
    fn credentials<'a>(&'a self) -> &'a ServiceUser {
        &self.spec.service_user
    }
}
