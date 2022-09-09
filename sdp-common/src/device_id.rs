use std::collections::HashMap;

pub use crate::crd::{DeviceId, ServiceIdentity};
use crate::service::ServiceCandidate;
use kube::{core::object::HasSpec};

impl ServiceCandidate for ServiceIdentity {
    fn name(&self) -> String {
        self.spec().service_name.clone()
    }
    fn namespace(&self) -> String {
        self.spec().service_namespace.clone()
    }

    fn labels(&self) -> std::collections::HashMap<String, String> {
        HashMap::new()
    }

    fn is_candidate(&self) -> bool {
        false
    }
}

impl ServiceCandidate for DeviceId {
    fn name(&self) -> String {
        self.spec().service_name.clone()
    }

    fn namespace(&self) -> String {
        self.spec().service_namespace.clone()
    }

    fn labels(&self) -> HashMap<String, String> {
        HashMap::new()
    }

    fn is_candidate(&self) -> bool {
        false
    }
}
