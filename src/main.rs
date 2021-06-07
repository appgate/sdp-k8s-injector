use actix_web::{post, App, HttpResponse, HttpServer, HttpRequest};
use k8s_openapi::api::core::v1::{Container, Pod, Volume};
use std::path::PathBuf;
use std::fs::File;
use std::io::BufReader;
use std::error::Error;
use std::collections::BTreeMap;
use serde::Deserialize;

const APPGATE_TAG_KEY: &str = "appgate-inject";
const APPGATE_TAG_VALUE: &str = "true";
const APPGATE_SIDECAR_NAMES: [&str; 2] = ["appgate-service", "appgate-driver"];
const APPGATE_VOLUME_NAMES: [&str; 2] = ["run-appgate", "tun-device"];


trait AppgatePod {
    fn has_appgate_label(&self) -> bool {
        if let Some(labels) = self.labels() {
            labels.get(APPGATE_TAG_KEY)
                .map(|v| v == APPGATE_TAG_VALUE)
                .unwrap_or(false)
        } else { false }
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

    fn needs_sidecar_containers(&self) -> bool {
        self.has_appgate_label() && self.has_containers() && !self.has_any_sidecars()
    }

    fn needs_sidecar_volumes(&self) -> bool {
        self.has_containers() && self.has_all_sidecars()
    }

    fn containers(&self) -> Option<&Vec<Container>>;

    fn mut_containers(&mut self) -> Option<&mut Vec<Container>>;

    fn volumes(&self) -> Option<&Vec<Volume>>;

    fn mut_volumes(&mut self) -> Option<&mut Vec<Volume>>;

    fn labels(&self) -> Option<&BTreeMap<String, String>>;

    fn normalize(&mut self) -> ();

    fn inject_containers(&mut self, containers: &Vec<Container>) {
        if self.needs_sidecar_containers() {
            if let Some(cs) = self.mut_containers() {
                cs.extend_from_slice(containers)
            }
        }
    }

    fn inject_volumes(&mut self, volumes: &Vec<Volume>) {
        if self.needs_sidecar_volumes() {
            if let Some(vs) = self.mut_volumes() {
                vs.extend_from_slice(volumes)
            }
        }
    }

    fn inject_sidecars(&mut self, appgate_sidecars: &AppgateSidecars) {
        self.normalize();
        self.inject_containers(&appgate_sidecars.containers);
        self.inject_volumes(&appgate_sidecars.volumes);
    }
}

impl AppgatePod for Pod {
    fn containers(&self) -> Option<&Vec<Container>> {
        self.spec.as_ref().map(|s| &s.containers)
    }

    fn mut_containers(&mut self) -> Option<&mut Vec<Container>> {
        self.spec.as_mut().map(|s| s.containers.as_mut())
    }

    fn volumes(&self) -> Option<&Vec<Volume>> {
        self.spec.as_ref().and_then(|s| s.volumes.as_ref())
    }

    fn mut_volumes(&mut self) -> Option<&mut Vec<Volume>> {
        self.spec.as_mut().and_then(|s| s.volumes.as_mut())
    }

    fn labels(&self) -> Option<&BTreeMap<String, String>> {
        self.metadata.labels.as_ref()
    }

