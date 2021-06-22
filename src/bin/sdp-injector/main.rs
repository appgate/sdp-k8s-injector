use actix_web::{post, App, HttpResponse, HttpServer, HttpRequest};
use k8s_openapi::api::core::v1::{Container, Pod, Volume};
use k8s_openapi::apimachinery::pkg::apis::meta::v1::Status;
use std::path::PathBuf;
use std::fs::File;
use std::io::BufReader;
use std::error::Error;
use serde::Deserialize;
use log::{debug, error, info};
use std::fmt::{Display, Formatter, Result as FResult};
use rustls::{ServerConfig, NoClientAuth};
use rustls::internal::pemfile::{certs, rsa_private_keys};
use kube::api::admission::{AdmissionReview, AdmissionResponse, AdmissionRequest};
use json_patch::{Patch, AddOperation};
use json_patch::PatchOperation::Add;
use std::convert::TryInto;
use kube::api::DynamicObject;
use std::collections::{HashSet, BTreeMap};
use std::iter::FromIterator;

const SDP_SIDECARS_FILE: &str = "/opt/sdp-injector/k8s/sdp-sidecars.json";
const SDP_SIDECARS_FILE_ENV: &str = "SDP_SIDECARS_FILE";
const SDP_CERT_FILE_ENV: &str = "SDP_CERT_FILE";
const SDP_KEY_FILE_ENV: &str = "SDP_KEY_FILE";
const SDP_CERT_FILE: &str = "/opt/sdp-injector/k8s/sdp-injector-crt.pem";
const SDP_KEY_FILE: &str = "/opt/sdp-injector/k8s/sdp-injector-key.pem";

fn reader_from_cwd(file_name: &str) -> Result<BufReader<File>, Box<dyn Error>> {
    let cwd = std::env::current_dir()?;
    let file = File::open(cwd.join(PathBuf::from(file_name).as_path()))?;
    Ok(BufReader::new(file))
}

struct SDPPodDisplay<'a>(&'a Pod, &'a SDPSidecars);

trait SDPPod {
    fn sidecars(&self, sdp_sidecars: &SDPSidecars) -> Option<Vec<&Container>> {
        self.containers().map(|cs|
            cs.iter()
                .filter(|&c| sdp_sidecars.container_names().contains(&c.name))
                .collect())
    }

    fn sidecar_names(&self, sdp_sidecars: &SDPSidecars) -> Option<Vec<String>> {
        self.sidecars(sdp_sidecars).map(|xs|
            xs.iter().map(|&x| x.name.clone()).collect())
    }

    fn container_names(&self) -> Option<Vec<String>> {
        self.containers().map(|xs|
            xs.iter().map(|x| x.name.clone()).collect())
    }

    fn volume_names(&self) -> Option<Vec<String>> {
        self.volumes().map(|vs| vs.iter().map(|v| v.name.clone()).collect())
    }

    fn has_any_sidecars(&self, sdp_sidecars: &SDPSidecars) -> bool {
        self.sidecars(sdp_sidecars).map(|xs| xs.len() != 0).unwrap_or(false)
    }

    fn has_all_sidecars(&self, sdp_sidecars: &SDPSidecars) -> bool {
        self.sidecars(sdp_sidecars)
            .map(|xs| xs.len() == sdp_sidecars.containers.len())
            .unwrap_or(false)
    }

    fn has_containers(&self) -> bool {
        self.containers().unwrap_or(&vec![]).len() > 0
    }

    fn disabled_by_annotations(&self) -> bool {
        self.annotations()
            .and_then(|bm|bm.get("sdp-injector"))
            .map(|a| a.eq("false"))
            .unwrap_or(false)
    }

    fn needs_patching(&self, sdp_sidecars: &SDPSidecars) -> bool {
        self.has_containers() && !self.has_any_sidecars(sdp_sidecars) && !self.disabled_by_annotations()
    }

    fn containers(&self) -> Option<&Vec<Container>>;

    fn volumes(&self) -> Option<&Vec<Volume>>;

    fn name(&self) -> String;

    fn annotations(&self) -> Option<&BTreeMap<String, String>>;

