use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};

use http::Uri;
use k8s_openapi::api::{
    apps::v1::Deployment,
    batch::v1::Job,
    core::v1::{Namespace, Pod},
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
pub const KUBE_SYSTEM_NAMESPACE: &str = "kube-system";

pub async fn get_k8s_client() -> Client {
    let mut k8s_host = String::from("https://");
    k8s_host.push_str(&std::env::var(SDP_K8S_HOST_ENV).unwrap_or(SDP_K8S_HOST_DEFAULT.to_string()));
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
        To get the name for a pod we do:
         1. Use `name_any` from kube crate (we get something like deployment-replicaset-pod)
         2. If `name_any` can not provide that info, get the first owner and get the
            name from there (we get something like deployment-replicaset)
         3. If none of those worked we just return a random name to make sure there
            are not matches later in the registered services
         3. If we got something we split by "-" and we remove 1 or 2 items from the right
            (depending where the name is coming from)
        */
        let maybe_name: Option<String>;
        // Job pod has 'job-name' label
        if let Some(job_label) = ResourceExt::labels(self).get("job-name") {
            maybe_name = Some(job_label.to_string());
        // Otherwise, assume it is a Deployment pod
        } else {
            let name = self.name_any();
            maybe_name = (name != "")
                .then_some((name, 2))
                .or_else(|| {
                    let owners = self.owner_references();
                    (owners.len() > 0).then_some((owners[0].name.clone(), 1))
                })
                .map(|(name, n)| {
                    let xs: Vec<&str> = name.split("-").collect();
                    let n = xs.len() - n;
                    xs[0..n].join("-")
                });
        }

        match maybe_name {
            Some(name) if name == "" => uuid::Uuid::new_v4().to_string(),
            Some(name) => name,
            None => uuid::Uuid::new_v4().to_string(),
        }
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

// Type to accept different resources as candidate
#[derive(Debug, Deserialize, Serialize)]
pub enum Target {
    Deployment(Deployment),
    Job(Job),
}

impl Target {
    pub fn deployment(&self) -> Option<&Deployment> {
        if let Target::Deployment(d) = self {
            Some(d)
        } else {
            None
        }
    }

    pub fn job(&self) -> Option<&Job> {
        if let Target::Job(j) = self {
            Some(j)
        } else {
            None
        }
    }
}

impl Default for Target {
    fn default() -> Self {
        Target::Deployment(Deployment::default())
    }
}

impl Clone for Target {
    fn clone(&self) -> Self {
        match self {
            Target::Deployment(d) => Target::Deployment(d.clone()),
            Target::Job(j) => Target::Job(j.clone()),
        }
    }

    fn clone_from(&mut self, source: &Self) {
        match self {
            Target::Deployment(d) => d.clone_from(source.deployment().unwrap()),
            Target::Job(j) => j.clone_from(source.job().unwrap()),
        }
    }
}

impl Named for Target {
    fn name(&self) -> String {
        match self {
            Target::Deployment(d) => {
                let name = d.name_any();
                (name != "").then_some(name).unwrap_or_else(|| {
                    error!("Unable to find service name for Deployment");
                    uuid::Uuid::new_v4().to_string()
                })
            }
            Target::Job(j) => {
                let name = j.name_any();
                (name != "").then_some(name).unwrap_or_else(|| {
                    error!("Unable to find service name for Job");
                    uuid::Uuid::new_v4().to_string()
                })
            }
        }
    }
}

impl MaybeNamespaced for Target {
    fn namespace(&self) -> Option<String> {
        match self {
            Target::Deployment(d) => ResourceExt::namespace(d),
            Target::Job(j) => ResourceExt::namespace(j),
        }
    }
}

impl MaybeService for Target {
    fn service_name(&self) -> Result<String, SDPServiceError> {
        match self {
            Target::Deployment(d) => d.service_name(),
            Target::Job(j) => j.service_name(),
        }
    }

    fn service_id(&self) -> Result<String, SDPServiceError> {
        match self {
            Target::Deployment(d) => d.service_id(),
            Target::Job(j) => j.service_id(),
        }
    }
}

impl Candidate for Target {
    fn is_candidate(&self) -> bool {
        match self {
            Target::Deployment(d) => needs_injection(d),
            Target::Job(j) => needs_injection(j),
        }
    }
}

impl Labeled for Target {
    fn labels(&self) -> Result<HashMap<String, String>, SDPServiceError> {
        match self {
            Target::Deployment(d) => {
                let mut labels = HashMap::from_iter(
                    ResourceExt::labels(d)
                        .iter()
                        .map(|(k, v)| (k.to_string(), v.to_string())),
                );
                let name = Named::name(d);
                MaybeNamespaced::namespace(d)
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
            Target::Job(j) => {
                let mut labels = HashMap::from_iter(
                    ResourceExt::labels(j)
                        .iter()
                        .map(|(k, v)| (k.to_string(), v.to_string())),
                );
                let name = Named::name(j);
                MaybeNamespaced::namespace(j)
                    .map(|ns| {
                        labels.extend([
                            ("namespace".to_string(), ns),
                            ("name".to_string(), name.clone()),
                        ]);
                        labels
                    })
                    .ok_or(SDPServiceError::from_string(format!(
                        "Unable to find namespace for Job {}",
                        &name
                    )))
            }
        }
    }
}

impl Annotated for Target {
    fn annotations(&self) -> Option<&BTreeMap<String, String>> {
        match self {
            Target::Deployment(d) => Some(ResourceExt::annotations(d)),
            Target::Job(j) => Some(ResourceExt::annotations(j)),
        }
    }
}

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

// Implement required traits for Job
impl Named for Job {
    fn name(&self) -> String {
        let name = self.name_any();
        (name != "").then_some(name).unwrap_or_else(|| {
            error!("Unable to find service name for Job");
            uuid::Uuid::new_v4().to_string()
        })
    }
}

impl MaybeNamespaced for Job {
    fn namespace(&self) -> Option<String> {
        ResourceExt::namespace(self)
    }
}

impl MaybeService for Job {}

impl Candidate for Job {
    fn is_candidate(&self) -> bool {
        needs_injection(self)
    }
}

impl Labeled for Job {
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
                "Unable to find namespace for Job {}",
                &name
            )))
    }
}

// Implement requried traits for AdmissionRequest<Job>
impl ObjectRequest<Job> for AdmissionRequest<Job> {
    fn object(&self) -> Option<&Job> {
        self.object.as_ref()
    }
}

impl Named for AdmissionRequest<Job> {
    fn name(&self) -> String {
        admission_request_name(self)
    }
}

impl MaybeNamespaced for AdmissionRequest<Job> {
    fn namespace(&self) -> Option<String> {
        admission_request_namespace(self)
    }
}

impl MaybeService for AdmissionRequest<Job> {}

impl Candidate for AdmissionRequest<Job> {
    fn is_candidate(&self) -> bool {
        true
    }
}

impl Annotated for Job {
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

fn admission_request_name<E: Resource + Named>(admission_request: &AdmissionRequest<E>) -> String {
    admission_request
        .object
        .as_ref()
        .map(Named::name)
        .unwrap_or_else(|| {
            error!("Object not found in admission request");
            uuid::Uuid::new_v4().to_string()
        })
}

fn admission_request_namespace<E: Resource + MaybeNamespaced + Clone>(
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
