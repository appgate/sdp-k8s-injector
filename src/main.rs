use actix_web::{post, App, HttpResponse, HttpServer, HttpRequest};
use k8s_openapi::api::core::v1::{Container, Pod, Volume};
use std::path::PathBuf;
use std::fs::File;
use std::io::BufReader;
use std::error::Error;
use std::collections::BTreeMap;
use serde::Deserialize;
use log::{debug, error, info};
use std::fmt::{Display, Formatter, Result as FResult};
use rustls::{ServerConfig, NoClientAuth};
use rustls::internal::pemfile::{certs, rsa_private_keys};
use kube::api::admission::{AdmissionReview, AdmissionResponse, AdmissionRequest};
use json_patch::{Patch, AddOperation};
use json_patch::PatchOperation::Add;
use std::convert::TryInto;

const APPGATE_TAG_KEY: &str = "appgate-inject";
const APPGATE_TAG_VALUE: &str = "true";
const APPGATE_SIDECAR_NAMES: [&str; 2] = ["appgate-service", "appgate-driver"];
const APPGATE_SIDECARS_FILE: &str = "sidecars.json";
const APPGATE_CERT_FILE: &str = "certs/cert.pem";
const APPGATE_KEY_FILE: &str = "certs/private-key.pem";

fn reader_from_cwd(file_name: &str) -> Result<BufReader<File>, Box<dyn Error>> {
    let cwd = std::env::current_dir()?;
    let file = File::open(cwd.join(PathBuf::from(file_name).as_path()))?;
    Ok(BufReader::new(file))
}

struct AppgatePodDisplay<'a>(&'a Pod);

trait AppgatePod {
    fn has_appgate_label(&self) -> bool {
        if let Some(labels) = self.labels() {
            labels.get(APPGATE_TAG_KEY)
                .map(|v| v == APPGATE_TAG_VALUE)
                .unwrap_or(false)
        } else {
            false
        }
    }

