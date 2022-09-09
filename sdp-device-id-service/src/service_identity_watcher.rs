use crate::device_id_manager::DeviceIdManagerProtocol;
use futures::{StreamExt, TryStreamExt};
use kube::api::ListParams;
use kube::core::WatchEvent;
use kube::{Api, Client};
use log::{error, info, warn};
use sdp_common::crd::{DeviceId, ServiceIdentity};
use sdp_common::service::{HasCredentials, ServiceCandidate};
use tokio::sync::mpsc::{Receiver, Sender};

#[derive(Debug)]
pub enum ServiceIdentityWatcherProtocol {
    DeviceIdManagerReady,
}

pub struct ServiceIdentityWatcher<D: ServiceCandidate + HasCredentials> {
    service_identity_api: Api<D>,
}

impl<'a> ServiceIdentityWatcher<ServiceIdentity> {
    async fn init(&self, mut receiver: Receiver<ServiceIdentityWatcherProtocol>) {
        info!("Waiting for DeviceIdManager to be ready");
        while let Some(message) = receiver.recv().await {
            match message {
                ServiceIdentityWatcherProtocol::DeviceIdManagerReady => {
                    info!("DeviceIdManager is ready. Starting ServiceIdentityWatcher.");
                    break;
                }
            }
        }
    }

    async fn watch_service_identity(
        &self,
        sender: Sender<DeviceIdManagerProtocol<ServiceIdentity, DeviceId>>,
    ) {
        info!("Starting ServiceIdentity Watcher");
        let tx = &sender;
        let xs = self.service_identity_api.clone().watch(&ListParams::default(), "0").await;
        if let Err(e) = xs {
            let err_str = format!(
                "Unable to create deployment watcher: {}. Exiting.",
                e.to_string()
            );
            error!("{}", err_str);
            panic!("{}", err_str);   
        }
        let mut xs = xs.unwrap().boxed();
        loop {
            match xs.try_next().await.expect("Error watching for service identity events") {
                Some(WatchEvent::Added(service_identity)) => {
                    if let Err(err) = tx
                        .send(DeviceIdManagerProtocol::FoundServiceIdentity {
                            service_identity_ref: service_identity,
                        })
                        .await
                    {
                        error!("Error requesting new DeviceId: {}", err);
                    }
                }
                Some(ev) => {
                    warn!("Ignored event: {:?}", ev);
                }
                None => {}
            }
        }
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
