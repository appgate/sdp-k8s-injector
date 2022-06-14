use std::{borrow::BorrowMut, collections::VecDeque};

use futures::StreamExt;
use k8s_openapi::api::apps::v1::Deployment;
use kube::{api::ListParams, Api, Client};
use kube_runtime::watcher;
use kube_runtime::watcher::Event;
use log::{error, info, warn};
use tokio::sync::mpsc::{channel, Receiver, Sender};

pub use crate::sdp;

use self::sdp::Credentials;

#[derive(Clone)]
pub struct ServiceIdentity {
    secret: String,
    field: String,
}

/// Messages exchanged between different components
pub enum IdentityMessageRequest {
    /// Message used to request a new user
    RequestIdentity {
        service_name: String,
        service_ns: String,
        channel: Sender<IdentityMessageResponse>,
    },
    CredentialsCreated(Credentials),
}

pub enum IdentityMessageResponse {
    /// Message used to send a new user
    NewIdentity(ServiceIdentity),
    IdentityUnavailable,
}

pub enum IdentityCreatorMessage {
    /// Message used to send a new user
    CreateIdentity,
    DeleteIdentity,
}

pub struct IdentityManager {
    pool: VecDeque<ServiceIdentity>,
}

impl IdentityManager {
    fn new() -> IdentityManager {
        IdentityManager {
            pool: VecDeque::with_capacity(30),
        }
    }

    fn next_identity(&mut self) -> Option<ServiceIdentity> {
        self.pool.pop_front().map(|id| id.clone())
    }

    async fn run(&mut self, mut rx: Receiver<IdentityMessageRequest>) -> () {
        while let Some(msg) = rx.recv().await {
            match msg {
                IdentityMessageRequest::RequestIdentity {
                    service_name,
                    service_ns,
                    channel,
                } => {
                    info!(
                        "New user requested for service {}[{}]",
                        service_name, service_ns
                    );
                    let event = self
                        .next_identity()
                        .map(|id| IdentityMessageResponse::NewIdentity(id))
                        .unwrap_or_else(|| {
                            warn!("Unable to get next ServiceIdentity");
                            IdentityMessageResponse::IdentityUnavailable
                        });
                    match channel.send(event).await {
                        Ok(_) => {
                            info!("New ServiceIdentity dispatched");
                        }
                        Err(err) => {
                            error!("Found error when notifying ServiceIdentify: {}", err);
                        }
                    };
                }
                IdentityMessageRequest::CredentialsCreated(credentials) => {
                    // TODO: Convert Crendetials => ServiceIdentity
                    self.pool.push_back(ServiceIdentity {
                        secret: "THESECRET".to_string(),
                        field: "THEFIELD".to_string(),
                    });
                }
            }
        }
    }

    async fn initialize(&mut self) -> () {
        ()
    }

    async fn identity_creator(
        system: sdp::System,
        mut rx: Receiver<IdentityCreatorMessage>,
        mut tx: Sender<IdentityMessageRequest>,
    ) -> () {
        while let Some(message) = rx.recv().await {
            match message {
                CreateIdentity => {
                    match system.create_user().await {
                        Ok(credentials) => {
                            if let Err(err) = tx
                                .send(IdentityMessageRequest::CredentialsCreated(credentials))
                                .await
                            {
                                error!("Error notifying Credentials: {}", err);
                                // TODO: Try later, credentials are already created
                            }
                        }
                        Err(err) => {
                            error!("Error creating new Credentials: {}", err);
                        }
                    };
                }
                DeleteIdentity => match system.delete_user().await {
                    Ok(credentials) => {
                        info!("Credentials deleted!");
                    }
                    Err(err) => {
                        error!("Unable to delete credentials");
                    }
                },
            }
        }
    }
}

async fn run() -> Sender<IdentityMessageRequest> {
    let (tx, mut rx) = channel::<IdentityMessageRequest>(50);
    let mut identity_manager = IdentityManager::new();
    tokio::spawn(async move {
        identity_manager.borrow_mut().run(rx).await;
    });
    tx
}

pub struct DeploymentWatcher;

async fn watch_deployments(client: Client) -> () {
    let deployments_api: Api<Deployment> = Api::all(client);
    watcher(deployments_api, ListParams::default())
        .for_each_concurrent(5, |res| async move {
            match res {
                Ok(Event::Restarted(deployments)) => {
                    println!("Watcher restarted");
                }
                Ok(Event::Applied(deployment)) => {
                    println!("New deployment or modified deployment");
                }
                Ok(Event::Deleted(deployment)) => {
                    println!("Deleted deployment");
                }
                Err(err) => {
                    println!("Some error")
                }
            }
        })
        .await
}

impl DeploymentWatcher {
    async fn run() -> () {
        tokio::spawn(async move {
            let client = Client::try_default()
                .await
                .expect("Unable to create K8S client");
            watch_deployments(client);
        });
    }
}
