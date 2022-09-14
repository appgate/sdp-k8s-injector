pub use crate::crd::ServiceIdentity;
use k8s_openapi::api::{apps::v1::Deployment, core::v1::Pod};
use kube::ResourceExt;
use log::error;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};

pub const SDP_INJECTOR_ANNOTATION: &str = "sdp-injector";

#[derive(Clone, JsonSchema, Debug, Serialize, Deserialize, PartialEq)]
pub struct ServiceUser {
    pub name: String,
    pub password: String,
    pub profile_url: String,
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
