use kube::{core::object::HasSpec, CustomResource, ResourceExt};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

use crate::{
    service::{needs_injection, ServiceUser},
    traits::{
        Annotated, Candidate, HasCredentials, MaybeNamespaced, MaybeService, Named, Namespaced,
        Service,
    },
};

#[derive(Debug, CustomResource, Serialize, Deserialize, Clone, JsonSchema, PartialEq)]
#[kube(
    group = "injector.sdp.com",
    version = "v1",
    kind = "SDPService",
    namespaced
)]

pub struct SDPServiceSpec {}

impl Named for SDPService {
    fn name(&self) -> String {
        self.name_any()
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

/// DeviceId
/// DeviceId represents the UUIDs assigned to a ServiceIdentity. There are N uuids stored
/// in the DeviceId where N equals to the number of replicas in a replicaset. The
/// combination of the service_name and service_namespace is unique to the cluster.
#[derive(Debug, CustomResource, Serialize, Deserialize, Clone, JsonSchema, PartialEq)]
#[kube(
    group = "injector.sdp.com",
    version = "v1",
    kind = "DeviceId",
    namespaced
)]
pub struct DeviceIdSpec {
    /// List of UUIDs
    pub uuids: Vec<String>,
    /// Name of the deployment that this is assigned to
    pub service_name: String,
    /// Namespace of the deployment that this is assigned to
    pub service_namespace: String,
}

impl Named for DeviceId {
    fn name(&self) -> String {
        self.spec().service_name.clone()
    }
}

impl Namespaced for DeviceId {
    fn namespace(&self) -> String {
        self.spec().service_namespace.clone()
    }
}

impl Service for DeviceId {}
