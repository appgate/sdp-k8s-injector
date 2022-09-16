use crate::constants::SDP_IDENTITY_MANAGER_SECRETS;
pub use crate::crd::ServiceIdentity;
use json_patch::{PatchOperation, RemoveOperation};
use k8s_openapi::{
    api::{
        apps::v1::Deployment,
        core::v1::{Pod, Secret},
    },
    ByteString,
};
use kube::api::{DeleteParams, Patch as KubePatch, PatchParams, PostParams};
use kube::{Api, ResourceExt};
use log::{error, info, warn};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, HashMap},
    error::Error,
};

pub const SDP_INJECTOR_ANNOTATION: &str = "sdp-injector";

#[derive(Clone, JsonSchema, Debug, Serialize, Deserialize, PartialEq)]
pub struct ServiceUser {
    pub name: String,
    pub password: String,
    pub profile_url: String,
}

impl ServiceUser {
    pub fn field_names(&self) -> (String, String, String) {
        (
            format!("{}-user", self.name),
            format!("{}-pw", self.name),
            format!("{}-url", self.name),
        )
    }

    pub async fn has_fields(&self, api: &Api<Secret>) -> (bool, bool, bool) {
        let (user_field, pw_field, url_field) = self.field_names();
        if let Ok(secret) = api.get(SDP_IDENTITY_MANAGER_SECRETS).await {
            secret
                .data
                .map(|data| {
                    (
                        data.get(&pw_field).is_some(),
                        data.get(&user_field).is_some(),
                        data.get(&url_field).is_some(),
                    )
                })
                .unwrap_or((false, false, false))
        } else {
            error!("Error getting ServiceUser with name {}", &self.name);
            (false, false, false)
        }
    }

    pub async fn delete(
        &self,
        api: Api<Secret>,
        service_ns: &str,
        service_name: &str,
    ) -> Result<(), Box<dyn Error>> {
        let secret_name = format!("{}-{}", service_ns, service_name);
        if let Some(_) = api.get_opt(&secret_name).await? {
            api.delete(&secret_name, &DeleteParams::default())
                .await?
                .map_left(|_| println!("Deleted secret {} [{}]", secret_name, service_ns))
                .map_right(|s| {
                    println!(
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

    pub async fn patch(&self, api: Api<Secret>, secret_name: &str) -> Result<(), Box<dyn Error>> {
        let (user_field, pw_field, url_field) = self.field_names();
        let (user_field_exists, passwd_field_exists, url_field_exists) =
            self.has_fields(&api).await;
        let mut secret = Secret::default();
        let mut data = BTreeMap::new();
        if !user_field_exists {
            info!("Create user entry for ServiceUser {}", &self.name);
            data.insert(user_field, ByteString(self.name.as_bytes().to_vec()));
        }
        if !passwd_field_exists {
            info!("Create password entry for ServiceUser {}", &self.name);
            data.insert(pw_field, ByteString(self.password.as_bytes().to_vec()));
        }
        if !url_field_exists {
            info!(
                "Create client profile url entry for ServiceUser {}",
                &self.name
            );
            data.insert(url_field, ByteString(self.profile_url.as_bytes().to_vec()));
        }
        if data.len() > 0 {
            secret.data = Some(data);
            info!("Creating secrets in K8S for ServiceUer: {}", self.name);
            let patch = KubePatch::Merge(secret);
            api.patch(secret_name, &PatchParams::default(), &patch)
                .await?;
        }
        Ok(())
    }

    pub async fn create(
        &self,
        api: Api<Secret>,
        service_ns: &str,
        service_name: &str,
    ) -> Result<(), Box<dyn Error>> {
        let (user_field, pw_field, url_field) = self.field_names();
        let secret_name = format!("{}-{}", service_ns, service_name);
        if let Some(_) = api.get_opt(&secret_name).await? {
            self.patch(api, &secret_name).await
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
        format!("{}-{}", self.namespace(), self.name()).to_string()
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
        HashMap::from([
            ("namespace".to_string(), ServiceCandidate::namespace(self)),
            ("name".to_string(), ServiceCandidate::name(self)),
        ])
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
        &self.spec.service_credentials
    }
}
