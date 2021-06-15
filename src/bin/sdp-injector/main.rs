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

const SDP_SIDECAR_NAMES: [&str; 2] = ["sdp-service", "sdp-driver"];
const SDP_SIDECARS_FILE: &str = "/opt/sdp-injector/k8s/sdp-sidecars.json";
const SDP_CERT_FILE_ENV: &str = "SDP_CERT_FILE";
const SDP_KEY_FILE_ENV: &str = "SDP_KEY_FILE";
const SDP_CERT_FILE: &str = "/opt/sdp-injector/k8s/sdp-injector-crt.pem";
const SDP_KEY_FILE: &str = "/opt/sdp-injector/k8s/sdp-injector-key.pem";

fn reader_from_cwd(file_name: &str) -> Result<BufReader<File>, Box<dyn Error>> {
    let cwd = std::env::current_dir()?;
    let file = File::open(cwd.join(PathBuf::from(file_name).as_path()))?;
    Ok(BufReader::new(file))
}

struct SDPPodDisplay<'a>(&'a Pod);

trait SDPPod {
    fn sidecars(&self) -> Option<Vec<&Container>> {
        self.containers().map(|cs|
            cs.iter()
                .filter(|&c| SDP_SIDECAR_NAMES.contains(&&c.name[..]))
                .collect())
    }

    fn sidecar_names(&self) -> Option<Vec<String>> {
        self.sidecars().map(|xs|
            xs.iter().map(|&x| x.name.clone()).collect())
    }

    fn container_names(&self) -> Option<Vec<String>> {
        self.containers().map(|xs|
            xs.iter().map(|x| x.name.clone()).collect())
    }

    fn volume_names(&self) -> Option<Vec<String>> {
        self.volumes().map(|vs| vs.iter().map(|v| v.name.clone()).collect())
    }

    fn has_any_sidecars(&self) -> bool {
        self.sidecars().map(|xs| xs.len() != 0).unwrap_or(false)
    }

    fn has_all_sidecars(&self) -> bool {
        self.sidecars()
            .map(|xs| xs.len() == SDP_SIDECAR_NAMES.len())
            .unwrap_or(false)
    }

    fn has_containers(&self) -> bool {
        self.containers().unwrap_or(&vec![]).len() > 0
    }

    fn needs_patching(&self) -> bool {
        self.has_containers() && !self.has_any_sidecars()
    }

    fn containers(&self) -> Option<&Vec<Container>>;

    fn volumes(&self) -> Option<&Vec<Volume>>;

    fn name(&self) -> String;

