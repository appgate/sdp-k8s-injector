pub use crate::crd::ServiceIdentity;
use k8s_openapi::api::apps::v1::Deployment;
use kube::ResourceExt;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Clone, JsonSchema, Debug, Serialize, Deserialize, PartialEq)]
pub struct ServiceCredentialsRef {
    pub id: String,
    pub name: String,
    pub secret: String,
    pub user_field: String,
    pub password_field: String,
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

/// ServiceIdentity is a ServiceCandidate by definition :D
impl ServiceCandidate for ServiceIdentity {
    fn name(&self) -> String {
        self.spec.service_name.clone()
    }

    fn namespace(&self) -> String {
        self.spec.service_namespace.clone()
    }

    fn labels(&self) -> HashMap<String, String> {
        self.spec.labels.clone()
    }

    fn is_candidate(&self) -> bool {
        true
    }
}

/// Deployment are the main source of ServiceCandidate
/// Final ServiceIdentity are created from Deployments
impl ServiceCandidate for Deployment {
    fn name(&self) -> String {
        ResourceExt::name(self)
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
        ResourceExt::namespace(self) == Some("purple-devops".to_string())
        //self.annotations().get("sdp-injector").map(|v| v.eq("true")).unwrap_or(false)
    }
}

pub trait HasCredentials {
    fn credentials<'a>(&'a self) -> &'a ServiceCredentialsRef;
}

impl HasCredentials for ServiceIdentity {
    fn credentials<'a>(&'a self) -> &'a ServiceCredentialsRef {
        &self.spec.service_credentials
    }
}
