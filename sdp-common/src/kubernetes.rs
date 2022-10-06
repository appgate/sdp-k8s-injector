use std::collections::{BTreeMap, HashMap};

use http::Uri;
use k8s_openapi::{
    api::{apps::v1::Deployment, core::v1::Pod},
    Metadata,
};
use kube::{core::admission::AdmissionRequest, Client, Config, Resource, ResourceExt};
use log::error;

use crate::{
    errors::SDPServiceError,
    traits::{Annotated, Candidate, Labelled, Named, Namespaced, ObjectRequest, Service},
};

pub const SDP_K8S_HOST_ENV: &str = "SDP_K8S_HOST";
pub const SDP_K8S_HOST_DEFAULT: &str = "kubernetes.default.svc";
pub const SDP_K8S_NO_VERIFY_ENV: &str = "SDP_K8S_NO_VERIFY";
pub const SDP_K8S_NAMESPACE: &str = "sdp-system";

pub async fn get_k8s_client() -> Client {
    let mut k8s_host = String::from("https://");
    k8s_host.push_str(&std::env::var(SDP_K8S_HOST_ENV).unwrap_or(SDP_K8S_HOST_DEFAULT.to_string()));
    let k8s_uri = k8s_host
        .parse::<Uri>()
        .expect("Unable to parse SDP_K8S_HOST value:");
    let mut k8s_config = Config::infer()
        .await
        .expect("Unable to infer K8S configuration");
    k8s_config.cluster_url = k8s_uri;
    k8s_config.accept_invalid_certs = std::env::var(SDP_K8S_NO_VERIFY_ENV)
        .map(|v| v == "1" || v.to_lowercase() == "true")
        .unwrap_or(false);
    Client::try_from(k8s_config).expect("Unable to create k8s client")
}

// Implement required traits for Pod

impl Named for Pod {
    fn name(&self) -> String {
        let name = self
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
        if let Some(name) = name {
            name
        } else {
            error!("Unable to find service name for Pod, generating a random one!");
            uuid::Uuid::new_v4().to_string()
        }
    }
}

impl Namespaced for Pod {
    fn namespace(&self) -> Option<String> {
        if let Some(ns) = ResourceExt::namespace(self) {
            Some(ns.clone())
        } else {
            error!("Unable to find service namespace for POD inside admission request, generating a random one!");
            Some(uuid::Uuid::new_v4().to_string())
        }
    }
}

impl Labelled for Pod {
    fn labels(&self) -> Result<HashMap<String, String>, SDPServiceError> {
        Ok(HashMap::default())
    }
}

impl Candidate for Pod {
    fn is_candidate(&self) -> bool {
        Annotated::annotation(self, "sdp-injection")
            .map(|v| v.eq("enabled"))
            .unwrap_or(false)
    }
}

impl Service for Pod {}

// Implement required traits for Deployment

impl Named for Deployment {
    fn name(&self) -> String {
        self.name_any()
    }
}

impl Namespaced for Deployment {
    fn namespace(&self) -> Option<String> {
        ResourceExt::namespace(self)
    }
}

impl Service for Deployment {}

impl Annotated for Pod {
    fn annotations(&self) -> Option<&BTreeMap<String, String>> {
        Some(ResourceExt::annotations(self))
    }
}

impl Candidate for Deployment {
    fn is_candidate(&self) -> bool {
        Annotated::annotation(self, "sdp-injection")
            .map(|v| v.eq("enabled"))
            .unwrap_or(false)
    }
}

impl Labelled for Deployment {
    fn labels(&self) -> Result<HashMap<String, String>, SDPServiceError> {
        let mut labels = HashMap::from_iter(
            ResourceExt::labels(self)
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string())),
        );
        labels.extend([
            ("namespace".to_string(), self.service_name()?),
            ("name".to_string(), self.service_name()?),
        ]);
        Ok(labels)
    }
}

impl Annotated for Deployment {
    fn annotations(&self) -> Option<&BTreeMap<String, String>> {
        Some(ResourceExt::annotations(self))
    }
}

// Implement required traits for AdmissionRequest<Pod>

impl ObjectRequest<Pod> for AdmissionRequest<Pod> {
    fn object(&self) -> Option<&Pod> {
        self.object.as_ref()
    }
}

impl Annotated for AdmissionRequest<Pod> {
    fn annotations(&self) -> Option<&BTreeMap<String, String>> {
        self.object.as_ref().and_then(|p| Annotated::annotations(p))
    }
}

fn admission_request_name<E: Resource + Metadata>(
    admisison_request: &AdmissionRequest<E>,
) -> String {
    let mut name = None;
    if let Some(obj) = admisison_request.object.as_ref() {
        name = obj
            .meta()
            .name
            .as_ref()
            .map(|n| (n.split("-").collect::<Vec<&str>>(), 2))
            .or_else(|| {
                let os = obj.owner_references();
                (os.len() > 0).then_some((os[0].name.split("-").collect(), 1))
            })
            .or_else(|| {
                obj.meta()
                    .generate_name
                    .as_ref()
                    .map(|s| (s.split("-").collect(), 2))
            })
            .map(|(xs, n)| xs[0..(xs.len() - n)].join("-"));
    } else {
        error!("Unable to find object inside admission request, ignoring it");
    }
    if let Some(name) = name {
        name
    } else {
        error!("Unable to find service name for object, generating a random one!");
        uuid::Uuid::new_v4().to_string()
    }
}

fn admission_request_namespace<E: Resource + Metadata>(
    admission_request: &AdmissionRequest<E>,
) -> Option<String> {
    let ns = match (
        admission_request.namespace.as_ref(),
        admission_request
            .object
            .as_ref()
            .and_then(|r| r.meta().namespace.as_ref()),
    ) {
        (_, Some(ns)) => Some(ns),
        (Some(ns), None) => Some(ns),
        _ => None,
    };
    if let Some(ns) = ns {
        Some(ns.clone())
    } else {
        error!("Unable to find service namespace for object inside admission request, generating a random one!");
        Some(uuid::Uuid::new_v4().to_string())
    }
}

impl Named for AdmissionRequest<Pod> {
    fn name(&self) -> String {
        admission_request_name(self)
    }
}

impl Namespaced for AdmissionRequest<Pod> {
    fn namespace(&self) -> Option<String> {
        admission_request_namespace(self)
    }
}

impl Service for AdmissionRequest<Pod> {}

// Implement required traits for AdmissionRequest<Deployment>

impl Named for AdmissionRequest<Deployment> {
    fn name(&self) -> String {
        admission_request_name(self)
    }
}

impl Namespaced for AdmissionRequest<Deployment> {
    fn namespace(&self) -> Option<String> {
        admission_request_namespace(self)
    }
}

impl Service for AdmissionRequest<Deployment> {}
