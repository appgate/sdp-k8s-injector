use crate::annotations::{SDP_INJECTOR_ANNOTATION_ENABLED, SDP_INJECTOR_ANNOTATION_STRATEGY};
use crate::constants::{
    IDENTITY_MANAGER_SECRET_NAME, SDP_LOG_CONFIG_FILE, SDP_LOG_CONFIG_FILE_ENV,
};
pub use crate::crd::ServiceIdentity;
use crate::errors::SDPServiceError;
use crate::sdp::{auth::SDPUser, system::ClientProfileUrl};
use crate::traits::{Annotated, Labeled, MaybeNamespaced, MaybeService, Named};
use json_patch::PatchOperation::Remove;
use json_patch::{Patch, RemoveOperation};
use k8s_openapi::api::core::v1::{ConfigMap, Container, Pod, Volume};
use k8s_openapi::{api::core::v1::Secret, ByteString};
use kube::api::{DeleteParams, Patch as KubePatch, PatchParams, PostParams};
use kube::Api;
use schemars::JsonSchema;
use sdp_macros::{logger, sdp_error, sdp_info, sdp_log, sdp_warn, with_dollar_sign};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::{collections::BTreeMap, error::Error};

logger!("ServiceIdentityProvider");

#[derive(PartialEq, Debug)]
pub enum SDPInjectionStrategy {
    EnabledByDefault,
    DisabledByDefault,
}

impl ToString for SDPInjectionStrategy {
    fn to_string(&self) -> String {
        match self {
            SDPInjectionStrategy::EnabledByDefault => "enabledByDefault".to_string(),
            SDPInjectionStrategy::DisabledByDefault => "disabledByDefault".to_string(),
        }
    }
}

pub struct ServiceLookup {
    name: String,
    namespace: String,
    labels: Option<HashMap<String, String>>,
}

impl ServiceLookup {
    pub fn new(namespace: &str, name: &str, labels: Option<HashMap<String, String>>) -> Self {
        ServiceLookup {
            name: name.to_string(),
            namespace: namespace.to_string(),
            labels,
        }
    }

    pub fn try_from_service(
        f: &(impl Named + MaybeNamespaced + Labeled),
    ) -> Result<Self, SDPServiceError> {
        match (f.namespace(), f.labels()) {
            (Some(ns), labels) => Ok(Self::new(
                &ns,
                &f.name(),
                Some(labels.unwrap_or(HashMap::default())),
            )),
            _ => Err(SDPServiceError::from_string(format!(
                "Unable to get namespace from f {}",
                f.name()
            ))),
        }
    }
}

impl Named for ServiceLookup {
    fn name(&self) -> String {
        self.name.clone()
    }
}

impl MaybeNamespaced for ServiceLookup {
    fn namespace(&self) -> Option<String> {
        Some(self.namespace.clone())
    }
}

impl MaybeService for ServiceLookup {}

impl Labeled for ServiceLookup {
    fn labels(
        &self,
    ) -> Result<std::collections::HashMap<String, String>, crate::errors::SDPServiceError> {
        Ok(self
            .labels
            .as_ref()
            .map(|labels| labels.clone())
            .unwrap_or(HashMap::default()))
    }
}

struct ServiceConfigFields {
    configmap_name: String,
    log_level: String,
}

