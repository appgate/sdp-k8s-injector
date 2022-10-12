use std::collections::HashSet;

use crate::device_id_manager::DeviceIdManagerProtocol;
use futures::{StreamExt, TryStreamExt};
use kube::api::ListParams;
use kube::runtime::watcher::{self, Event};
use kube::{Api, Client};
use log::{error, info, warn};
use sdp_common::crd::{ServiceIdentity};
use sdp_common::kubernetes::SDP_K8S_NAMESPACE;
use sdp_common::traits::{HasCredentials, Service};
use sdp_macros::when_ok;
use tokio::sync::mpsc::{Receiver, Sender};

#[derive(Debug)]
pub enum ServiceIdentityWatcherProtocol {
    DeviceIdManagerReady,
}

pub struct ServiceIdentityWatcher<D: Service + HasCredentials> {
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
        sender: Sender<DeviceIdManagerProtocol<ServiceIdentity>>,
    ) {
        info!("Starting ServiceIdentity Watcher");
        let tx = &sender;
        let xs = watcher::watcher(self.service_identity_api.clone(), ListParams::default());
        let mut xs = xs.boxed();
        let mut applied: HashSet<String> = HashSet::new();
        loop {
            match xs
                .try_next()
                .await
                .expect("Error watching for service identity events")
            {
                Some(Event::Applied(service_identity)) => {
                    when_ok!((service_id = service_identity.service_id()) {
                        if !applied.contains(&service_id) {
                            if let Err(err) = tx
                                .send(DeviceIdManagerProtocol::FoundServiceIdentity {
                                    service_identity_ref: service_identity,
                                })
                                .await
                            {
                                error!("Error requesting new DeviceId: {}", err);
                            }
                            applied.insert(service_id);
                        }
                    });
                }
                Some(Event::Deleted(service_identity)) => {
                    when_ok!((service_id = service_identity.service_id()) {
                        applied.remove(&service_id);
                    });
                }
                // TODO: Use this event
                Some(Event::Restarted(_)) => {
                    warn!("Ignored restarted event");
                }
                None => {}
            }
        }
    }

    pub async fn run(
        &self,
        receiver: Receiver<ServiceIdentityWatcherProtocol>,
        sender: Sender<DeviceIdManagerProtocol<ServiceIdentity>>,
    ) {
        self.init(receiver).await;
        self.watch_service_identity(sender).await;
    }
}

impl<'a> ServiceIdentityWatcher<ServiceIdentity> {
    pub fn new(client: Client) -> Self {
        let api: Api<ServiceIdentity> = Api::namespaced(client, SDP_K8S_NAMESPACE);
        ServiceIdentityWatcher {
            service_identity_api: api,
        }
    }
}
