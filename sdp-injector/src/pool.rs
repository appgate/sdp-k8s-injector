use std::collections::HashMap;
use kube::api::ListParams;
use kube::{Api, Client};
use sdp_common::crd::DeviceId;
use tokio::sync::mpsc::{Receiver, Sender};
use uuid::Uuid;
use k8s_openapi::api::core::v1::Pod;
use crate::InjectorProtocol;

#[derive(Debug)]
pub enum InjectorPoolProtocol {
    RequestDeviceId {
        service_id: String
    }
}

pub struct InjectorPool {
    pool: HashMap<String, (Uuid, bool)>,
    device_id_api:  Api<DeviceId>,
    pod_api: Api<Pod>
}

impl InjectorPool {
    pub async fn run(
        &self,
        receiver: Receiver<InjectorPoolProtocol>,
        sender: Sender<InjectorProtocol>,
    ) {
        // Initialize the pool with existing device ids
        let device_ids = self.device_id_api.list(&ListParams::default()).await.expect("Unable to list device ids");
        for d in device_ids {
            let deployment = format!("{}-{}", d.spec.service_namespace, d.spec.service_name);

            // Mark UUIDs as activated if it exists in pod annotations

            // Otherwise store the uuid as inactive

            // Store in the pool
        }

        // Wait for messages

    }

    pub fn new(client: Client) -> Self {
        let device_id_api: Api<DeviceId> = Api::all(client.clone());
        let pod_api: Api<Pod> = Api::all(client.clone());
        InjectorPool {
            pool: HashMap::new(),
            device_id_api,
            pod_api
        }
    }
}
