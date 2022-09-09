use std::time::Duration;

use futures::{StreamExt, TryStreamExt};
use k8s_openapi::api::apps::v1::Deployment;
use kube::{
    api::ListParams,
    runtime::watcher::{self, Event},
    Api, Client, core::{watch, WatchEvent},
};
use log::{debug, error, info, warn};
use schemars::_private::NoSerialize;
use sdp_common::crd::ServiceIdentity;
use sdp_common::service::ServiceCandidate;
use tokio::{
    sync::mpsc::{Receiver, Sender},
    time::sleep,
};

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

    pub async fn initialize(&self, mut queue: Receiver<DeploymentWatcherProtocol>) -> () {
        info!("Waiting for IdentityManager to be ready!");
        while let Some(msg) = queue.recv().await {
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
        let xs = self.deployment_api.clone().watch(&ListParams::default(), "0").await;
        if let Err(e) = xs {
            let err_str = format!("Unable to create deployment watcher: {}. Exiting.", e.to_string());
            error!("{}", err_str);
            panic!("{}", err_str);
        }
        let mut xs = xs.unwrap().boxed();
            //.for_each_concurrent(1, |res| async move {
        loop {
            match xs.try_next().await.expect("Error!") {
                Some(WatchEvent::Added(deployment)) if deployment.is_candidate() => {
                    info!("New service candidate: {}", deployment.service_id());
                    if let Err(err) = tx
                        .send(IdentityManagerProtocol::RequestServiceIdentity {
                            service_candidate: deployment,
                        })
                        .await
                    {
                        error!("Error requesting new ServiceIdentity: {}", err);
                    }
                }
                Some(WatchEvent::Added(deployment)) => {
                    info!("Ignoring service {}, not a candidate", deployment.service_id());

                }
                Some(WatchEvent::Deleted(deployment)) if deployment.is_candidate() => {
                    info!("Deleted service candidate {}", deployment.service_id());
                }
                Some(WatchEvent::Deleted(deployment)) => {
                    debug!(
                        "Ignoring service not being candidate {}",
                        deployment.service_id()
                    );
                }
                Some(ev) => {
                    warn!("Ignored event: {:?}", ev);
                }
                None => {
                
                }
            }
        }
    }

    pub async fn run(
        &self,
        receiver: Receiver<DeploymentWatcherProtocol>,
        sender: Sender<IdentityManagerProtocol<Deployment, ServiceIdentity>>,
    ) -> () {
        self.initialize(receiver).await;
        self.watch_deployments(sender).await;
    }
}
