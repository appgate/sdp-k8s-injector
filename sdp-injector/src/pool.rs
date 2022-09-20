use std::collections::HashMap;
use kube::api::ListParams;
use kube::{Api, Client};
use sdp_common::crd::DeviceId;
use tokio::sync::mpsc::{Receiver, Sender};
use uuid::Uuid;
use k8s_openapi::api::core::v1::Pod;
use log::info;
use sdp_common::kubernetes::SDP_K8S_NAMESPACE;
use crate::InjectorProtocol;

#[derive(Debug)]
pub enum InjectorPoolProtocol {
    RequestDeviceId {
        service_id: String,
        pod_name: String
    }
}

pub struct InjectorPool {
    /// Mapping of pod.metadata.name <-> (uuid, is_activated)
    pool: HashMap<String, (Uuid, bool)>,
    device_id_api:  Api<DeviceId>,
    pod_api: Api<Pod>
}

impl InjectorPool {
    pub async fn run(
        &mut self,
        mut pool_rx: Receiver<InjectorPoolProtocol>,
        injector_tx: Sender<InjectorProtocol>,
    ) {
        info!("Initializing InjectorPool by reading existing device ids");
        // Initialize the pool with existing device ids
        let _existing_device_ids = self.device_id_api.list(&ListParams::default()).await.expect("Unable to list device ids");
        let _pods = self.pod_api.list(&ListParams::default()).await.expect("Unable to list pods");

        injector_tx.send(InjectorProtocol::InjectorPoolReady)
            .await
            .expect("Error when sending InjectorPoolReady message");

        while let Some(message) = pool_rx.recv().await {
            match message {
                InjectorPoolProtocol::RequestDeviceId { service_id, pod_name} => {
                    if let Some(device_id) = self.pool.get(&pod_name) {
                        let is_activated = device_id.1;
                        if !is_activated {
                            info!("In pool and inactive. Sending to injector: {}", device_id.1.to_string());
                            injector_tx.send(InjectorProtocol::FoundDeviceId {
                                uuid: device_id.0
                            }).await.expect(&format!("Error when sending FoundDeviceId to Injector for pod {}", pod_name));
                        } else {
                            info!("In pool but activated: {}", device_id.1.to_string())
                        }
                    } else {
                        info!("Couldn't find uuid for pod {} in the pool. Assigning new one", pod_name);
                        let device_id = self.device_id_api.get(&service_id).await.expect(&format!("Unable to get device id {}", service_id));
                        let inactive_device_ids: Vec<String> = self.pool.values().filter(|did | !did.1).map(|did| did.0.to_string()).collect();
                        for uuid in device_id.spec.uuids {
                            info!("{}", uuid);
                            if !inactive_device_ids.contains(&uuid) {
                                info!("Assining uuid {} to pod {}", uuid, pod_name);
                                self.pool.insert(pod_name.clone(), (Uuid::parse_str(&uuid).expect("Error parsing uuid"), true));
                            }
                        }
                    }
                }
            }
        }
    }

    pub fn new(client: Client) -> Self {
        let device_id_api: Api<DeviceId> = Api::namespaced(client.clone(), SDP_K8S_NAMESPACE);
        let pod_api: Api<Pod> = Api::all(client.clone());
        InjectorPool {
            pool: HashMap::new(),
            device_id_api,
            pod_api
        }
    }
}
