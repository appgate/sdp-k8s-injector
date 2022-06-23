use futures::StreamExt;
use k8s_openapi::api::apps::v1::Deployment;
use kube::{
    api::ListParams,
    runtime::watcher::{self, Event},
    Api, Client,
};
use log::{debug, error, info, warn};
use tokio::sync::mpsc::{Receiver, Sender};

use crate::identity_manager::{IdentityManagerProtocol, ServiceCandidate};

#[derive(Debug)]
pub enum DeploymentWatcherProtocol {
    IdentityManagerReady,
}

pub struct DeploymentWatcher<D: ServiceCandidate> {
    deployment_api: Api<D>,
}

impl<'a> DeploymentWatcher<Deployment> {
    pub fn new(client: Client) -> Self {
        let deployment_api: Api<Deployment> = Api::all(client);
        DeploymentWatcher {
            deployment_api: deployment_api,
        }
    }

    pub async fn initialize(&self, mut queue: Receiver<DeploymentWatcherProtocol>) -> () {
        info!("Waiting for IdentityManager to be ready!");
        while let Some(msg) = queue.recv().await {
            if let DeploymentWatcherProtocol::IdentityManagerReady = msg {
                info!("IdentityManager is ready, starting DeploymentWatcher!");
                break;
            } else {
                warn!("Ignore message, waiting for IdentityManager to be ready!");
            }
        }
    }

    pub async fn watch_deployments(
        &self,
        queue: Sender<IdentityManagerProtocol<Deployment>>,
    ) -> () {
        info!("Starting Deployments watcher!");
        let tx = &queue;
        watcher::watcher(self.deployment_api.clone(), ListParams::default())
            .for_each_concurrent(5, |res| async move {
                match res {
                    Ok(Event::Restarted(deployments)) => {
                        for deployment in deployments {
                            if deployment.is_candidate() {
                                info!("Found new service candidate: {}", deployment.service_id());
                                let msg = IdentityManagerProtocol::RequestIdentity {
                                    service_candidate: deployment,
                                };
                                if let Err(err) = tx.send(msg).await {
                                    error!("Error requesting new ServiceIdentity: {}", err);
                                }
                            }
                        }
                    }
                    Ok(Event::Applied(deployment)) if deployment.is_candidate() => {
                        info!("Found new service candidate: {}", deployment.service_id());
                        if let Err(err) = tx
                            .send(IdentityManagerProtocol::RequestIdentity {
                                service_candidate: deployment,
                            })
                            .await
                        {
                            error!("Error requesting new ServiceIdentity: {}", err);
                        }
                    }
                    Ok(Event::Applied(deployment)) => {
                        debug!(
                            "Ignoring service not being candidate {}",
                            deployment.service_id()
                        );
                    }
                    Ok(Event::Deleted(deployment)) if deployment.is_candidate() => {
                        info!("Deleted service candidate {}", deployment.service_id());
                    }
                    Ok(Event::Deleted(deployment)) => {
                        debug!(
                            "Ignoring service not being candidate {}",
                            deployment.service_id()
                        );
                    }
                    Err(err) => {
                        error!("Some error: {}", err);
                    }
                }
            })
            .await
    }

    pub async fn run(
        &self,
        receiver: Receiver<DeploymentWatcherProtocol>,
        sender: Sender<IdentityManagerProtocol<Deployment>>,
    ) -> () {
        self.initialize(receiver).await;
        self.watch_deployments(sender).await;
    }
}
