use futures::{StreamExt, TryStreamExt};
use k8s_openapi::api::apps::v1::Deployment;
use k8s_openapi::Resource;
use kube::{
    api::ListParams,
    runtime::watcher::{self, Event},
    Api,
};
use log::{error, info};
use sdp_common::watcher::SimpleWatchingProtocol;
use sdp_common::{crd::ServiceIdentity, traits::Candidate, traits::Service};
use sdp_macros::when_ok;
use serde::de::DeserializeOwned;
use std::fmt::Debug;
use tokio::sync::mpsc::{Receiver, Sender};

use crate::identity_manager::IdentityManagerProtocol;

#[derive(Debug)]
pub enum DeploymentWatcherProtocol {
    IdentityManagerReady,
}

pub struct Watcher<E, P, R>
where
    E: Clone + Debug + Send + DeserializeOwned + Resource + SimpleWatchingProtocol<P> + 'static,
{
    pub api: Api<E>,
    pub queue_tx: Sender<P>,
    pub queue_rx: Receiver<R>,
}

impl
    Watcher<
        Deployment,
        IdentityManagerProtocol<Deployment, ServiceIdentity>,
        DeploymentWatcherProtocol,
    >
{
    pub async fn watch(&mut self) {
        info!("Initializing Deployment Watcher");
        let xs = self.api.list(&ListParams::default()).await;
        if let Err(e) = xs {
            let err_str = format!("Unable to list current deployments: {}", e.to_string());
            error!("{}", err_str);
            panic!("{}", err_str);
        }

        for candidate in xs.unwrap().items.iter().filter(|c| c.is_candidate()) {
            if let Some(msg) = candidate.initialize() {
                if let Err(e) = self.queue_tx.send(msg).await {
                    error!("Error sending Initialized message: {}", e.to_string())
                }
            }
        }

        info!("Deployment Watcher is ready to watch");
        let msg = IdentityManagerProtocol::DeploymentWatcherReady;
        if let Err(err) = self.queue_tx.send(msg).await {
            error!("Error sending DeploymentWatcherReady: {}", err);
        }

        info!("Waiting for IdentityManager to be ready");
        while let Some(msg) = self.queue_rx.recv().await {
            match msg {
                DeploymentWatcherProtocol::IdentityManagerReady => {
                    info!("Identity Manager is ready");
                    break;
                }
            }
        }

        info!("Watching deployments");
        let xs = watcher::watcher(self.api.clone(), ListParams::default());
        let mut xs = xs.boxed();
        loop {
            match xs.try_next().await {
                Ok(Some(Event::Applied(e))) => {
                    if e.is_candidate() {
                        if let Some(msg) = e.applied() {
                            if let Err(e) = self.queue_tx.send(msg).await {
                                error!("Error sending Applied message: {}", e.to_string())
                            }
                        }
                    }
                }
                Ok(Some(Event::Deleted(e))) => {
                    if e.is_candidate() {
                        if let Some(msg) = e.deleted() {
                            if let Err(e) = self.queue_tx.send(msg).await {
                                error!("Error sending Deleted message: {}", e.to_string())
                            }
                        }
                    }
                }
                Ok(Some(Event::Restarted(_))) => {}
                Ok(None) => {}
                Err(e) => {
                    error!("Error reading watcher events: {}", e.to_string());
                }
            }
        }
    }
}

impl SimpleWatchingProtocol<IdentityManagerProtocol<Deployment, ServiceIdentity>> for Deployment {
    fn initialize(&self) -> Option<IdentityManagerProtocol<Deployment, ServiceIdentity>> {
        when_ok!((service_id:IdentityManagerProtocol<Deployment, ServiceIdentity> = self.service_id()) {
            info!("Found service candidate: {}", service_id);
            Some(IdentityManagerProtocol::FoundServiceCandidate(self.clone()))
        })
    }

    fn applied(&self) -> Option<IdentityManagerProtocol<Deployment, ServiceIdentity>> {
        when_ok!((service_id:IdentityManagerProtocol<Deployment, ServiceIdentity> = self.service_id()) {
            info!("Applied Deployment {}", service_id);
            Some(IdentityManagerProtocol::RequestServiceIdentity {
                service_candidate: self.clone(),
            })
        })
    }

    fn deleted(&self) -> Option<IdentityManagerProtocol<Deployment, ServiceIdentity>> {
        when_ok!((service_id:IdentityManagerProtocol<Deployment, ServiceIdentity> = self.service_id()) {
            info!("Deleted Deployment {}", service_id);
            Some(IdentityManagerProtocol::DeletedServiceCandidate(self.clone()))
        })
    }
}
