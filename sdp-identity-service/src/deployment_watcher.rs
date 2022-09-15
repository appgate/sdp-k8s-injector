use std::collections::HashSet;

use futures::{StreamExt, TryStreamExt};
use k8s_openapi::api::apps::v1::Deployment;
use kube::{
    api::ListParams,
    runtime::watcher::{self, Event},
    Api, Client,
};
use log::{debug, error, info, warn};
use sdp_common::crd::ServiceIdentity;
use sdp_common::service::ServiceCandidate;
use tokio::sync::mpsc::{Receiver, Sender};

use crate::identity_manager::IdentityManagerProtocol;

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

    pub async fn initialize(
        &self,
        mut q_rx: Receiver<DeploymentWatcherProtocol>,
        q_tx: Sender<IdentityManagerProtocol<Deployment, ServiceIdentity>>,
    ) -> () {
        info!("Waiting for IdentityManager to be ready!");
        let xs = self.deployment_api.list(&ListParams::default()).await;
        if let Err(e) = xs {
            let err_str = format!(
                "Unable to get current deployments: {}. Exiting.",
                e.to_string()
            );
            error!("{}", err_str);
            panic!("{}", err_str);
        }

        // First of all notify the known service candidates to the IdentityManager so we can clean up the system if needed
        for candidate in xs.unwrap().items.iter().filter(|c| c.is_candidate()) {
            info!("Found service candidate: {}", candidate.service_id());
            let msg = IdentityManagerProtocol::FoundServiceCandidate(candidate.clone());
            if let Err(err) = q_tx.send(msg).await {
                error!("Error reporting found ServiceIdentity: {}", err);
            }
        }

        // Notify IdentityManager that we are ready to proceed
        if let Err(err) = q_tx
            .send(IdentityManagerProtocol::DeploymentWatcherReady)
            .await
        {
            error!("Error reporting  {}", err);
        }

        // Wait for IdentityManager tell us to start processing events
        while let Some(msg) = q_rx.recv().await {
            match msg {
                DeploymentWatcherProtocol::IdentityManagerReady => {
                    info!("IdentityManager is ready, starting DeploymentWatcher!");
                    break;
                }
            }
        }
    }

    pub async fn watch_deployments(
        &self,
        queue: Sender<IdentityManagerProtocol<Deployment, ServiceIdentity>>,
    ) -> () {
        info!("Starting Deployments watcher!");
        let tx = &queue;
        let xs = watcher::watcher(self.deployment_api.clone(), ListParams::default());
        let mut xs = xs.boxed();
        let mut applied: HashSet<String> = HashSet::new();
        loop {
            match xs.try_next().await.expect("Error getting event!") {
                Some(Event::Applied(deployment)) if deployment.is_candidate() => {
                    let service_id = deployment.service_id().clone();
                    if !applied.contains(&service_id) {
                        info!("New service candidate: {}", &service_id);
                        if let Err(err) = tx
                            .send(IdentityManagerProtocol::RequestServiceIdentity {
                                service_candidate: deployment.clone(),
                            })
                            .await
                        {
                            error!("Error requesting new ServiceIdentity: {}", err);
                        }
                        applied.insert(service_id);
                    } else {
                        info!("Modified service candidate: {}", &service_id);
                    }
                }
                Some(Event::Applied(deployment)) => {
                    info!(
                        "Ignoring service {}, not a candidate",
                        deployment.service_id()
                    );
                }
                Some(Event::Deleted(deployment)) if deployment.is_candidate() => {
                    let service_id = deployment.service_id().clone();
                    info!("Deleted service candidate {}", &service_id);
                    if let Err(err) = tx
                        .send(IdentityManagerProtocol::DeletedServiceCandidate(
                            deployment.clone(),
                        ))
                        .await
                    {
                        error!("Error requesting new ServiceIdentity: {}", err);
                    }
                    applied.remove(&service_id);
                }
                Some(Event::Deleted(deployment)) => {
                    debug!(
                        "Ignoring service not being candidate {}",
                        deployment.service_id()
                    );
                }
                // TODO: User this event, also we can replace the for list during initalization with this
                Some(Event::Restarted(_xs)) => {
                    warn!("Ignored restarted event");
                }
                None => {}
            }
        }
    }

    pub async fn run(
        &self,
        receiver: Receiver<DeploymentWatcherProtocol>,
        sender: Sender<IdentityManagerProtocol<Deployment, ServiceIdentity>>,
    ) -> () {
        self.initialize(receiver, sender.clone()).await;
        self.watch_deployments(sender).await;
    }
}