    fn normalize(&mut self) -> () {
        let spec = self.spec.get_or_insert(Default::default());
        spec.volumes.get_or_insert(Default::default());
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
struct AppgateSidecars {
    containers: Box<Vec<Container>>,
    volumes: Box<Vec<Volume>>,
}

fn error_to_bad_request(e: serde_json::Error) -> HttpResponse {
    HttpResponse::BadRequest().body(e.to_string())
}

fn load_sidecar_containers() -> Result<AppgateSidecars, Box<dyn Error>> {
    let cwd = std::env::current_dir()?;
    let file = File::open(cwd.join(PathBuf::from("sidecars.json").as_path()))?;
    let reader = BufReader::new(file);
    let appgate_sidecars = serde_json::from_reader(reader)?;
    Ok(appgate_sidecars)
}

fn inject_sidecars(request_body: &str, context: &AppgateSidecars) -> Result<HttpResponse, HttpResponse> {
    let mut pod = serde_json::from_str::<Pod>(&request_body).map_err(error_to_bad_request)?;
    if pod.needs_sidecar_containers() {
        pod.inject_sidecars(context);
    }
    let response_body = serde_json::to_string(&pod).map_err(error_to_bad_request)?;
    Ok(HttpResponse::Ok().body(response_body))
}

#[post("/mutate")]
async fn mutate(request: HttpRequest, body: String) -> Result<HttpResponse, HttpResponse> {
    let context = request.app_data::<AppgateSidecars>()
        .expect("Unable to get app context");
    inject_sidecars(&body, context)
}

#[cfg(test)]
mod tests {
    use k8s_openapi::api::core::v1::{Pod, Container};
    use std::collections::BTreeMap;
    use crate::{AppgatePod, APPGATE_SIDECAR_NAMES, load_sidecar_containers};

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

    fn run_test<F>(pod: &mut Pod, test: &TestInject, predicate: &mut F) -> (bool, String)
    where F: FnMut(&mut Pod, &TestInject) -> (bool, String) {
        pod.metadata.labels = test.labels.as_ref()
            .map(|xs| create_labels(&xs[..]));
        let test_cs: Vec<Container> = test.containers.iter()
            .map(|&x| create_container(x)).collect();
        if let Some(spec) = pod.spec.as_mut() {
            spec.containers = test_cs;
        }
        predicate(pod, test)
    }

    fn assert_tests<F>(pod: &mut Pod, tests: &[TestInject], predicate: &mut F) -> () where
        F: FnMut(&mut Pod, &TestInject) -> (bool, String)
    {
        let mut test_errors: Vec<(&TestInject, String)> = Vec::new();
        let ok = tests.iter().fold(true, |total, t| {
            let (result, description) = run_test(pod, t, predicate);
            if !result {
                test_errors.push((t, description));
            }
            total && result
        });
        if !ok {
            let errors: Vec<String> = test_errors.iter().map(|x|
                format!("Test {} for {:?} failed, expecting {} but got {}",
                        x.1, x.0, x.0.result, !x.0.result).to_string()
            ).collect();
            panic!("Inject test failed: {}", errors.join("\n"));
        }
        assert_eq!(true, true);
    }

    #[derive(Debug)]
    struct TestInject<'a> {
        labels: Option<Vec<(&'a str, &'a str)>>,
        containers: Vec<&'a str>,
        result: bool,
    }

    fn tests() -> Vec<TestInject<'static>> {
        vec![
            TestInject {
                labels: Some(vec![("appgate-inject", "false")]),
                containers: vec![],
                result: false,
            },
            TestInject {
                labels: Some(vec![]),
                containers: vec![],
                result: false,
            },
            TestInject {
                labels: None,
                containers: vec![],
                result: false,
            },
            TestInject {
                labels: Some(vec![("appgate-inject", "true")]),
                containers: vec![],
                result: false,
            },
            TestInject {
                labels: Some(vec![("appgate-inject", "true")]),
                containers: vec![],
                result: false,
            },
            TestInject {
                labels: Some(vec![("appgate-inject", "true")]),
                containers: vec!["some-random-service"],
                result: true,
            },
            TestInject {
                labels: Some(vec![("appgate-inject", "true")]),
                containers: vec!["some-random-service-1", "some-random-service-2"],
                result: true,
            },
            TestInject {
                labels: Some(vec![("appgate-inject", "false")]),
                containers: vec!["some-random-service-1", "some-random-service-2"],
                result: false,
            },
            TestInject {
                labels: Some(vec![("appgate-inject", "true")]),
                containers: vec![APPGATE_SIDECAR_NAMES[0], APPGATE_SIDECAR_NAMES[1],
                                 "some-random-service"],
                result: false,
            },
            TestInject {
                labels: Some(vec![("appgate-inject", "true")]),
                containers: vec![APPGATE_SIDECAR_NAMES[1], "some-random-service"],
                result: false,
            },
            TestInject {
                labels: Some(vec![("appgate-inject", "true")]),
                containers: vec![APPGATE_SIDECAR_NAMES[0], "some-random-service"],
                result: false,
            }
        ]
    }

    #[test]
    fn needs_injection_simple() {
        let mut pod: Pod = Default::default();
        pod.spec = Some(Default::default());

        let mut predicate = |pod: &mut Pod, test: &TestInject| -> (bool, String) {
            (test.result == pod.needs_sidecar(), "Injection Simple Test".to_string())
        };

        assert_tests(&mut pod, &tests(), &mut predicate)
    }

    #[test]
    fn test_pod_inject_sidecar() {
        let mut pod: Pod = Default::default();
        pod.spec = Some(Default::default());

        let expected_sidecars = Some(vec!["appgate-service".to_string(),
                                          "appgate-driver".to_string()]);
        let appgated_context = load_sidecar_containers()
            .expect("Unable to load the sidecar information");
        let mut predicate = |pod: &mut Pod, test: &TestInject| -> (bool, String) {
            let mut r = test.result == pod.needs_sidecar();
            if r && test.result {
                pod.inject_sidecars(&appgated_context.containers);
                r = r && (pod.sidecar_names() == expected_sidecars);
            }
            (r, "Injection Containers Test".to_string())
        };

        assert_tests(&mut pod, &tests(), &mut predicate)
    }
}

#[actix_web::main]
async fn main() -> std::io::Result<()> {
    // Get the sidecar containers definition
    // this is used to inject later the sidecars
    let appgated_context = load_sidecar_containers()
        .expect("Unable to load the sidecar information");
    HttpServer::new(move || {
        App::new()
            .app_data(appgated_context.clone())
            .service(mutate)
    })
        .bind("127.0.0.1:8080")?
        .run()
        .await
}