    fn patch_sidecars(&self, sdp_sidecars: &SDPSidecars) -> Result<Option<Patch>, Box<dyn Error>> {
        info!("Patching POD with SDP client");
        let mut patches = vec![];
        if self.needs_patching(&sdp_sidecars) {
            for c in sdp_sidecars.containers.iter() {
                patches.push(Add(AddOperation {
                    path: "/spec/containers/-".to_string(),
                    value: serde_json::to_value(&c)?,
                }));
            }
            if self.volumes().is_some() {
                for v in sdp_sidecars.volumes.iter() {
                    patches.push(Add(AddOperation {
                        path: "/spec/volumes/-".to_string(),
                        value: serde_json::to_value(&v)?,
                    }));
                }
            } else {
                patches.push(Add(AddOperation {
                    path: "/spec/volumes".to_string(),
                    value: serde_json::to_value(&sdp_sidecars.volumes)?,
                }));
            }
        }
        if patches.is_empty() {
            debug!("POD does not require patching");
            Ok(None)
        } else {
            Ok(Some(Patch(patches)))
        }
    }

    fn validate_sidecars(&self, sdp_sidecars: &SDPSidecars) -> Result<(), String> {
        let expected_volume_names = HashSet::from_iter(
            sdp_sidecars.volumes.iter().map(|v| v.name.clone()));
        let expected_container_names = HashSet::from_iter(
            sdp_sidecars.containers.iter().map(|c| c.name.clone()));
        let container_names = HashSet::from_iter(self.sidecar_names(sdp_sidecars)
            .unwrap_or(vec![])
            .iter().cloned());
        let volume_names = HashSet::from_iter(self.volume_names()
            .unwrap_or(vec![])
            .iter().cloned());
        let mut errors: Vec<String> = vec![];
        if !expected_container_names.is_subset(&container_names) {
            let container_i: HashSet<String> = container_names.intersection(&expected_container_names)
                .cloned().collect();
            let mut missing_containers: Vec<String> = expected_container_names
                .difference(&container_i).cloned().collect();
            missing_containers.sort();
            errors.push(format!("POD is missing needed containers: {}", missing_containers.join(", ")));
        }
        if !expected_volume_names.is_subset(&volume_names) {
            let volumes_i: HashSet<String> = volume_names.intersection(&expected_volume_names)
                .cloned().collect();
            let mut missing_volumes: Vec<String> = expected_volume_names
                .difference(&volumes_i).cloned().collect();
            missing_volumes.sort();
            errors.push(format!("POD is missing needed volumes: {}", missing_volumes.join(", ")));
        }
        if errors.is_empty() {
            Ok(())
        } else {
            Err(format!("Unable to run SDP client on POD: {}", errors.join("\n")).to_string())
        }
    }
}

impl SDPPod for Pod {
    fn containers(&self) -> Option<&Vec<Container>> {
        self.spec.as_ref().map(|s| &s.containers)
    }

    fn volumes(&self) -> Option<&Vec<Volume>> {
        self.spec.as_ref().and_then(|s| s.volumes.as_ref())
    }

    fn name(&self) -> String {
        self.metadata.name.as_ref().map(|x| x.clone())
            .unwrap_or("Unnamed".to_string())
    }

    fn annotations(&self) -> Option<&BTreeMap<String, String>> {
        self.metadata.annotations.as_ref()
    }
}