impl ServiceConfigFields {
    fn new(service_ns: &str, service_name: &str) -> Self {
        ServiceConfigFields {
            configmap_name: format!("{}-{}-service-config", service_ns, service_name),
            log_level: "appgate-loglevel".to_string(),
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
                .map_left(|_| {
                    info!(
                        "[{}] Deleted secret {}",
                        format!("{}_{}", service_ns, service_name),
                        secret_name
                    );
                })
                .map_right(|s| {
                    info!(
                        "[{}] Deleting secret {}: {}",
                        format!("{}_{}", service_ns, service_name),
                        secret_name,
                        s.status
                    );
                });
            Ok(())
        } else {
            warn!(
                "[{}] Unable to find Secret {} to delete it",
                format!("{}_{}", service_ns, service_name),
                secret_name
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
                .map_left(|_| {
                    info!(
                        "[{}] Deleted secret {} [{}]",
                        format!("{}_{}", service_ns, service_name),
                        config_map_name,
                        service_ns
                    );
                })
                .map_right(|s| {
                    info!(
                        "[{}] Deleting config {} [{}]: {}",
                        format!("{}_{}", service_ns, service_name),
                        config_map_name,
                        service_ns,
                        s.status
                    );
                });
            Ok(())
        } else {
            warn!(
                "[{}] Unable to find Config {} to delete it.",
                format!("{}_{}", service_ns, service_name),
                config_map_name
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
                "[{}] Found old config. Deleting it.",
                format!("{}_{}", service_name, service_ns)
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

pub fn injection_strategy<A: Annotated>(entity: &A) -> SDPInjectionStrategy {
    entity
        .annotation(SDP_INJECTOR_ANNOTATION_STRATEGY)
        .and_then(|s| match s {
            v if v.to_lowercase() == "enabledbydefault" => {
                Some(SDPInjectionStrategy::EnabledByDefault)
            }
            v if v.to_lowercase() == "disabledbydefault" => {
                Some(SDPInjectionStrategy::DisabledByDefault)
            }
            _ => None,
        })
        .unwrap_or_else(|| {
            let ann = entity.annotation(SDP_INJECTOR_ANNOTATION_ENABLED);
            if ann.is_some() && ann.unwrap().to_lowercase() == "disabled" {
                SDPInjectionStrategy::DisabledByDefault
            } else {
                SDPInjectionStrategy::EnabledByDefault
            }
        })
}

pub fn is_injection_disabled<A: Annotated>(entity: &A) -> bool {
    entity
        .annotation(SDP_INJECTOR_ANNOTATION_ENABLED)
        .map(|v| v.to_lowercase() == "false" || v == "0")
        .unwrap_or(false)
}

pub fn is_injection_enabled<A: Annotated>(entity: &A) -> bool {
    entity
        .annotation(SDP_INJECTOR_ANNOTATION_ENABLED)
        .map(|v| v.to_lowercase() == "true" || v == "1")
        .unwrap_or(false)
}

pub fn needs_injection<A: Annotated>(entity: &A) -> bool {
    match injection_strategy(entity) {
        SDPInjectionStrategy::EnabledByDefault => !is_injection_disabled(entity),
        SDPInjectionStrategy::DisabledByDefault => is_injection_enabled(entity),
    }
}

pub fn get_service_username(cluster_name: &str, service_ns: &str, service_name: &str) -> String {
    format!("{}_{}_{}", cluster_name, service_ns, service_name)
}

pub fn get_profile_client_url_name(cluster_name: &str) -> String {
    format!("{}_k8s-service", cluster_name)
}

pub fn containers(pod: &Pod) -> Option<&Vec<Container>> {
    pod.spec.as_ref().map(|s| &s.containers)
}

pub fn init_containers(pod: &Pod) -> Option<&Vec<Container>> {
    pod.spec
        .as_ref()
        .and_then(|s| s.init_containers.as_ref())
        .and_then(|xs| (xs.len() > 0).then_some(xs))
}

pub fn volumes(pod: &Pod) -> Option<&Vec<Volume>> {
    pod.spec.as_ref().and_then(|s| s.volumes.as_ref())
}

pub fn volume_names(pod: &Pod) -> Option<Vec<String>> {
    volumes(pod).map(|vs| vs.iter().map(|v| v.name.clone()).collect())
}

pub fn get_log_config_path() -> String {
    std::env::var(SDP_LOG_CONFIG_FILE_ENV).unwrap_or(SDP_LOG_CONFIG_FILE.to_string())
}

#[cfg(test)]
mod tests {
    use k8s_openapi::api::core::v1::Pod;
    use sdp_test_macros::{pod, set_pod_field};
    use std::collections::BTreeMap;

    use crate::service::{
        needs_injection, SDPInjectionStrategy, SDP_INJECTOR_ANNOTATION_ENABLED,
        SDP_INJECTOR_ANNOTATION_STRATEGY,
    };

    use super::injection_strategy;

    #[test]
    fn test_sdp_injection_strategy_0() {
        assert_eq!(
            injection_strategy(&pod!(0)),
            SDPInjectionStrategy::EnabledByDefault
        );
        assert_eq!(
            injection_strategy(
                &pod!(0, annotations => vec![("SDP_INJECTOR_ANNOTATION_STRATEGY", "")])
            ),
            SDPInjectionStrategy::EnabledByDefault
        );
        assert_eq!(
            injection_strategy(&pod!(0, annotations => vec![(
            SDP_INJECTOR_ANNOTATION_STRATEGY, "enabledByDefault")
            ])),
            SDPInjectionStrategy::EnabledByDefault
        );
        assert_eq!(
            injection_strategy(&pod!(0, annotations => vec![(
            SDP_INJECTOR_ANNOTATION_STRATEGY, "enabledbydefault")
            ])),
            SDPInjectionStrategy::EnabledByDefault
        );
        assert_eq!(
            injection_strategy(&pod!(0, annotations => vec![(
            SDP_INJECTOR_ANNOTATION_STRATEGY, "enabled-by-default")
            ])),
            SDPInjectionStrategy::EnabledByDefault
        );
        assert_eq!(
            injection_strategy(&pod!(0, annotations => vec![(
            SDP_INJECTOR_ANNOTATION_STRATEGY, "disabledByDefault")
            ])),
            SDPInjectionStrategy::DisabledByDefault
        );
        assert_eq!(
            injection_strategy(&pod!(0, annotations => vec![(
            SDP_INJECTOR_ANNOTATION_STRATEGY, "disabledbydefault")
            ])),
            SDPInjectionStrategy::DisabledByDefault
        );
        assert_eq!(
            injection_strategy(&pod!(0, annotations => vec![(
            SDP_INJECTOR_ANNOTATION_STRATEGY, "disabled-by-default")
            ])),
            SDPInjectionStrategy::EnabledByDefault
        );
    }

    #[test]
    fn test_sdp_injection_enabled() {
        assert!(needs_injection(&pod!(0)));
        assert!(needs_injection(&pod!(0, annotations => vec![
            (SDP_INJECTOR_ANNOTATION_STRATEGY, ""),
        ])));
        assert!(needs_injection(&pod!(0, annotations => vec![
            (SDP_INJECTOR_ANNOTATION_STRATEGY, "enabledByDefault"),
        ])));
        assert!(needs_injection(&pod!(0, annotations => vec![
            (SDP_INJECTOR_ANNOTATION_STRATEGY, "enabledByDefault"),
            (SDP_INJECTOR_ANNOTATION_ENABLED, "true")
        ])));
        assert!(!needs_injection(&pod!(0, annotations => vec![
            (SDP_INJECTOR_ANNOTATION_ENABLED, "false")
        ])));
        assert!(!needs_injection(&pod!(0, annotations => vec![
            (SDP_INJECTOR_ANNOTATION_STRATEGY, ""),
            (SDP_INJECTOR_ANNOTATION_ENABLED, "false")
        ])));
        assert!(!needs_injection(&pod!(0, annotations => vec![
            (SDP_INJECTOR_ANNOTATION_STRATEGY, "enabledByDefault"),
            (SDP_INJECTOR_ANNOTATION_ENABLED, "false")
        ])));
        assert!(needs_injection(&pod!(0, annotations => vec![
            (SDP_INJECTOR_ANNOTATION_STRATEGY, "disabledByDefault"),
            (SDP_INJECTOR_ANNOTATION_ENABLED, "true")
        ])));
        assert!(!needs_injection(&pod!(0, annotations => vec![
            (SDP_INJECTOR_ANNOTATION_STRATEGY, "disabledByDefault"),
        ])));
        assert!(!needs_injection(&pod!(0, annotations => vec![
            (SDP_INJECTOR_ANNOTATION_STRATEGY, "disabledByDefault"),
            (SDP_INJECTOR_ANNOTATION_ENABLED, "false")
        ])));
    }
}
