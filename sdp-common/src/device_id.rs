pub use crate::crd::{DeviceId, ServiceIdentity};
use kube::ResourceExt;
use crate::kubernetes::SDP_K8S_NAMESPACE;

pub trait DeviceIdCandidate {
    fn name(&self) -> String;
    fn namespace(&self) -> String;
    fn service_identity_id(&self) -> String {
        format!("{}-{}", self.namespace(), self.name()).to_string()
    }
}

impl DeviceIdCandidate for ServiceIdentity {
    fn name(&self) -> String {
        ResourceExt::name(self)
    }
    fn namespace(&self) -> String {
        ResourceExt::namespace(self).unwrap_or("default".to_string())
    }
}

impl DeviceIdCandidate for DeviceId {
    fn name(&self) -> String {
        ResourceExt::name(self)
    }

    fn namespace(&self) -> String {
        ResourceExt::namespace(self).unwrap_or(SDP_K8S_NAMESPACE.to_string())
    }
}