impl Display for SDPPodDisplay<'_> {
    fn fmt(&self, f: &mut Formatter<'_>) -> FResult {
        let pod = self.0;
        let sdp_sidecars = self.1;
        write!(f, "POD(name:{}, needs_patching:{}, containers:[{}], volumes: [{}])",
               pod.name(), pod.needs_patching(sdp_sidecars),
               pod.container_names().unwrap_or(vec![]).join(","),
               pod.volume_names().unwrap_or(vec![]).join(","))
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
struct SDPSidecars {
    containers: Box<Vec<Container>>,
    volumes: Box<Vec<Volume>>,
}

impl SDPSidecars {
    fn container_names(&self) -> Vec<String> {
        self.containers.iter().map(|c| c.name.clone()).collect()
    }

    fn volume_names(&self) -> Vec<String> {
        self.volumes.iter().map(|c| c.name.clone()).collect()
    }
}

fn error_to_bad_request(e: Box<dyn Error>) -> HttpResponse {
    HttpResponse::BadRequest().body(e.to_string())
}

fn load_sidecar_containers() -> Result<SDPSidecars, Box<dyn Error>> {
    debug!("Loading SDP context");
    let cwd = std::env::current_dir()?;
    let mut sidecars_path = cwd.join(PathBuf::from(SDP_SIDECARS_FILE).as_path());
    if let Ok(sidecars_file) = std::env::var(SDP_SIDECARS_FILE_ENV) {
        sidecars_path = PathBuf::from(sidecars_file);
    }
    let file = File::open(sidecars_path)?;
    let reader = BufReader::new(file);
    let sdp_sidecars = serde_json::from_reader(reader)?;
    debug!("SDP context loaded successful");
    Ok(sdp_sidecars)
}

fn load_cert_files() -> Result<(BufReader<File>, BufReader<File>), Box<dyn Error>> {
    let cert_file = std::env::var(SDP_CERT_FILE_ENV).unwrap_or(SDP_CERT_FILE.to_string());
    let key_file = std::env::var(SDP_KEY_FILE_ENV).unwrap_or(SDP_KEY_FILE.to_string());
    let cert_buf = reader_from_cwd(&cert_file)?;
    let key_buf = reader_from_cwd(&key_file)?;
    Ok((cert_buf, key_buf))
}


fn patch_request(request_body: &str, context: &SDPSidecars) -> Result<AdmissionReview<DynamicObject>, Box<dyn Error>> {
    let admision_review = serde_json::from_str::<AdmissionReview<Pod>>(&request_body)
        .map_err(|e| format!("Error parsing payload: {}", e.to_string()))?;
    let admission_request: AdmissionRequest<Pod> = admision_review.try_into()?;
    let pod = admission_request.object.as_ref().ok_or("Admission review does not contain a POD")?;
    let mut admission_response = AdmissionResponse::from(&admission_request);
    match pod.patch_sidecars(&context)? {
        Some(p) => admission_response = admission_response.with_patch(p)?,
        None => {
            let mut status: Status = Default::default();
            status.code = Some(40);
            status.message = Some("This POD can not be patched".to_string());
            admission_response.allowed = false;
            admission_response.result = status;
        }
    }
    Ok(admission_response.into_review())
}

fn validate_request(request_body: &str, context: &SDPSidecars) -> Result<AdmissionReview<DynamicObject>, Box<dyn Error>> {
    let admision_review = serde_json::from_str::<AdmissionReview<Pod>>(&request_body)
        .map_err(|e| format!("Error parsing payload: {}", e.to_string()))?;
    let admission_request: AdmissionRequest<Pod> = admision_review.try_into()?;
    let pod = admission_request.object.as_ref().ok_or("Admission review does not contain a POD")?;
    let mut admission_response = AdmissionResponse::from(&admission_request);
    if let Err(error) = pod.validate_sidecars(&context) {
        let mut status: Status = Default::default();
        status.code = Some(40);
        status.message = Some(error);
        admission_response.allowed = false;
        admission_response.result = status;
    }
    Ok(admission_response.into_review())
}

#[post("/mutate")]
async fn mutate(request: HttpRequest, body: String) -> Result<HttpResponse, HttpResponse> {
    let context = request.app_data::<SDPSidecars>()
        .expect("Unable to get app context");
    let admission_review = patch_request(&body, context);
    if admission_review.is_err() {
        error!("Error injecting SDP client into POD: {}", admission_review.as_ref().unwrap_err());
    }
    admission_review
        .and_then(|r| Ok(HttpResponse::Ok().json(&r)))
        .map_err(error_to_bad_request)
}

#[post("/validate")]
async fn validate(request: HttpRequest, body: String) -> Result<HttpResponse, HttpResponse> {
    let context = request.app_data::<SDPSidecars>()
        .expect("Unable to get app context");
    let admission_review = validate_request(&body, context);
    if admission_review.is_err() {
        error!("Error validating POD with SDP client: {}", admission_review.as_ref().unwrap_err());
    }
    admission_review
        .and_then(|r| Ok(HttpResponse::Ok().json(&r)))
        .map_err(error_to_bad_request)
}

#[cfg(test)]
mod tests {
    use k8s_openapi::api::core::v1::{Pod, Container, Volume};
    use std::collections::{BTreeMap, HashSet};
    use crate::{SDPPod, load_sidecar_containers, SDPPodDisplay, SDP_SIDECARS_FILE_ENV, SDPSidecars, patch_request};
    use std::iter::FromIterator;
    use kube::core::admission::AdmissionReview;
    use serde_json::json;
    use json_patch::Patch;

    fn load_sidecar_containers_env() -> Result<SDPSidecars, String> {
        std::env::set_var(SDP_SIDECARS_FILE_ENV, "k8s/sdp-sidecars.json");
        load_sidecar_containers()
            .map_err(|e| format!("Unable to load the sidecar information {}", e.to_string()))
    }

    macro_rules! set_pod_field {
        ($pod:expr, containers => $cs:expr) => {
            let test_cs: Vec<Container> = $cs.iter().map(|x| {
                let mut c: Container = Default::default();
                c.name = x.to_string();
                c
            }).collect();
            if let Some(spec) = $pod.spec.as_mut() {
                spec.containers = test_cs;
            }
        };
        ($pod:expr, volumes => $vs:expr) => {
            let volumes: Vec<Volume> = $vs.iter().map(|x| {
                    let mut v: Volume = Default::default();
                    v.name = x.to_string();
                    v
            }).collect();

            if volumes.len() > 0 {
                if let Some(spec) = $pod.spec.as_mut() {
                    spec.volumes = Some(volumes);
                }
            }
        };
        ($pod:expr, annotations => $cs:expr) => {
            let mut bm = BTreeMap::new();
            for (k, v) in $cs {
                bm.insert(k.to_string(), v.to_string());
            }
            $pod.metadata.annotations = Some(bm);
        };
    }

    macro_rules! test_pod {
        () => {{
            let mut pod: Pod = Default::default();
            pod.spec = Some(Default::default());
            pod
        }};
        ($pod:expr) => {{$pod}};
        ($($fs:ident => $es:expr),*) => {{
            let mut pod: Pod = Default::default();
            pod.spec = Some(Default::default());
            test_pod!(pod,$($fs => $es),*)
        }};
        ($pod:expr,$f:ident => $e:expr) => {{
            set_pod_field!($pod, $f => $e);
            test_pod!($pod)
        }};
        ($pod:expr,$f:ident => $e:expr,$($fs:ident => $es:expr),*) => {{
            set_pod_field!($pod, $f => $e);
            test_pod!($pod,$($fs => $es),*)
        }};
    }

    fn assert_tests(test_results: &[TestResult]) -> () {
        let mut test_errors: Vec<(usize, String, String)> = Vec::new();
        let ok = test_results.iter()
            .enumerate().fold(true, |total, (c, (result, test_description, error_message))| {
            if !result {
                test_errors.push((c, test_description.clone(), error_message.clone()));
            }
            total && *result
        });
        if !ok {
            let errors: Vec<String> = test_errors.iter().map(|x|
                format!("Test {} [#{}] failed, reason: {}", x.1, x.0, x.2).to_string()
            ).collect();
            panic!("Inject test failed: {}", errors.join("\n"));
        }
        assert_eq!(true, true);
    }

    #[derive(Debug)]
    struct TestPatch {
        pod: Pod,
        needs_patching: bool,
    }

    struct TestValidate {
        pod: Pod,
        validation_errors: Option<String>,
    }

    type TestResult = (bool, String, String);

    fn patch_tests(sdp_sidecars: &SDPSidecars) -> Vec<TestPatch> {
        let sdp_sidecar_names = sdp_sidecars.container_names();
        vec![
            TestPatch {
                pod: test_pod!(),
                needs_patching: false,
            },
            TestPatch {
                pod: test_pod!(containers => vec!["random-service"]),
                needs_patching: true,
            },
            TestPatch {
                pod: test_pod!(containers => vec!["random-service"],
                               annotations => vec![("sdp-injector", "false")]),
                needs_patching: false,
            },
            TestPatch {
                pod: test_pod!(containers => vec!["random-service"],
                               annotations => vec![("sdp-injector", "who-knows")]),
                needs_patching: true,
            },
            TestPatch {
                pod: test_pod!(containers => vec!["random-service"],
                               annotations => vec![("sdp-injector", "true")]),
                needs_patching: true,
            },
            TestPatch {
                pod: test_pod!(containers => vec!["some-random-service-1",
                                                  "some-random-service-2"]),
                needs_patching: true,
            },
            TestPatch {
                pod: test_pod!(containers => vec![sdp_sidecar_names[0].clone(),
                                                  sdp_sidecar_names[1].clone(),
                                                  "some-random-service".to_string()]),
                needs_patching: false,
            },
            TestPatch {
                pod: test_pod!(containers => vec![sdp_sidecar_names[0].clone(),
                                                  sdp_sidecar_names[1].clone(),
                                                  "some-random-service".to_string()],
                               annotations => vec![("sdp-injector", "true")]),
                needs_patching: false,
            },
            TestPatch {
                pod: test_pod!(containers => vec![sdp_sidecar_names[1].clone(),
                                                  "some-random-service".to_string()]),
                needs_patching: false,
            },
            TestPatch {
                pod: test_pod!(containers => vec![sdp_sidecar_names[1].clone(),
                                                  "some-random-service".to_string()],
                               annotations => vec![("sdp-injector", "true")]),
                needs_patching: false,
            },
        ]
    }

    fn validation_tests() -> Vec<TestValidate> {
        vec![
            TestValidate {
                pod: test_pod!(),
                validation_errors: Some(r#"Unable to run SDP client on POD: POD is missing needed containers: sdp-dnsmasq, sdp-driver, sdp-service
POD is missing needed volumes: run-appgate, tun-device"#.to_string())
            },
            TestValidate {
                pod: test_pod!(containers => vec!["sdp-dnsmasq"]),
                validation_errors: Some(r#"Unable to run SDP client on POD: POD is missing needed containers: sdp-driver, sdp-service
POD is missing needed volumes: run-appgate, tun-device"#.to_string())
            },
            TestValidate {
                pod: test_pod!(containers => vec!["sdp-dnsmasq", "sdp-service"]),
                validation_errors: Some(r#"Unable to run SDP client on POD: POD is missing needed containers: sdp-driver
POD is missing needed volumes: run-appgate, tun-device"#.to_string())
            },
            TestValidate {
                pod: test_pod!(containers => vec!["sdp-service", "sdp-dnsmasq", "sdp-driver"]),
                validation_errors: Some(r#"Unable to run SDP client on POD: POD is missing needed volumes: run-appgate, tun-device"#.to_string())
            },
            TestValidate {
                pod: test_pod!(containers => vec!["sdp-service", "sdp-dnsmasq", "sdp-driver"],
                               volumes => vec!["run-appgate"]),
                validation_errors: Some(r#"Unable to run SDP client on POD: POD is missing needed volumes: tun-device"#.to_string())
            },
            TestValidate {
                pod: test_pod!(containers => vec!["sdp-service", "sdp-dnsmasq", "sdp-driver"],
                               volumes => vec!["run-appgate"]),
                validation_errors: Some(r#"Unable to run SDP client on POD: POD is missing needed volumes: tun-device"#.to_string())
            },
            TestValidate {
                pod: test_pod!(containers => vec!["sdp-driver"],
                               volumes => vec!["run-appgate", "tun-device"]),
                validation_errors: Some(r#"Unable to run SDP client on POD: POD is missing needed containers: sdp-dnsmasq, sdp-service"#.to_string())
            },
            TestValidate {
                pod: test_pod!(containers => vec!["sdp-service", "sdp-dnsmasq", "sdp-driver"],
                               volumes => vec!["run-appgate", "tun-device"]),
                validation_errors: None
            },
            TestValidate {
                pod: test_pod!(containers => vec!["_sdp-service", "_sdp-dnsmasq", "_sdp-driver"],
                               volumes => vec!["_run-appgate", "_tun-device"]),
                validation_errors: Some(r#"Unable to run SDP client on POD: POD is missing needed containers: sdp-dnsmasq, sdp-driver, sdp-service
POD is missing needed volumes: run-appgate, tun-device"#.to_string())
            },
        ]
    }

    #[test]
    fn needs_patching() {
        let sdp_sidecars = load_sidecar_containers_env()
            .expect("Unable to load sidecars context");
        let results: Vec<TestResult> = patch_tests(&sdp_sidecars).iter().map(|t| {
            let needs_patching = t.pod.needs_patching(&sdp_sidecars);
            (t.needs_patching == needs_patching, "Needs patching simple".to_string(),
             format!("Got {}, expected {}", needs_patching, t.needs_patching))
        }).collect();
        assert_tests(&results)
    }

    #[test]
    fn test_pod_patch_sidecars() {
        let sdp_sidecars = load_sidecar_containers_env()
            .expect("Unable to load sidecars context");
        let expected_containers = |vs: &[String]| -> HashSet<String> {
            let mut xs: Vec<String> = sdp_sidecars.container_names();
            xs.extend_from_slice(vs);
            HashSet::from_iter(xs.iter().cloned())
        };
        let expected_volumes = |vs: &[String]| -> HashSet<String> {
            let mut xs: Vec<String> = sdp_sidecars.volume_names();
            xs.extend_from_slice(vs);
            HashSet::from_iter(xs.iter().cloned())
        };
        let assert_patch = |pod: &Pod, mb: Option<Patch>| -> Result<bool, String> {
            mb.ok_or("Patch not found!".to_string()).and_then(|patch| {
                let unpatched_containers = pod.container_names().unwrap_or(vec![]);
                let unpatched_volumes = pod.volume_names().unwrap_or(vec![]);
                let mut unpatched_pod = serde_json::to_value(pod)
                    .map_err(|e| format!("Unable to convert POD to value [{}]", e.to_string()))?;
                json_patch::patch(&mut unpatched_pod, &patch)
                    .map_err(|e| format!("Unable to patch POD [{}]", e.to_string()))?;
                let patched_pod: Pod = serde_json::from_value(unpatched_pod)
                    .map_err(|e| format!("Unable to convert patched POD [{}]", e.to_string()))?;
                let patched_containers = HashSet::from_iter(patched_pod.container_names()
                    .unwrap_or(vec![]).iter().cloned());
                (patched_containers == expected_containers(&unpatched_containers)).then(|| true)
                    .ok_or(format!("Wrong containers after patch, got {:?}, expected {:?}", patched_containers,
                                   expected_containers(&unpatched_containers)))?;
                let patched_volumes = HashSet::from_iter(patched_pod.volume_names()
                    .unwrap_or(vec![]).iter().cloned());
                (patched_volumes == expected_volumes(&unpatched_volumes)).then(|| true)
                    .ok_or(format!("Wrong volumes after patch, got {:?}, expected {:?}", patched_volumes,
                                   expected_volumes(&unpatched_volumes)))?;
                Ok(true)
            })
        };

        let test_description = || "Test patch".to_string();
        let results: Vec<TestResult> = patch_tests(&sdp_sidecars).iter().map(|test| {
            let needs_patching = test.pod.needs_patching(&sdp_sidecars);
            let patch = test.pod.patch_sidecars(&sdp_sidecars);
            if test.needs_patching && needs_patching {
                match patch.map_err(|e| e.to_string()).and_then(|p| assert_patch(&test.pod, p)) {
                    Ok(res) => (res, test_description(), "".to_string()),
                    Err(error) => (false, test_description(), format!("Error applying patch: {}", error))
                }
            } else if test.needs_patching != needs_patching {
                (false, test_description(), format!("Got needs_patching {}, expected {}", needs_patching,
                                                    test.needs_patching))
            } else {
                (true, test_description(), "".to_string())
            }
        }).collect();

        assert_tests(&results)
    }

    #[test]
    fn test_mutate_responses() {
        let sdp_sidecars = load_sidecar_containers_env().expect("Unable to load sidecars context");
        let assert_response = |test: &TestPatch| -> Result<bool, String> {
            let pod_value = serde_json::to_value(&test.pod)
                .expect(&format!("Unable to parse test input {}", SDPPodDisplay(&test.pod, &sdp_sidecars)));
            let admission_review_value = json!({
                "apiVersion": "admission.k8s.io/v1",
                "kind": "AdmissionReview",
                "request": {
                    "uid": "705ab4f5-6393-11e8-b7cc-42010a800002",
                    "kind" :{"group":"","version":"v1","kind":"Pod"},
                    "resource":{"group":"","version":"v1","resource":"pods"},
                    "requestKind":{"group":"","version": "v1","kind":"Pod"},
                    "requestResource":{"group":"","version":"v1","resource":"pods"},
                    "name": "my-deployment",
                    "namespace": "my-namespace",
                    "operation": "UPDATE",
                    "userInfo": {
                        "username": "admin",
                        "uid": "014fbff9a07c",
                        "groups": ["system:authenticated","my-admin-group"],
                        "extra": {
                            "some-key":["some-value1", "some-value2"]
                        }
                    }
                },
                "object": pod_value});

            let mut admission_review: AdmissionReview<Pod> = serde_json::from_value(admission_review_value)
                .map_err(|e| format!("Unable to parse generated AdmissionReview value: {}",
                                     e.to_string()))?;
            // copy the POD
            let copied_pod: Pod = serde_json::to_value(&test.pod)
                .and_then(|v| serde_json::from_value(v))
                .map_err(|e| format!("Unable to deserialize POD {}", e.to_string()))?;
            if let Some(request) = admission_review.request.as_mut() {
                request.object = Some(copied_pod);
            }
            // Serialize the request into a string
            let admission_review_str = serde_json::to_string(&admission_review)
                .map_err(|e| format!("Unable to convert to string generated AdmissionReview value: {}", e.to_string()))?;
            // test the patch_request function now
            let admission_review = patch_request(&admission_review_str, &sdp_sidecars)
                .map_err(|e| format!("Error getting response: {}", e.to_string()))?;
            let r = true;
            if let Some(response) = admission_review.response {
                let response_allowed = test.needs_patching;
                if response.allowed == response_allowed {
                    Ok(r)
                } else {
                    Err(format!("{} got response allowed {}, expected response allowed {}",
                                SDPPodDisplay(&test.pod, &sdp_sidecars), response.allowed,
                                test.needs_patching))
                }
            } else {
                Err("Could not find a response!".to_string())
            }
        };

        let results: Vec<TestResult> = patch_tests(&sdp_sidecars).iter().map(|test| {
            match assert_response(test) {
                Ok(r) => (r, "Injection Containers Test".to_string(), "Unknown".to_string()),
                Err(error) => (false, "Injection Containers Test".to_string(), error)
            }
        }).collect();
        assert_tests(&results)
    }

    #[test]
    fn test_pod_validation() {
        let sdp_sidecars = load_sidecar_containers_env()
            .expect("Unable to load sidecars context");
        let test_description = || "Test POD validation".to_string();
        let results: Vec<TestResult> = validation_tests().iter().map(|t| {
            let pass_validation = t.pod.validate_sidecars(&sdp_sidecars);
            match (pass_validation, t.validation_errors.as_ref()) {
                (Ok(_), Some(expected_error)) => {
                    let error_message = format!("Got no validation error, expected '{}'", expected_error);
                    (false, test_description(), error_message)
                },
                (Ok(_), None) => (true, test_description(), "".to_string()),
                (Err(error), None) => {
                    let error_message = format!("Got validation error '{}', expected none", error);
                    (false, test_description(), error_message)
                },
                (Err(error), Some(expected_error)) if !error.eq(expected_error) => {
                    let error_message = format!("Got validation error '{}', expected '{}'", error, expected_error);
                    (false, test_description(), error_message)
                },
                (Err(_), Some(_)) => {
                    (true, test_description(), "".to_string())
                },
            }}).collect();
        assert_tests(&results)
    }
}

fn load_ssl() -> Result<ServerConfig, Box<dyn Error>> {
    let (mut cert_buf, mut key_buf) = load_cert_files()?;

    let mut config = ServerConfig::new(NoClientAuth::new());
    let cert_chain = certs(&mut cert_buf)
        .map_err(|_| "Unable to load certificate file")?;
    let mut keys = rsa_private_keys(&mut key_buf)
        .map_err(|_| "Unable to load key file")?;
    config.set_single_cert(cert_chain, keys.remove(0))?;
    Ok(config)
}

#[actix_web::main]
async fn main() -> std::io::Result<()> {
    env_logger::init();
    info!("Starting sdp-injector!!!!");
    // Get the sidecar containers definition
    // this is used to inject later the sidecars
    let sdp_context = load_sidecar_containers()
        .expect("Unable to load the sidecar information");
    let ssl_config = load_ssl().expect("Unable to load certificates");
    HttpServer::new(move || {
        App::new()
            .app_data(sdp_context.clone())
            .service(mutate)
            .service(validate)
    })
        .bind_rustls("0.0.0.0:8443", ssl_config)?
        .run()
        .await
}
