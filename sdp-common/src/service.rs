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
use kube::api::{Patch as KubePatch, PatchParams};
use kube::{Api, ResourceExt};
use log::{error, info};
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

    pub async fn delete(&self, api: Api<Secret>) -> Result<(), Box<dyn Error>> {
        let (user_field, pw_field, url_field) = self.field_names();
        let (user_field_exists, passwd_field_exists, url_field_exists) =
            self.has_fields(&api).await;
        let mut patch_operations: Vec<PatchOperation> = Vec::new();
        if user_field_exists {
            patch_operations.push(PatchOperation::Remove(RemoveOperation {
                path: format!("/data/{}", user_field),
            }));
        } else {
            info!(
                "User field in ServiceUser {} not found, ignoring it",
                &self.name
            );
        }
        if passwd_field_exists {
            patch_operations.push(PatchOperation::Remove(RemoveOperation {
                path: format!("/data/{}", pw_field),
            }));
        } else {
            info!(
                "Password field in ServiceUser {} not found, ignoring it",
                &self.name
            );
        }
        if url_field_exists {
            patch_operations.push(PatchOperation::Remove(RemoveOperation {
                path: format!("/data/{}", url_field),
            }));
        } else {
            info!(
                "Client profile url in ServiceUser {} not found, ignoring it",
                &self.name
            );
        }
        if patch_operations.len() > 0 {
            info!("Deleting ServiceUser {} from K8S secret", &self.name);
            let patch: KubePatch<Secret> = KubePatch::Json(json_patch::Patch(patch_operations));
            api.patch(
                SDP_IDENTITY_MANAGER_SECRETS,
                &PatchParams::default(),
                &patch,
            )
            .await?;
        }
        Ok(())
    }

    pub async fn create(&self, api: Api<Secret>) -> Result<(), Box<dyn Error>> {
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
            api.patch(
                SDP_IDENTITY_MANAGER_SECRETS,
                &PatchParams::default(),
                &patch,
            )
            .await?;
        }
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
