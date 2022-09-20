use std::collections::HashMap;
use kube::api::ListParams;
use kube::{Api, Client};
use sdp_common::crd::DeviceId;
use tokio::sync::mpsc::{Receiver, Sender};
use uuid::Uuid;
use k8s_openapi::api::core::v1::Pod;
use k8s_openapi::api::apps::v1::ReplicaSet;
use log::{error, info};
use sdp_common::kubernetes::SDP_K8S_NAMESPACE;
use crate::{InjectorProtocol, SDP_ANNOTATION_CLIENT_DEVICE_ID};
use sdp_common::service::Annotated;
use kube::Resource;

#[derive(Debug)]
pub enum InjectorPoolProtocol {
    RequestDeviceId {
        service_id: String,
    }
}


#[derive(Debug)]
struct InjectorPoolItem {
    pod: String,
    uuid: Uuid,
}

pub struct InjectorPool {
    /// Mapping of Deployment <-> Vec<(Pod, UUID)>
    pool: HashMap<String, Vec<InjectorPoolItem>>,
    device_id_api:  Api<DeviceId>,
    replicaset_api: Api<ReplicaSet>,
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
        let existing_device_ids = self.device_id_api.list(&ListParams::default()).await.expect("Unable to list device ids");

        for device_id in existing_device_ids {
            let replicasets = self.replicaset_api.list(&ListParams::default()).await.expect("Unable to list replicaset");

            for replicaset in replicasets {
                if let Some(replicaset_owners) = &replicaset.metadata.owner_references.as_ref() {
                    let replicaset_owner = &replicaset_owners[0];
                    let deployment_name = device_id.metadata.name.as_ref().unwrap().clone();
                    if replicaset_owner.name == deployment_name {
                        let replicaset_name =  replicaset.metadata.name.as_ref().unwrap().clone();
                        info!("Owner for replicaset {} is deployment {}", replicaset_name, replicaset_owner.name);
                        let pods = self.pod_api.list(&ListParams::default()).await.expect("Unable to list pods");
                        let mut pool_items = vec![];

                        for pod in pods {
                            if let Some(pod_owners) = &pod.metadata.owner_references.as_ref() {
                                let pod_owner = &pod_owners[0];
                                if pod_owner.name == replicaset_name {
                                    let pod_name = pod.metadata.name.as_ref().unwrap().clone();
                                    info!("Owner for pod {} is replicaset {}", pod_name,  pod_owner.name);
                                    let device_id_annotation = pod.annotation(SDP_ANNOTATION_CLIENT_DEVICE_ID);
                                    if device_id_annotation.is_some() {
                                        info!("Found device ID {:?} annotated in pod {}", device_id_annotation, pod_name);
                                        pool_items.push(InjectorPoolItem {
                                            pod: pod_name,
                                            uuid: Uuid::parse_str(device_id_annotation.unwrap()).unwrap()
                                        })
                                    } else {
                                        info!("No device id annotation found in pod {}", pod_name);
                                        pool_items.push(InjectorPoolItem {
                                            pod: "".to_string(),
                                            uuid: Uuid::parse_str(device_id_annotation.unwrap()).unwrap()
                                        })
                                    }
                                }
                            }
                        }
                        info!("Inserting to the pool: {:?}", pool_items);
                        self.pool.insert(device_id.metadata.name.as_ref().unwrap().clone(), pool_items);
                    }
                }
            }
        }

        injector_tx.send(InjectorProtocol::InjectorPoolReady)
            .await
            .expect("Error when sending InjectorPoolReady message");

        while let Some(message) = pool_rx.recv().await {
            match message {
                InjectorPoolProtocol::RequestDeviceId { service_id} => {
                    match self.device_id_api.get_opt(&service_id).await {
                        Ok(None) => {
                            info!("Couldn't find device ID yet. Retrying later.")
                        }
                        Ok(device_id) => {
                            let device_id = device_id.unwrap();
                            // We have a deployment registered in the pool, find an inactive uuid.
                            // inactive uuids have an empty pod field in the InjectorPoolItem
                            if let Some(pool_item) = self.pool.get(device_id.metadata.name.as_ref().unwrap()) {
                                info!("Found existing entry in the InjectorPool. Looking for available UUID.")

                                // We have a brand new deployment
                            } else {
                                info!("Brand new deployment");
                                // Just get the first one
                                injector_tx.send(InjectorProtocol::FoundDeviceId { uuid: Uuid::parse_str(&device_id.spec.uuids[0]).unwrap() })
                                    .await.expect(&format!("Error when sending FoundDeviceId to Injector"));
                            }
                        }
                        Err(err) => {
                            error!("Error when getting the device id: {:?}", err);
                        }
                    }
                }
            }
        }
    }

    pub fn new(client: Client) -> Self {
        let device_id_api: Api<DeviceId> = Api::namespaced(client.clone(), SDP_K8S_NAMESPACE);
        let replicaset_api: Api<ReplicaSet> = Api::all(client.clone());
        let pod_api: Api<Pod> = Api::all(client.clone());
        InjectorPool {
            pool: HashMap::new(),
            device_id_api,
            replicaset_api,
            pod_api
        }
    }
}