    fn sidecars(&self) -> Option<Vec<&Container>> {
        self.containers().map(|cs|
            cs.iter()
                .filter(|&c| APPGATE_SIDECAR_NAMES.contains(&&c.name[..]))
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
            .map(|xs| xs.len() == APPGATE_SIDECAR_NAMES.len())
            .unwrap_or(false)
    }

    fn has_containers(&self) -> bool {
        self.containers().unwrap_or(&vec![]).len() > 0
    }

    fn needs_patching(&self) -> bool {
        self.has_appgate_label() && self.has_containers() && !self.has_any_sidecars()
    }

    fn containers(&self) -> Option<&Vec<Container>>;

    fn volumes(&self) -> Option<&Vec<Volume>>;

    fn labels(&self) -> Option<&BTreeMap<String, String>>;

    fn name(&self) -> String;

    fn patch_sidecars(&self, appgate_sidecars: &AppgateSidecars) -> Result<Option<Patch>, Box<dyn Error>> {
        info!("Patching POD with appgate client");
        let mut patches = vec![];
        if self.needs_patching() {
            for c in appgate_sidecars.containers.iter() {
                patches.push(Add(AddOperation {
                    path: "/spec/containers/-".to_string(),
                    value: serde_json::to_value(&c)?,
                }));
            }
            if self.volumes().is_some() {
                for v in appgate_sidecars.volumes.iter() {
                    patches.push(Add(AddOperation {
                        path: "/spec/volumes/-".to_string(),
                        value: serde_json::to_value(&v)?,
                    }));
                }
            } else {
                patches.extend(vec![Add(AddOperation {
                    path: "/spec/volumes".to_string(),
                    value: serde_json::to_value(&appgate_sidecars.volumes)?,
                })]);
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

impl AppgatePod for Pod {
    fn containers(&self) -> Option<&Vec<Container>> {
        self.spec.as_ref().map(|s| &s.containers)
    }

    fn volumes(&self) -> Option<&Vec<Volume>> {
        self.spec.as_ref().and_then(|s| s.volumes.as_ref())
    }

    fn labels(&self) -> Option<&BTreeMap<String, String>> {
        self.metadata.labels.as_ref()
    }

    fn name(&self) -> String {
        self.metadata.name.as_ref().map(|x| x.clone())
            .unwrap_or("Unnamed".to_string())
    }
}

impl Display for AppgatePodDisplay<'_> {
    fn fmt(&self, f: &mut Formatter<'_>) -> FResult {
        let pod = self.0;
        write!(f, "POD(name:{}, needs_patching:{}, containers:[{}], volumes: [{}])",
               pod.name(), pod.needs_patching(),
               pod.container_names().unwrap_or(vec![]).join(","),
               pod.volume_names().unwrap_or(vec![]).join(","))
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
struct AppgateSidecars {
    containers: Box<Vec<Container>>,
    volumes: Box<Vec<Volume>>,
}

fn error_to_bad_request(e: Box<dyn Error>) -> HttpResponse {
    HttpResponse::BadRequest().body(e.to_string())
}

fn load_sidecar_containers() -> Result<AppgateSidecars, Box<dyn Error>> {
    debug!("Loading appgate context");
    let cwd = std::env::current_dir()?;
    let file = File::open(cwd.join(PathBuf::from(APPGATE_SIDECARS_FILE).as_path()))?;
    let reader = BufReader::new(file);
    let appgate_sidecars = serde_json::from_reader(reader)?;
    debug!("Appgate context loaded successful");
    Ok(appgate_sidecars)
}

fn load_cert_files() -> Result<(BufReader<File>, BufReader<File>), Box<dyn Error>> {
    let cert_buf = reader_from_cwd(APPGATE_CERT_FILE)?;
    let key_buf = reader_from_cwd(APPGATE_KEY_FILE)?;
    Ok((cert_buf, key_buf))
}

fn generate_patch(request_body: &str, context: &AppgateSidecars) -> Result<HttpResponse, Box<dyn Error>> {
    let admision_review = serde_json::from_str::<AdmissionReview<Pod>>(&request_body)
        .map_err(|e| format!("Error parsing payload: {}", e.to_string()))?;
    let admission_request: AdmissionRequest<Pod> = admision_review.try_into()?;
    let pod = admission_request.object.as_ref().ok_or("Admission review does not contain a POD")?;
    let mut admission_response = AdmissionResponse::from(&admission_request);
    if let Some(p) = pod.patch_sidecars(&context)? {
        admission_response = admission_response.with_patch(p)?;
    }
    let response = &admission_response.into_review();
    //let response_body = serde_json::to_string(&admission_response.into_review())?;
    Ok(HttpResponse::Ok().json(&response))
}

#[post("/mutate")]
async fn mutate(request: HttpRequest, body: String) -> Result<HttpResponse, HttpResponse> {
    let context = request.app_data::<AppgateSidecars>()
        .expect("Unable to get app context");
    let result = generate_patch(&body, context);
    if result.is_err() {
        error!("Error injecting appgate client into POD: {}", result.as_ref().unwrap_err());
    }
    result.map_err(error_to_bad_request)
}

#[cfg(test)]
mod tests {
    use k8s_openapi::api::core::v1::{Pod, Container};
    use std::collections::{BTreeMap, HashSet};
    use crate::{AppgatePod, APPGATE_SIDECAR_NAMES, load_sidecar_containers, AppgatePodDisplay};
    use std::iter::FromIterator;

    const APPGATE_VOLUME_NAMES: [&str; 2] = ["run-appgate", "tun-device"];

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
                labels: Some(vec![("appgate-inject", "false")]),
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
                labels: Some(vec![("appgate-inject", "true")]),
                containers: vec![],
                needs_patching: false,
            },
            TestInject {
                labels: Some(vec![("appgate-inject", "true")]),
                containers: vec![],
                needs_patching: false,
            },
            TestInject {
                labels: Some(vec![("appgate-inject", "true")]),
                containers: vec!["some-random-service"],
                needs_patching: true,
            },
            TestInject {
                labels: Some(vec![("appgate-inject", "true")]),
                containers: vec!["some-random-service-1", "some-random-service-2"],
                needs_patching: true,
            },
            TestInject {
                labels: Some(vec![("appgate-inject", "false")]),
                containers: vec!["some-random-service-1", "some-random-service-2"],
                needs_patching: false,
            },
            TestInject {
                labels: Some(vec![("appgate-inject", "true")]),
                containers: vec![APPGATE_SIDECAR_NAMES[0], APPGATE_SIDECAR_NAMES[1],
                                 "some-random-service"],
                needs_patching: false,
            },
            TestInject {
                labels: Some(vec![("appgate-inject", "true")]),
                containers: vec![APPGATE_SIDECAR_NAMES[1], "some-random-service"],
                needs_patching: false,
            },
            TestInject {
                labels: Some(vec![("appgate-inject", "true")]),
                containers: vec![APPGATE_SIDECAR_NAMES[0], "some-random-service"],
                needs_patching: false,
            }
        ]
    }

    #[test]
    fn needs_injection_simple() {
        let mut predicate = |pod: &mut Pod, test: &TestInject| -> (bool, String, String) {
            (test.needs_patching == pod.needs_patching(), "Injection Simple Test".to_string(),
             format!("{}", AppgatePodDisplay(pod)))
        };

        assert_tests(&tests(), &mut predicate)
    }

    #[test]
    fn test_pod_inject_sidecar() {
        let expected_containers = |vs: &[String]| -> HashSet<String> {
            let mut xs: Vec<String> = APPGATE_SIDECAR_NAMES.iter().map(|n| n.to_string()).collect();
            xs.extend_from_slice(vs);
            HashSet::from_iter(xs.iter().cloned())
        };
        let expected_volumes = |vs: &[String]| -> HashSet<String> {
            let mut xs: Vec<String> = APPGATE_VOLUME_NAMES.iter().map(|n| n.to_string()).collect();
            xs.extend_from_slice(vs);
            HashSet::from_iter(xs.iter().cloned())
        };
        let appgate_context = load_sidecar_containers()
            .expect("Unable to load the sidecar information");
        let mut predicate = |pod: &mut Pod, test: &TestInject| -> (bool, String, String) {
            let message = format!("Failure for {}", AppgatePodDisplay(pod));
            let needs_patching = pod.needs_patching();
            let assert_patch = || -> Result<bool, String> {
                match pod.patch_sidecars(&appgate_context) {
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
                    Ok(None) => Err(format!("Patch was expected for {}", AppgatePodDisplay(pod))),
                    Err(error) =>
                        Err(format!("Unable to generate patch for {}, {}", AppgatePodDisplay(pod), error.to_string()))
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
    info!("Starting appgate-injector!!!!");
    // Get the sidecar containers definition
    // this is used to inject later the sidecars
    let appgated_context = load_sidecar_containers()
        .expect("Unable to load the sidecar information");
    let ssl_config = load_ssl().expect("Unable to load certificates");
    HttpServer::new(move || {
        App::new()
            .app_data(appgated_context.clone())
            .service(mutate)
    })
        .bind_rustls("0.0.0.0:8433", ssl_config)?
        .run()
        .await
}
