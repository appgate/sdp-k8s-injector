use std::collections::{BTreeMap, HashMap};

use http::Uri;
use k8s_openapi::api::{
    apps::v1::Deployment,
    core::v1::{Namespace, Pod},
};
use kube::{core::admission::AdmissionRequest, Client, Config, Resource, ResourceExt};
use log::error;

use crate::{
    annotations::SDP_INJECTOR_ANNOTATION_POD_NAME,
    errors::SDPServiceError,
    service::needs_injection,
    traits::{Annotated, Candidate, Labeled, MaybeNamespaced, MaybeService, Named, ObjectRequest},
};

pub const SDP_K8S_HOST_ENV: &str = "SDP_K8S_HOST";
pub const SDP_K8S_PORT_ENV: &str = "SDP_K8S_PORT";
pub const SDP_K8S_PORT_DEFAULT: &str = "443";
pub const SDP_K8S_HOST_DEFAULT: &str = "kubernetes.default.svc";
pub const SDP_K8S_NO_VERIFY_ENV: &str = "SDP_K8S_NO_VERIFY";
pub const SDP_K8S_NAMESPACE: &str = "sdp-system";
pub const KUBE_SYSTEM_NAMESPACE: &str = "kube-system";

pub async fn get_k8s_client() -> Client {
    let mut k8s_host = String::from("https://");
    k8s_host.push_str(&std::env::var(SDP_K8S_HOST_ENV).unwrap_or(SDP_K8S_HOST_DEFAULT.to_string()));
    k8s_host.push_str(":");
    k8s_host.push_str(&std::env::var(SDP_K8S_PORT_ENV).unwrap_or(SDP_K8S_PORT_DEFAULT.to_string()));
    let k8s_uri = k8s_host
        .parse::<Uri>()
        .expect(format!("Unable to parse SDP_K8S_HOST value: {}", k8s_host).as_str());
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
        /*
        To get the service name for a pod we do:
         1. Check if it's in an annotation defined (added by injector). If it's tehre return it
         2. Check if we have a generate_name field in the metadata (replica set owner / old injectors), then use it.
         3. Return the name as it is
        */
        self.annotation(SDP_INJECTOR_ANNOTATION_POD_NAME)
            .map(Clone::clone)
            .unwrap_or({
                match self.metadata.generate_name.as_ref() {
                    Some(generate_name) => {
                        let mut xs = generate_name.split("-").collect::<Vec<&str>>();
                        if xs.len() > 2 {
                            xs.truncate(xs.len() - 2);
                            xs.join("-")
                        } else {
                            error!(
                                "Unable to find a suitable generate name field: {}",
                                generate_name
                            );
                            uuid::Uuid::new_v4().to_string()
                        }
                    }
                    None => self.metadata.name.as_ref().map(Clone::clone).unwrap_or({
                        error!("Unable to find service name for Pod");
                        uuid::Uuid::new_v4().to_string()
                    }),
                }
            })
    }
}

impl MaybeNamespaced for Pod {
    fn namespace(&self) -> Option<String> {
        ResourceExt::namespace(self)
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
        let name = self.name_any();
        (name != "").then_some(name).unwrap_or_else(|| {
            error!("Unable to find service name for Deployment");
            uuid::Uuid::new_v4().to_string()
        })
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

pub fn admission_request_name<E: Resource + Named>(
    admission_request: &AdmissionRequest<E>,
) -> String {
    admission_request
        .object
        .as_ref()
        .map(Named::name)
        .unwrap_or_else(|| {
            error!("Object not found in admission request");
            uuid::Uuid::new_v4().to_string()
        })
}

pub fn admission_request_namespace<E: Resource + MaybeNamespaced + Clone>(
    admission_request: &AdmissionRequest<E>,
) -> Option<String> {
    if let Some(ns) = admission_request.namespace.as_ref() {
        Some(ns.clone())
    } else {
        admission_request
            .object
            .as_ref()
            .and_then(MaybeNamespaced::namespace)
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
        annotations::{
            SDP_INJECTOR_ANNOTATION_ENABLED, SDP_INJECTOR_ANNOTATION_POD_NAME,
            SDP_INJECTOR_ANNOTATION_STRATEGY,
        },
        traits::{Candidate, Named},
    };

    #[test]
    fn test_pod_service_name() {
        assert_eq!(
            &pod!(0, generate_name => Some("None".to_string()), name => Some("bat".to_string()))
                .name(),
            &"bat"
        );
        assert_eq!(
            &pod!(0, generate_name => Some("None".to_string()), name => Some("bat-bi".to_string()))
                .name(),
            &"bat-bi"
        );
        assert_eq!(
            &pod!(0, generate_name => Some("None".to_string()), name => Some("bat-bi-hiru".to_string())).name(),
            &"bat-bi-hiru"
        );
        assert_eq!(
            &pod!(0, generate_name => Some("None".to_string()), name => Some("bat-bi".to_string()),
            annotations => vec![
               (SDP_INJECTOR_ANNOTATION_POD_NAME, "bost-sei-zazpi-zortzi")
            ])
            .name(),
            &"bost-sei-zazpi-zortzi"
        );
        assert_eq!(
            &pod!(0, name => Some("bat-bi".to_string()),
                generate_name => Some("bost-sei-zazpi-".to_string())
            )
            .name(),
            &"bost-sei"
        );
        assert_eq!(
            &pod!(0, name => Some("bat-bi".to_string()),
                generate_name => Some("bost-sei-zazpi-".to_string()),
                annotations => vec![
                   (SDP_INJECTOR_ANNOTATION_POD_NAME, "bost-sei-zazpi-zortzi")
                ]
            )
            .name(),
            &"bost-sei-zazpi-zortzi"
        );
    }

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
