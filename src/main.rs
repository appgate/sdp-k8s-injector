use actix_web::{post, App, HttpResponse, HttpServer, Responder};
use serde_json::{Value, Map, json};
use serde_json::map::Entry::Occupied;
use k8s_openapi::api::core::v1::{Container, EnvVar, EnvVarSource, Pod, PodSpec, Volume};
use std::path::{Path, PathBuf};
use std::fs::File;
use std::io::BufReader;
use std::error::Error;

fn load_sidecar_containers() -> Result<Vec<Container>, Box<dyn Error>> {
    let cwd = std::env::current_dir()?;
    let file = File::open(cwd.join(PathBuf::from("sidecars.json").as_path()))?;
    let reader = BufReader::new(file);
    let containers = serde_json::from_reader(reader)?;
    Ok(containers)
}

fn get_or_set_entry<'a>(obj: &'a mut Value, keys: &[&str], value: Option<Value>) -> Result<&'a Value, String> {
    if keys.len() == 0 {
        if let Some(v) = value {
            *obj = v;
        }
        return Ok(obj);
    }
    let key = &keys[0];
    match obj {
        Value::Object(m) => {
            match m.entry(*key) {
                Occupied(v) => {
                    get_or_set_entry(v.into_mut(), &keys[1..], value)
                }
                _ => Result::Err(String::from("Key not found"))
            }
        }
        _ => Result::Err(String::from(format!("Error {}", key)))
    }
}

fn get_entry<'a>(obj: &'a Value, keys: &[&str]) -> Result<&'a Value, String> {
    if keys.len() == 0 {
        return Ok(obj);
    }
    let key = &keys[0];
    match obj {
        Value::Object(m) => {
            match m.get(*key) {
                Some(v) => {
                    get_entry(v, &keys[1..])
                }
                _ => Result::Err(String::from("Key not found"))
            }
        }
        _ => Result::Err(String::from(format!("Error {}", key)))
    }
}

fn set_entry<'a>(obj: &'a mut Value, keys: &[&str], value: Option<Value>) -> Result<&'a Value, String> {
    get_or_set_entry(obj, keys, value)
}

fn has_tag_bool(tags: &Map<String, Value>, tag: &str) -> bool {
    tags.get(tag).and_then(|v| v.as_bool()).unwrap_or(false)
}

fn pod_containers(value: &Value) -> Option<&Vec<Value>> {
    get_entry(value, &["spec", "containers"])
        .ok()
        .and_then(|v| v.as_array())
}

fn pod_labels(value: &Value) -> Option<&Map<String, Value>> {
    get_entry(value, &["metadata", "labels"])
        .ok()
        .and_then(|v| v.as_object())
}

fn container_is_appgate(container: &Value) -> bool {
    get_entry(container, &["name"]).ok()
        .and_then(|v| v.as_str())
        .map(|n| n == "appgate-service" || n == "appgate-driver")
        .unwrap_or(false)
}

fn containers_already_injected(containers: &Vec<Value>) -> bool {
    let mut cnt = 0;
    for c in containers {
        if container_is_appgate(c) {
            cnt += 1;
        }
    }
    cnt == 2
}

fn container_appgate_service() -> Value {
    json!({
        "name": "appgate-service"
    })
}

fn container_appgate_driver() -> Value {
    json!({
        "name": "appgate-driver"
    })
}

fn pod_needs_injection(pod: &Value) -> bool {
    let b1 = pod_labels(pod).map_or(false, |ts|
        has_tag_bool(ts, "appgate-inject"));
    let b2 = pod_containers(pod).map_or(false,
                                        |x| containers_already_injected(x));
    b1 && !b2
}


fn pod_inject_sidecar(pod: &mut Value) -> Result<(), String> {
    if pod_needs_injection(pod) {
        if let Some(vs) = pod_containers(pod) {
            let mut xs = vs.clone();
            xs.insert(0, container_appgate_service());
            xs.insert(0, container_appgate_driver());
            set_entry(pod, &["spec", "containers"], Some(Value::Array(xs)))?;
            Ok(())
        } else {
            Err(String::from("Containers not found in spec"))
        }
    } else {
        Ok(())
    }
}

fn inject_sidecar(req_body: &str, pod: &mut Value) -> Result<(), String> {
    *pod = serde_json::from_str(req_body).map_err(|e| e.to_string())?;
    pod_inject_sidecar(pod)
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
async fn mutate(req_body: String) -> impl Responder {
    let mut pod = Value::Null;
    inject_sidecar(&req_body, &mut pod)
        .map_or_else(|error| HttpResponse::BadRequest().body(error),
                     |_| HttpResponse::Ok().body(pod))
}

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

#[actix_web::main]
async fn main() -> std::io::Result<()> {
    // Get the sidecar containers definition
    // this is used to inject later the sidecars
    let mut appgated_context  = AppgatedContext::new();
    HttpServer::new(move || {
        App::new()
            .app_data(appgated_context.clone())
            .service(mutate)
    })
        .bind("127.0.0.1:8080")?
        .run()
        .await
}
