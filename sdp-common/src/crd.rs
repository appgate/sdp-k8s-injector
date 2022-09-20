use kube::CustomResource;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

use crate::service::ServiceUser;

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