    fn patch_sidecars(&self, spd_sidecars: &SDPSidecars) -> Result<Option<Patch>, Box<dyn Error>> {
        info!("Patching POD with SDP client");
        let mut patches = vec![];
        if self.needs_patching() {
            for c in spd_sidecars.containers.iter() {
                patches.push(Add(AddOperation {
                    path: "/spec/containers/-".to_string(),
                    value: serde_json::to_value(&c)?,
                }));
            }
            if self.volumes().is_some() {
                for v in spd_sidecars.volumes.iter() {
                    patches.push(Add(AddOperation {
                        path: "/spec/volumes/-".to_string(),
                        value: serde_json::to_value(&v)?,
                    }));
                }
            } else {
                patches.push(Add(AddOperation {
                    path: "/spec/volumes".to_string(),
                    value: serde_json::to_value(&spd_sidecars.volumes)?,
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
}

impl Display for SDPPodDisplay<'_> {
    fn fmt(&self, f: &mut Formatter<'_>) -> FResult {
        let pod = self.0;
        write!(f, "POD(name:{}, needs_patching:{}, containers:[{}], volumes: [{}])",
               pod.name(), pod.needs_patching(),
               pod.container_names().unwrap_or(vec![]).join(","),
               pod.volume_names().unwrap_or(vec![]).join(","))
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
struct SDPSidecars {
    containers: Box<Vec<Container>>,
    volumes: Box<Vec<Volume>>,
}

fn error_to_bad_request(e: Box<dyn Error>) -> HttpResponse {
    HttpResponse::BadRequest().body(e.to_string())
}

fn load_sidecar_containers() -> Result<SDPSidecars, Box<dyn Error>> {
    debug!("Loading SDP context");
    let cwd = std::env::current_dir()?;
    let file = File::open(cwd.join(PathBuf::from(SDP_SIDECARS_FILE).as_path()))?;
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


fn generate_patch(request_body: &str, context: &SDPSidecars) -> Result<AdmissionReview<DynamicObject>, Box<dyn Error>> {
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

#[post("/mutate")]
async fn mutate(request: HttpRequest, body: String) -> Result<HttpResponse, HttpResponse> {
    let context = request.app_data::<SDPSidecars>()
        .expect("Unable to get app context");
    let admission_review = generate_patch(&body, context);
    if admission_review.is_err() {
        error!("Error injecting SDP client into POD: {}", admission_review.as_ref().unwrap_err());
    }
    //     Ok(HttpResponse::Ok().json(&admission_response.into_review()))
    admission_review
        .and_then(|r| Ok(HttpResponse::Ok().json(&r)))
        .map_err(error_to_bad_request)
}

#[cfg(test)]
mod tests {
    use k8s_openapi::api::core::v1::{Pod, Container};
    use std::collections::{BTreeMap, HashSet};
    use crate::{SDPPod, SDP_SIDECAR_NAMES, load_sidecar_containers, SDPPodDisplay, generate_patch};
    use std::iter::FromIterator;
    use kube::core::admission::AdmissionReview;
    use serde_json::json;

    const SDP_VOLUME_NAMES: [&str; 2] = ["run-appgate", "tun-device"];

    fn create_labels(labels: &[(&str, &str)]) -> BTreeMap<String, String> {
        let mut bm = BTreeMap::new();
        for (k, v) in labels {
            bm.insert(k.to_string(), v.to_string());
        }
        bm
    }

    fn create_container(name: &str) -> Container {
        let mut c: Container = Default::default();
        c.name = name.to_string();
        c
    }

    fn run_test<F>(pod: &mut Pod, test: &TestInject, predicate: &mut F) -> (bool, String, String)
        where F: FnMut(&mut Pod, &TestInject) -> (bool, String, String) {
        pod.metadata.labels = test.labels.as_ref()
            .map(|xs| create_labels(&xs[..]));
        let test_cs: Vec<Container> = test.containers.iter()
            .map(|&x| create_container(x)).collect();
        if let Some(spec) = pod.spec.as_mut() {
            spec.containers = test_cs;
        }
        predicate(pod, test)
    }

    fn assert_tests<F>(tests: &[TestInject], predicate: &mut F) -> () where
        F: FnMut(&mut Pod, &TestInject) -> (bool, String, String)
    {
        let mut test_errors: Vec<(&TestInject, String, String)> = Vec::new();
        let ok = tests.iter().fold(true, |total, t| {
            let mut pod: Pod = Default::default();
            pod.spec = Some(Default::default());
            pod.spec = Some(Default::default());
            let (result, test_description, error_message) = run_test(&mut pod, t, predicate);
            if !result {
                test_errors.push((t, test_description, error_message));
            }
            total && result
        });
        if !ok {
            let errors: Vec<String> = test_errors.iter().map(|x|
                format!("Test {} failed for {:?}, reason: {}",
                        x.1, x.0, x.2).to_string()
            ).collect();
            panic!("Inject test failed: {}", errors.join("\n"));
        }
        assert_eq!(true, true);
    }

    #[derive(Debug)]
    struct TestInject<'a> {
        labels: Option<Vec<(&'a str, &'a str)>>,
        containers: Vec<&'a str>,
        needs_patching: bool,
    }

    fn tests() -> Vec<TestInject<'static>> {
        vec![
            TestInject {
                labels: Some(vec![("sdp-inject", "false")]),
                containers: vec![],
                needs_patching: false,
            },
            TestInject {
                labels: Some(vec![]),
                containers: vec![],
                needs_patching: false,
            },
            TestInject {
                labels: None,
                containers: vec![],
                needs_patching: false,
            },
            TestInject {
                labels: Some(vec![("sdp-inject", "true")]),
                containers: vec![],
                needs_patching: false,
            },
            TestInject {
                labels: Some(vec![("sdp-inject", "true")]),
                containers: vec![],
                needs_patching: false,
            },
            TestInject {
                labels: Some(vec![("sdp-inject", "true")]),
                containers: vec!["some-random-service"],
                needs_patching: true,
            },
            TestInject {
                labels: Some(vec![("sdp-inject", "true")]),
                containers: vec!["some-random-service-1", "some-random-service-2"],
                needs_patching: true,
            },
            TestInject {
                labels: Some(vec![("sdp-inject", "false")]),
                containers: vec!["some-random-service-1", "some-random-service-2"],
                needs_patching: false,
            },
            TestInject {
                labels: Some(vec![("sdp-inject", "true")]),
                containers: vec![SDP_SIDECAR_NAMES[0], SDP_SIDECAR_NAMES[1],
                                 "some-random-service"],
                needs_patching: false,
            },
            TestInject {
                labels: Some(vec![("sdp-inject", "true")]),
                containers: vec![SDP_SIDECAR_NAMES[1], "some-random-service"],
                needs_patching: false,
            },
            TestInject {
                labels: Some(vec![("sdp-inject", "true")]),
                containers: vec![SDP_SIDECAR_NAMES[0], "some-random-service"],
                needs_patching: false,
            }
        ]
    }

    #[test]
    fn needs_injection_simple() {
        let mut predicate = |pod: &mut Pod, test: &TestInject| -> (bool, String, String) {
            (test.needs_patching == pod.needs_patching(), "Injection Simple Test".to_string(),
             format!("{}", SDPPodDisplay(pod)))
        };

        assert_tests(&tests(), &mut predicate)
    }

    #[test]
    fn test_pod_inject_sidecar() {
        let expected_containers = |vs: &[String]| -> HashSet<String> {
            let mut xs: Vec<String> = SDP_SIDECAR_NAMES.iter().map(|n| n.to_string()).collect();
            xs.extend_from_slice(vs);
            HashSet::from_iter(xs.iter().cloned())
        };
        let expected_volumes = |vs: &[String]| -> HashSet<String> {
            let mut xs: Vec<String> = SDP_VOLUME_NAMES.iter().map(|n| n.to_string()).collect();
            xs.extend_from_slice(vs);
            HashSet::from_iter(xs.iter().cloned())
        };
        let sdp_context = load_sidecar_containers()
            .expect("Unable to load the sidecar information");
        let mut predicate = |pod: &mut Pod, test: &TestInject| -> (bool, String, String) {
            let message = format!("Failure for {}", SDPPodDisplay(pod));
            let needs_patching = pod.needs_patching();
            let assert_patch = || -> Result<bool, String> {
                match pod.patch_sidecars(&sdp_context) {
                    Ok(Some(patch)) => {
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
                    }
                    Ok(None) => Err(format!("Patch was expected for {}", SDPPodDisplay(pod))),
                    Err(error) =>
                        Err(format!("Unable to generate patch for {}, {}", SDPPodDisplay(pod), error.to_string()))
                }
            };
            if test.needs_patching && needs_patching {
                if let Err(error_message) = assert_patch() {
                    (false, "Injection Containers Test".to_string(), error_message)
                } else {
                    (true, "Injection Containers Test".to_string(), message)
                }
            } else if test.needs_patching != needs_patching {
                (false, "Injection Containers Test".to_string(), message)
            } else {
                (true, "Injection Containers Test".to_string(), message)
            }
        };

        assert_tests(&tests(), &mut predicate)
    }

    #[test]
    fn test_responses() {
        let mut predicate = |pod: &mut Pod, test: &TestInject| -> (bool, String, String) {
            let pod_value = serde_json::to_value(&pod)
                .expect(&format!("Unable to parse test input {}", SDPPodDisplay(&pod)));
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
            let assert_response = || -> Result<bool, String> {
                let mut admission_review: AdmissionReview<Pod> = serde_json::from_value(admission_review_value)
                    .map_err(|e| format!("Unable to parse generated AdmissionReview value: {}", e.to_string()))?;
                // copy the POD
                let copied_pod: Pod = serde_json::to_value(&pod)
                    .and_then(|v| serde_json::from_value(v))
                    .map_err(|e| format!("Unable to deserialize POD {}", e.to_string()))?;
                if let Some(request) = admission_review.request.as_mut() {
                    request.object = Some(copied_pod);
                }
                // Serialize the request into a string
                let admission_review_str = serde_json::to_string(&admission_review)
                    .map_err(|e| format!("Unable to convert to string generated AdmissionReview value: {}",e.to_string()))?;
                let sdp_context = load_sidecar_containers()
                    .map_err(|e| format!("Unable to load the sidecar information {}", e.to_string()))?;

                // test the generate_patch function now
                let admission_review = generate_patch(&admission_review_str, &sdp_context)
                    .map_err(|e| format!("Error getting response: {}", e.to_string()))?;
                let r = true;
                if let Some(response) = admission_review.response {
                    let response_allowed = test.needs_patching;
                    if response.allowed == response_allowed {
                        Ok(r)
                    } else {
                        Err(format!("{} got response allowed = {} but it expected {}", SDPPodDisplay(&pod),
                                    response.allowed, test.needs_patching))
                    }
                } else {
                    Err("Could not find a response!".to_string())
                }
            };
            match assert_response() {
                Ok(r) => (r, "Injection Containers Test".to_string(), "Unknown".to_string()),
                Err(error) => (false, "Injection Containers Test".to_string(), error)

            }
        };
        assert_tests(&tests(), &mut predicate)
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
    })
        .bind_rustls("0.0.0.0:8443", ssl_config)?
        .run()
        .await
}
