pub use crate::service::ServiceCredentialsRef;
use kube::CustomResource;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

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
    pub service_credentials: ServiceCredentialsRef,
    pub service_name: String,
    pub service_namespace: String,
    pub labels: HashMap<String, String>,
    pub disabled: bool,
}

/// DeviceId
/// DeviceId represents the list of device ids assigned to a ServiceIdentity
/// There is N uuid stored in the DeviceId where N == number of pods in a deployment/replicaset
#[derive(Debug, CustomResource, Serialize, Deserialize, Clone, JsonSchema, PartialEq)]
#[kube(
    group = "injector.sdp.com",
    version = "v1",
    kind = "DeviceId",
    namespaced
)]
pub struct DeviceIdSpec {
    /// List of uuid assigned to a ServiceIdentity
    pub uuids: Vec<String>,
    pub service_name: String,
    pub service_namespace: String,
}
