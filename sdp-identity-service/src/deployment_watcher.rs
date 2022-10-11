use futures::{StreamExt, TryStreamExt};
use k8s_openapi::api::apps::v1::Deployment;
use k8s_openapi::Resource;
use kube::{
    api::ListParams,
    runtime::watcher::{self, Event},
    Api,
};
use log::{error, info};
use sdp_common::errors::SDPServiceError;
use sdp_common::watcher::SimpleWatchingProtocol;
use sdp_common::{crd::ServiceIdentity, traits::Candidate, traits::Service};
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
    async fn initialize(&self) -> Result<(), SDPServiceError> {
        let xs = self.api.list(&ListParams::default()).await;
        if let Err(e) = xs {
            let err_str = format!("Unable to list current deployments: {}", e.to_string());
            error!("{}", err_str);
            panic!("{}", err_str);
        }

        // First of all notify the known service candidates to the IdentityManager so we can clean up the system if needed
        for candidate in xs.unwrap().items.iter() {
            if let Some(msg) = candidate.initialize() {
                if let Err(err) = self.queue_tx.send(msg).await {
                    error!("Error sending FoundServiceIdentity: {}", err);
                }
            }
        }
        Ok(())
    }

    async fn wait(&mut self) -> Result<DeploymentWatcherProtocol, SDPServiceError> {
        while let Some(msg) = self.queue_rx.recv().await {
            match msg {
                DeploymentWatcherProtocol::IdentityManagerReady => break,
            }
        }
        Ok(DeploymentWatcherProtocol::IdentityManagerReady)
    }

    async fn ready(
        &self,
    ) -> Result<IdentityManagerProtocol<Deployment, ServiceIdentity>, SDPServiceError> {
        Ok(IdentityManagerProtocol::DeploymentWatcherReady)
    }

    async fn watch(&self) -> () {
        let xs = watcher::watcher(self.api.clone(), ListParams::default());
        let mut xs = xs.boxed();
        loop {
            match xs.try_next().await {
                Ok(Some(Event::Applied(e))) => {
                    if let Some(msg) = e.applied() {
                        if let Err(e) = self.queue_tx.send(msg).await {
                            error!("Error sending Applied message: {}", e.to_string())
                        }
                    }
                }
                Ok(Some(Event::Deleted(e))) => {
                    if let Some(msg) = e.deleted() {
                        if let Err(e) = self.queue_tx.send(msg).await {
                            error!("Error sending Deleted message: {}", e.to_string())
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

    pub async fn start(&mut self) {
        info!("Initializing Deployment Watcher");
        if let Ok(()) = self.initialize().await {}

        if let Ok(msg) = self.ready().await {
            info!("Deployment Watcher is ready to watch");
            if let Err(err) = self.queue_tx.send(msg).await {
                error!("Error sending DeploymentWatcherReady: {}", err);
            }
        }

        info!("Waiting for IdentityManager to be ready");
        if let Ok(_) = self.wait().await {
            info!("Identity Manager is ready")
        }

        info!("Watching deployments");
        self.watch().await
    }
}

impl SimpleWatchingProtocol<IdentityManagerProtocol<Deployment, ServiceIdentity>> for Deployment {
    fn initialize(&self) -> Option<IdentityManagerProtocol<Deployment, ServiceIdentity>> {
        if self.is_candidate() {
            info!(
                "Found deployment {} at initialization",
                self.service_name().unwrap()
            );
            Some(IdentityManagerProtocol::FoundServiceCandidate(self.clone()))
        } else {
            info!(
                "Ignoring deployment {} for initialization",
                self.service_name().unwrap()
            );
            None
        }
    }

    fn applied(&self) -> Option<IdentityManagerProtocol<Deployment, ServiceIdentity>> {
        if self.is_candidate() {
            info!("Applied Deployment {}", self.service_name().unwrap());
            Some(IdentityManagerProtocol::RequestServiceIdentity {
                service_candidate: self.clone(),
            })
        } else {
            info!(
                "Ignoring applied Deployment {}, not a candidate",
                self.service_name().unwrap()
            );
            None
        }
    }

    fn deleted(&self) -> Option<IdentityManagerProtocol<Deployment, ServiceIdentity>> {
        if self.is_candidate() {
            info!("Deleted Deployment {}", self.service_name().unwrap());
            Some(IdentityManagerProtocol::DeletedServiceCandidate(
                self.clone(),
            ))
        } else {
            info!(
                "Ignoring deleted Deployment {}, not a candidate",
                self.service_name().unwrap()
            );
            None
        }
    }
}
