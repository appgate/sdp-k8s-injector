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

    fn needs_sidecar(&self) -> bool {
        self.has_appgate_label() && !self.has_sidecars()
    }

    fn containers(&self) -> Option<&Vec<Container>>;

    fn labels(&self) -> Option<&BTreeMap<String, String>>;

    fn inject_sidecars(&mut self, _esidecars: &Vec<Container>) -> () {
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

/*
#[cfg(test)]
mod tests {
    use crate::{pod_needs_injection, pod_inject_sidecar, pod_containers, container_is_appgate,
                get_entry};
    use serde_json::json;

    #[test]
    fn needs_injection_simple() {
        let mut json: serde_json::Value = json!({
            "metadata": {
                "labels": {
                    "appgate-inject": true
                }
            }
        });
        assert_eq!(true, pod_needs_injection(&mut json));

        let mut json = json!({
            "metadata": {
                "labels": {
                    "appgate-inject": "some-value"
                }
            }
        });
        assert_eq!(false, pod_needs_injection(&mut json));

        let mut json = json!({
            "metadata": {
                "labels": {
                    "appgate-inject": false
                }
            }
        });
        assert_eq!(false, pod_needs_injection(&mut json));

        let mut json = json!({
            "metadata": {
                "labels": {
                }
            }
        });
        assert_eq!(false, pod_needs_injection(&mut json));

        let mut json = json!({});
        assert_eq!(false, pod_needs_injection(&mut json))
    }

    #[test]
    fn needs_injection_complex() {
        let mut json = json!({
            "metadata": {
                "labels": {
                    "appgate-inject": true
                }
            },
            "spec": {
                "containers": []
            }
        });
        assert_eq!(true, pod_needs_injection(&mut json));

        let mut json = json!({
            "metadata": {
                "labels": {
                    "appgate-inject": false
                }
            },
            "spec": {
                "containers": [{
                    "name": "appgate-service"
                }]
            }
        });
        assert_eq!(false, pod_needs_injection(&mut json));

        let mut json = json!({
            "metadata": {
                "labels": {
                    "appgate-inject": false
                }
            },
            "spec": {
                "containers": [{
                    "name": "appgate-driver"
                }]
            }
        });
        assert_eq!(false, pod_needs_injection(&mut json));

        let mut json = json!({
            "metadata": {
                "labels": {
                    "appgate-inject": false
                }
            },
            "spec": {
                "containers": [{
                    "name": "appgate-driver"
                }, {
                    "name": "appgate-service"
                }
                ]
            }
        });
        assert_eq!(false, pod_needs_injection(&mut json));

        let mut json = json!({
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
        assert_eq!(true, pod_needs_injection(&mut json));

        let mut json = json!({
            "metadata": {
                "labels": {
                    "appgate-inject": false
                }
            },
            "spec": {
                "containers": [{
                    "name": "my-app"
                }]
            }
        });
        assert_eq!(false, pod_needs_injection(&mut json));
    }

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
    }
}
*/

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
