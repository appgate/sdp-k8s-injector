use crate::device_id_manager::DeviceIdManagerProtocol;
use futures::StreamExt;
use kube::api::ListParams;
use kube::runtime::watcher;
use kube::runtime::watcher::Event;
use kube::{Api, Client};
use log::{error, info};
use sdp_common::crd::{DeviceId, ServiceIdentity};
use sdp_common::service::{HasCredentials, ServiceCandidate};
use tokio::sync::mpsc::{Receiver, Sender};

#[derive(Debug)]
pub enum ServiceIdentityWatcherProtocol {}

pub struct ServiceIdentityWatcher<D: ServiceCandidate + HasCredentials> {
    service_identity_api: Api<D>,
}

impl<'a> ServiceIdentityWatcher<ServiceIdentity> {
    async fn init(&self, mut receiver: Receiver<ServiceIdentityWatcherProtocol>) {
        info!("Waiting for DeviceIdManager to be ready");
        while let Some(message) = receiver.recv().await {
            match message {}
        }
    }

    async fn watch_service_identity(
        &self,
        sender: Sender<DeviceIdManagerProtocol<ServiceIdentity, DeviceId>>,
    ) {
        info!("Starting ServiceIdentity Watcher");
        let tx = &sender;
        watcher::watcher(self.service_identity_api.clone(), ListParams::default())
            .for_each_concurrent(5, |result| async move {
                match result {
                    Ok(Event::Applied(_)) => {}
                    Ok(Event::Restarted(_)) => {}
                    Ok(Event::Deleted(_)) => {}
                    Err(err) => {
                        error!("Error")
                    }
                }
            })
            .await;
    }

    pub async fn run(
        &self,
        receiver: Receiver<ServiceIdentityWatcherProtocol>,
        sender: Sender<DeviceIdManagerProtocol<ServiceIdentity, DeviceId>>,
    ) {
        self.init(receiver).await;
        self.watch_service_identity(sender).await;
    }
}

impl<'a> ServiceIdentityWatcher<ServiceIdentity> {
    pub fn new(client: Client) -> Self {
        let api: Api<ServiceIdentity> = Api::all(client);
        ServiceIdentityWatcher {
            service_identity_api: api,
        }
    }
}
