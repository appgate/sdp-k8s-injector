use futures::StreamExt;
use k8s_openapi::api::apps::v1::Deployment;
use kube::{
    api::ListParams,
    runtime::watcher::{self, Event},
    Api, Client,
};
use log::{error, info};
use tokio::sync::mpsc::Sender;

use crate::identity_manager::{IdentityManagerProtocol, ServiceCandidate};

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

    pub async fn watch_deployments(self, queue: Sender<IdentityManagerProtocol<Deployment>>) -> () {
        info!("Starting Deployments watcher!");
        let tx = &queue;
        watcher::watcher(self.deployment_api, ListParams::default())
            .for_each_concurrent(5, |res| async move {
                match res {
                    Ok(Event::Restarted(deployments)) => {
                        for deployment in deployments {
                            info!("Found new service candidate: {}", deployment.service_id());
                            let msg = IdentityManagerProtocol::RequestIdentity {
                                service_candidate: deployment,
                            };
                            if let Err(err) = tx.send(msg).await {
                                error!("Error requesting new ServiceIdentity: {}", err);
                            }
                        }
                    }
                    Ok(Event::Applied(deployment)) => {
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
                    Ok(Event::Deleted(deployment)) => {
                        info!("Deleted service candidate {}", deployment.service_id());
                    }
                    Err(err) => {
                        error!("Some error: {}", err);
                    }
                }
            })
            .await
    }
}
