use kube::{
    core::{admission::AdmissionRequest, object::HasSpec},
    CustomResource, ResourceExt,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

use crate::{
    errors::SDPServiceError,
    kubernetes::{admission_request_name, admission_request_namespace},
    service::{needs_injection, ServiceUser},
    traits::{
        Annotated, Candidate, HasCredentials, Labeled, MaybeNamespaced, MaybeService, Named,
        Namespaced, Service,
    },
};

#[derive(Debug, CustomResource, Serialize, Deserialize, Clone, JsonSchema, PartialEq)]
#[kube(
    group = "injector.sdp.com",
    version = "v1",
    kind = "SDPService",
    namespaced
)]

pub struct SDPServiceSpec {
    pub kind: String,
    pub name: String,
}

impl Named for SDPService {
    fn name(&self) -> String {
        self.spec.name.clone()
    }
}

impl MaybeNamespaced for SDPService {
    fn namespace(&self) -> Option<String> {
        ResourceExt::namespace(self)
    }
}

impl Candidate for SDPService {
    fn is_candidate(&self) -> bool {
        needs_injection(self)
    }
}

impl Annotated for SDPService {
    fn annotations(&self) -> Option<&std::collections::BTreeMap<String, String>> {
        Some(ResourceExt::annotations(self))
    }
}

impl MaybeService for SDPService {}

impl Labeled for SDPService {
    // TODO: Code repeated in the Deployment implementation
    fn labels(&self) -> Result<HashMap<String, String>, SDPServiceError> {
        let mut labels = HashMap::from_iter(
            ResourceExt::labels(self)
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string())),
        );
        let name = Named::name(self);
        MaybeNamespaced::namespace(self)
            .map(|ns| {
                labels.extend([
                    ("namespace".to_string(), ns),
                    ("name".to_string(), name.clone()),
                ]);
                labels
            })
            .ok_or(SDPServiceError::from_string(format!(
                "Unable to find namespace for Deployment {}",
                &name
            )))
    }
}

impl Named for AdmissionRequest<SDPService> {
    fn name(&self) -> String {
        admission_request_name(self)
    }
}

impl MaybeNamespaced for AdmissionRequest<SDPService> {
    fn namespace(&self) -> Option<String> {
        admission_request_namespace(self)
    }
}

impl MaybeService for AdmissionRequest<SDPService> {}

impl Candidate for AdmissionRequest<SDPService> {
    fn is_candidate(&self) -> bool {
        true
    }
}

/// ServiceIdentity CRD
/// This is the CRD where we store the credentials for the services
#[derive(Debug, CustomResource, Serialize, Deserialize, Clone, JsonSchema, PartialEq)]
#[kube(
    group = "injector.sdp.com",
    version = "v1",
    kind = "ServiceIdentity",
    namespaced
)]

/// Spec for ServiceIdentity CRD
/// This CRD defines the credentials and the labels used by a specific k8s service
/// The credentials are stored in a k8s secret entity
/// The labels in the service are used to determine what kind of access the service will have
/// service_namespace + service_name identify each service
pub struct ServiceIdentitySpec {
    pub service_user: ServiceUser,
    pub service_name: String,
    pub service_namespace: String,
    pub labels: HashMap<String, String>,
    pub disabled: bool,
    pub device_ids: Vec<String>,
}

impl Named for ServiceIdentity {
    fn name(&self) -> String {
        self.spec().service_name.clone()
    }
}

impl Namespaced for ServiceIdentity {
    fn namespace(&self) -> String {
        self.spec().service_namespace.clone()
    }
}

impl HasCredentials for ServiceIdentity {
    fn credentials<'a>(&'a self) -> &'a ServiceUser {
        &self.spec.service_user
    }
}
impl Service for ServiceIdentity {}
