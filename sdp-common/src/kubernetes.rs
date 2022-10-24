use std::collections::{BTreeMap, HashMap};

use http::Uri;
use k8s_openapi::{
    api::{
        apps::v1::Deployment,
        core::v1::{Namespace, Pod},
    },
    Metadata,
};
use kube::{core::admission::AdmissionRequest, Client, Config, Resource, ResourceExt};
use log::error;

use crate::{
    errors::SDPServiceError,
    service::needs_injection,
    traits::{Annotated, Candidate, Labeled, MaybeNamespaced, MaybeService, Named, ObjectRequest},
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

impl MaybeNamespaced for Pod {
    fn namespace(&self) -> Option<String> {
        if let Some(ns) = ResourceExt::namespace(self) {
            Some(ns.clone())
        } else {
            error!("Unable to find service namespace for POD inside admission request, generating a random one!");
            Some(uuid::Uuid::new_v4().to_string())
        }
    }
}

impl Labeled for Pod {
    fn labels(&self) -> Result<HashMap<String, String>, SDPServiceError> {
        Ok(HashMap::default())
    }
}

impl Candidate for Pod {
    fn is_candidate(&self) -> bool {
        needs_injection(self)
    }
}

impl MaybeService for Pod {}

// Implement required traits for Deployment

impl Named for Deployment {
    fn name(&self) -> String {
        self.name_any()
    }
}

impl MaybeNamespaced for Deployment {
    fn namespace(&self) -> Option<String> {
        ResourceExt::namespace(self)
    }
}

impl MaybeService for Deployment {}

impl Annotated for Pod {
    fn annotations(&self) -> Option<&BTreeMap<String, String>> {
        Some(ResourceExt::annotations(self))
    }
}

impl Candidate for Deployment {
    fn is_candidate(&self) -> bool {
        needs_injection(self)
    }
}

impl Labeled for Deployment {
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

impl MaybeNamespaced for AdmissionRequest<Pod> {
    fn namespace(&self) -> Option<String> {
        admission_request_namespace(self)
    }
}

impl MaybeService for AdmissionRequest<Pod> {}

impl Candidate for AdmissionRequest<Pod> {
    fn is_candidate(&self) -> bool {
        self.object().map(|pod| pod.is_candidate()).unwrap_or(false)
    }
}

// Implement required traits for AdmissionRequest<Deployment>
impl ObjectRequest<Deployment> for AdmissionRequest<Deployment> {
    fn object(&self) -> Option<&Deployment> {
        self.object.as_ref()
    }
}

impl Named for AdmissionRequest<Deployment> {
    fn name(&self) -> String {
        admission_request_name(self)
    }
}

impl MaybeNamespaced for AdmissionRequest<Deployment> {
    fn namespace(&self) -> Option<String> {
        admission_request_namespace(self)
    }
}

impl MaybeService for AdmissionRequest<Deployment> {}

impl Candidate for AdmissionRequest<Deployment> {
    fn is_candidate(&self) -> bool {
        true
    }
}

impl Annotated for Namespace {
    fn annotations(&self) -> Option<&BTreeMap<String, String>> {
        Some(ResourceExt::annotations(self))
    }
}

#[cfg(test)]
mod test {
    use k8s_openapi::api::core::v1::Pod;
    use sdp_test_macros::{pod, set_pod_field};
    use std::collections::BTreeMap;

    use crate::{
        annotations::{SDP_INJECTOR_ANNOTATION_ENABLED, SDP_INJECTOR_ANNOTATION_STRATEGY},
        traits::Candidate,
    };

    #[test]
    fn test_sdp_injection_enabled() {
        assert!((&pod!(0).is_candidate()));
        assert!(&pod!(0, annotations => vec![
            (SDP_INJECTOR_ANNOTATION_STRATEGY, ""),
        ])
        .is_candidate());
        assert!(&pod!(0, annotations => vec![
            (SDP_INJECTOR_ANNOTATION_STRATEGY, "enabledByDefault"),
        ])
        .is_candidate());
        assert!(&pod!(0, annotations => vec![
            (SDP_INJECTOR_ANNOTATION_STRATEGY, "enabledByDefault"),
            (SDP_INJECTOR_ANNOTATION_ENABLED, "true")
        ])
        .is_candidate());
        assert!(!&pod!(0, annotations => vec![
            (SDP_INJECTOR_ANNOTATION_ENABLED, "false")
        ])
        .is_candidate());
        assert!(!&pod!(0, annotations => vec![
            (SDP_INJECTOR_ANNOTATION_STRATEGY, ""),
            (SDP_INJECTOR_ANNOTATION_ENABLED, "false")
        ])
        .is_candidate());
        assert!(!&pod!(0, annotations => vec![
            (SDP_INJECTOR_ANNOTATION_STRATEGY, "enabledByDefault"),
            (SDP_INJECTOR_ANNOTATION_ENABLED, "false")
        ])
        .is_candidate());
        assert!(&pod!(0, annotations => vec![
            (SDP_INJECTOR_ANNOTATION_STRATEGY, "disabledByDefault"),
            (SDP_INJECTOR_ANNOTATION_ENABLED, "true")
        ])
        .is_candidate());
        assert!(!&pod!(0, annotations => vec![
            (SDP_INJECTOR_ANNOTATION_STRATEGY, "disabledByDefault"),
        ])
        .is_candidate());
        assert!(!&pod!(0, annotations => vec![
            (SDP_INJECTOR_ANNOTATION_STRATEGY, "disabledByDefault"),
            (SDP_INJECTOR_ANNOTATION_ENABLED, "false")
        ])
        .is_candidate());
    }
}
