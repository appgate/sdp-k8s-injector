use actix_web::{post, App, HttpResponse, HttpServer, Responder, HttpRequest};
use k8s_openapi::api::core::v1::{Container, Pod, Volume};
use std::path::PathBuf;
use std::fs::File;
use std::io::BufReader;
use std::error::Error;
use std::collections::BTreeMap;

const APPGATE_TAG_KEY: &str = "appgate-inject";
const APPGATE_TAG_VALUE: &str = "true";
const APPGATE_SIDECAR_NAMES: [&str;2] = ["appgate-service","appgate-driver"];

pub trait AppgatePod {
    fn has_appgate_label(&self) -> bool {
        if let Some(labels) = self.labels() {
            labels.get(APPGATE_TAG_KEY)
                .map(|v| v == APPGATE_TAG_VALUE)
                .unwrap_or(false)
        } else { false }
    }


    fn has_sidecars(&self) -> bool {
        if let Some(containers) = self.containers() {
            containers.iter()
                .filter(|&c| APPGATE_SIDECAR_NAMES.contains(&&c.name[..]))
                .collect::<Vec<&Container>>().len() != 0
        } else { false }
    }

    fn has_containers(&self) -> bool {
        self.containers().unwrap_or(&vec![]).len() > 0
    }

    fn needs_sidecar(&self) -> bool {
        self.has_appgate_label() && self.has_containers() && !self.has_sidecars()
    }

    fn containers(&self) -> Option<&Vec<Container>>;

    fn labels(&self) -> Option<&BTreeMap<String, String>>;

    fn inject_sidecars(&mut self, _sidecars: &Vec<Container>) -> () {
        if let Some(_containers) = self.containers() {
            ()
        }
    }
}

impl AppgatePod for Pod {
    fn containers(&self) -> Option<&Vec<Container>> {
        self.spec.as_ref().map(|s| &s.containers)
    }

    fn labels(&self) -> Option<&BTreeMap<String, String>> {
        self.metadata.labels.as_ref()
    }
}

fn load_sidecar_containers() -> Result<Vec<Container>, Box<dyn Error>> {
    let cwd = std::env::current_dir()?;
    let file = File::open(cwd.join(PathBuf::from("sidecars.json").as_path()))?;
    let reader = BufReader::new(file);
    let containers = serde_json::from_reader(reader)?;
    Ok(containers)
}

fn pod_inject_sidecar(pod: &mut dyn AppgatePod, sidecars: &Vec<Container>) -> Result<(), String> {
    if pod.needs_sidecar() {
        pod.inject_sidecars(&sidecars);
    }
    Ok(())
}

fn inject_sidecar<'a>(req_body: &str, pod: &'a mut Pod) -> Result<&'a mut Pod, String> {
    *pod = serde_json::from_str(&req_body).map_err(|e| e.to_string())?;
    Ok(pod)
}

#[derive(Debug, Clone, Default)]
struct AppgatedContext {
    sidecars: Box<Vec<Container>>,
    volumes: Box<Vec<Volume>>,
}

impl AppgatedContext {
    fn new() -> Self {
        let cs = load_sidecar_containers().expect("Unable to load sidecar containers");
        let vs = vec![];
        AppgatedContext {
            sidecars: Box::new(cs),
            volumes: Box::new(vs),
        }
    }
}

#[post("/mutate")]
async fn mutate(request: HttpRequest) -> impl Responder {
    let _ctx = request.app_data::<AppgatedContext>();
    let _pod: Pod = Default::default();
    HttpResponse::Ok()
}

#[cfg(test)]
mod tests {
    use k8s_openapi::api::core::v1::{Pod, Container};
    use std::collections::BTreeMap;
    use crate::{AppgatePod, APPGATE_SIDECAR_NAMES};

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

    #[test]
    fn needs_injection_simple() {
        let mut pod: Pod = Default::default();
        pod.spec = Some(Default::default());

        #[derive(Debug)]
        struct TestInject<'a> {
            labels: Option<Vec<(&'a str, &'a str)>>,
            containers: Vec<&'a str>,
            result: bool,
        }

        fn run_test(pod: &mut Pod, test: &TestInject) -> bool {
            pod.metadata.labels = test.labels.as_ref()
                .map(|xs| create_labels(&xs[..]));
            let test_cs: Vec<Container> = test.containers.iter()
                .map(|&x| create_container(x)).collect();
            if let Some(spec) = pod.spec.as_mut() {
                spec.containers = test_cs;
            }
            test.result == pod.needs_sidecar()
        }

        let tests = vec![
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
        ];

        let mut test_errors: Vec<&TestInject> = Vec::new();
        let ok = tests.iter().fold(true, |total, t| {
            let r = run_test(&mut pod, t);
            if ! r {
                test_errors.push(t);
            }
            total && r
        });
        if ! ok {
            let errors: Vec<String> = test_errors.iter().map(|&x|
                format!("{:?} expecting {} but got {}", x, x.result, !x.result).to_string()
            ).collect();
            panic!("Inject test failed: {}", errors.join("\n"));
        }
        assert_eq!(true, true);
    }
/*
    #[test]
    fn test_pod_inject_sidecar() {
        let mut pod = json!({
            "metadata": {
                "labels": {
                    "appgate-inject": true
                }
            },
            "spec": {
                "containers": [{
                    "name": "my-app"
                }]
            }
        });
        assert_eq!(true, pod_inject_sidecar(&mut pod).is_ok());
        let cs = pod_containers(&pod);
        assert_eq!(true, cs.is_some());
        if let Some(containers) = pod_containers(&pod) {
            let cs: Vec<&str> = containers.iter()
                .filter(|&c| container_is_appgate(c))
                .map(|c| get_entry(c, &["name"]).ok()
                    .and_then(|n| n.as_str()).unwrap()).collect();
            assert_eq!(["appgate-driver", "appgate-service"], *cs);
        } else {
            panic!("Error");
        }
    }*/
}


#[actix_web::main]
async fn main() -> std::io::Result<()> {
    // Get the sidecar containers definition
    // this is used to inject later the sidecars
    let appgated_context  = AppgatedContext::new();
    HttpServer::new(move || {
        App::new()
            .app_data(appgated_context.clone())
            .service(mutate)
    })
        .bind("127.0.0.1:8080")?
        .run()
        .await
}
