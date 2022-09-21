use crate::InjectorProtocol;
use k8s_openapi::api::apps::v1::ReplicaSet;
use k8s_openapi::api::core::v1::Pod;
use kube::api::ListParams;
use kube::{Api, Client};
use log::{error, info};
use sdp_common::crd::DeviceId;
use sdp_common::kubernetes::SDP_K8S_NAMESPACE;
use tokio::sync::mpsc::{Receiver, Sender};
use tokio::sync::{broadcast, watch};
use uuid::Uuid;

#[derive(Debug)]
pub enum InjectorPoolProtocol {
    RequestDeviceId { service_id: String },
}

pub struct InjectorPool {
    device_id_api: Api<DeviceId>,
    replicaset_api: Api<ReplicaSet>,
    pod_api: Api<Pod>,
}

impl InjectorPool {
    pub async fn run(
        &mut self,
        mut pool_rx: broadcast::Receiver<InjectorProtocol>,
        injector_tx: Sender<InjectorProtocol>,
    ) {
        while let Ok(message) = pool_rx.recv().await {
            match message {
                InjectorProtocol::RequestDeviceId { service_id } => {
                    match self.device_id_api.get_opt(&service_id).await {
                        Ok(None) => {
                            info!(
                                "Device ID for {} is not available yet. Retrying.",
                                service_id
                            );
                        }
                        Ok(device_id) => {
                            let device_id = device_id.unwrap();

                            let mut available_uuids: Vec<String> = device_id.spec.uuids;

                            // Figure out which UUIDs are still available by iterating
                            // through the pods belonging to the replicaset/deployment
                            // and checking the environment variable CLIENT_DEVICE_ID.
                            // If the env is set, we remove the UUID from availability
                            let replicasets = self
                                .replicaset_api
                                .list(&ListParams::default())
                                .await
                                .expect("Unable to list replicaset");
                            for replicaset in replicasets {
                                if let Some(replicaset_owners) =
                                    &replicaset.metadata.owner_references.as_ref()
                                {
                                    let replicaset_owner = &replicaset_owners[0];
                                    let replicaset_name = format!(
                                        "{}-{}",
                                        replicaset.metadata.namespace.unwrap(),
                                        replicaset_owner.name
                                    );
                                    let deployment_name =
                                        device_id.metadata.name.as_ref().unwrap().clone();

                                    // Found a replicaset owned by the deployment
                                    if replicaset_name == deployment_name {
                                        info!(
                                            "Found owner for replicaset {}: deployment {}",
                                            replicaset_name, replicaset_owner.name
                                        );
                                        let pods = self
                                            .pod_api
                                            .list(&ListParams::default())
                                            .await
                                            .expect("Unable to list pods");
                                        for pod in pods {
                                            if let Some(pod_owners) =
                                                &pod.metadata.owner_references.as_ref()
                                            {
                                                let replicaset_name = replicaset
                                                    .metadata
                                                    .name
                                                    .as_ref()
                                                    .unwrap()
                                                    .clone();
                                                let pod_owner = &pod_owners[0];
                                                let pod_name =
                                                    pod.metadata.name.as_ref().unwrap().clone();

                                                // Found a pod owned by the replicaset
                                                if pod_owner.name == replicaset_name {
                                                    info!(
                                                        "Found owner for pod {}: replicaset {}",
                                                        pod_name, pod_owner.name
                                                    );
                                                    // Iterate through environment variable, looking for CLIENT_DEVICE_ID
                                                    for container in
                                                        &pod.spec.as_ref().unwrap().containers
                                                    {
                                                        if container.name == "sdp-service" {
                                                            for env in
                                                                container.env.as_ref().unwrap()
                                                            {
                                                                // If found, remove the uuid from availability
                                                                if env.name == "CLIENT_DEVICE_ID" {
                                                                    let unavailable_uuid =
                                                                        env.value.as_ref().unwrap();
                                                                    let index = available_uuids
                                                                        .iter()
                                                                        .position(|x| {
                                                                            x == unavailable_uuid
                                                                        })
                                                                        .unwrap();
                                                                    info!("Removing uuid {} from availability", unavailable_uuid);
                                                                    available_uuids.remove(index);
                                                                }
                                                            }
                                                        }
                                                    }
                                                }
                                            }
                                        }
                                    }
                                }
                            }

                            info!("Available uuids: {:?}", available_uuids);

                            let available_uuid = available_uuids.get(0).unwrap();
                            injector_tx
                                .send(InjectorProtocol::FoundDeviceId {
                                    uuid: Uuid::parse_str(&available_uuid).unwrap(),
                                })
                                .await
                                .expect("Error when sending FoundDeviceId message to Injector")
                        }
                        Err(err) => {
                            error!("Error when getting the device id: {:?}", err);
                        }
                    }
                }
                InjectorProtocol::InjectorPoolReady => {}
                InjectorProtocol::FoundDeviceId { uuid } => {}
            }
        }
    }

    pub fn new(client: Client) -> Self {
        let device_id_api: Api<DeviceId> = Api::namespaced(client.clone(), SDP_K8S_NAMESPACE);
        let replicaset_api: Api<ReplicaSet> = Api::all(client.clone());
        let pod_api: Api<Pod> = Api::all(client.clone());
        InjectorPool {
            device_id_api,
            replicaset_api,
            pod_api,
        }
    }
}
