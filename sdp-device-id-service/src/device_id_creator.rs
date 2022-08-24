use crate::device_id_manager::DeviceIdManagerProtocol;
use kube::{Api, Client};
use sdp_common::crd::{DeviceId, ServiceIdentity};
use sdp_common::kubernetes::{get_k8s_client, SDP_K8S_NAMESPACE};
use sdp_common::sdp::system::System;
use tokio::sync::mpsc::{Receiver, Sender};

pub struct DeviceIdCreator {
    service_identity_api: Api<ServiceIdentity>,
    device_id_api: Api<DeviceId>,
    pool_size: usize,
}

#[derive(Debug)]
pub enum DeviceIdCreatorProtocol {
    StartCreator,
}

impl DeviceIdCreator {
    pub async fn new(_client: Client, pool_size: usize) -> DeviceIdCreator {
        let service_identity_api = Api::namespaced(get_k8s_client().await, SDP_K8S_NAMESPACE);
        let device_id_api: Api<DeviceId> =
            Api::namespaced(get_k8s_client().await, SDP_K8S_NAMESPACE);
        DeviceIdCreator {
            service_identity_api,
            device_id_api,
            pool_size,
        }
    }

    pub async fn run(
        self,
        _system: &mut System,
        mut creator_proto_rx: Receiver<DeviceIdCreatorProtocol>,
        _manager_proto_tx: Sender<DeviceIdManagerProtocol<ServiceIdentity, DeviceId>>,
    ) -> () {
        while let Some(message) = creator_proto_rx.recv().await {
            match message {
                DeviceIdCreatorProtocol::StartCreator => {}
            }
        }
    }
}
