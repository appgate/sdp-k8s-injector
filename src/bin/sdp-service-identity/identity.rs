use std::{
    borrow::BorrowMut,
    collections::{HashMap, VecDeque},
};

use futures::StreamExt;
use k8s_openapi::api::apps::v1::Deployment;
use kube::{api::ListParams, Api, Client};
use kube_derive::CustomResource;
use kube_runtime::watcher;
use kube_runtime::watcher::Event;
use log::{error, info, warn};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc::{channel, Receiver, Sender};

pub use crate::sdp;

use self::sdp::Credentials;

#[derive(CustomResource, Serialize, Deserialize, Clone, Debug, JsonSchema)]
#[kube(group = "sdp", version = "v1", kind = "ServiceIdentity", namespaced)]
pub struct ServiceIdentitySpec {
    credentials: ServiceCredentials,
    name: String,
    namespace: String,
}

impl ServiceIdentity {
    pub fn name(&self) -> String {
        format!("{}-{}", self.spec.namespace, self.spec.name).to_string()
    }
}

#[derive(Clone, JsonSchema, Debug, Serialize, Deserialize)]
pub struct ServiceCredentials {
    secret: String,
    field: String,
}

/// Messages exchanged between different components
pub enum IdentityManagerProtocol {
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
    NewIdentity(ServiceCredentials),
    IdentityUnavailable,
}

pub enum IdentityCreatorMessage {
    /// Message used to send a new user
    CreateIdentity,
    DeleteIdentity,
}

pub struct IdentityManager {
    pool: VecDeque<ServiceCredentials>,
    service_identity_api: Api<ServiceIdentity>,
    services: HashMap<String, ServiceIdentity>,
}

impl IdentityManager {
    fn new(api: Api<ServiceIdentity>) -> IdentityManager {
        IdentityManager {
            pool: VecDeque::with_capacity(30),
            service_identity_api: api,
            services: HashMap::new(),
        }
    }

    fn next_identity(&mut self) -> Option<ServiceCredentials> {
        self.pool.pop_front().map(|id| id.clone())
    }

    async fn run(&mut self, mut rx: Receiver<IdentityManagerProtocol>) -> () {
        while let Some(msg) = rx.recv().await {
            match msg {
                IdentityManagerProtocol::RequestIdentity {
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
                            error!("Found error when notifying ServiceIdentity: {}", err);
                        }
                    };
                }
                IdentityManagerProtocol::CredentialsCreated(credentials) => {
                    // TODO: Convert Crendetials => ServiceIdentity
                    self.pool.push_back(ServiceCredentials {
                        secret: "THESECRET".to_string(),
                        field: "THEFIELD".to_string(),
                    });
                },
            }
        }
    }

    async fn initialize(&mut self) -> () {
        match self.service_identity_api.list(&ListParams::default()).await {
            Ok(xs) => {
                xs.iter().for_each(|s| {
                    self.services.insert(s.name(), s.clone());
                });
            }
            Err(err) => {
                error!("Error fetching list of current ServiceIdentity: {}", err);
            }
        }
    }

    async fn identity_creator(
        system: sdp::System,
        mut rx: Receiver<IdentityCreatorMessage>,
        mut tx: Sender<IdentityManagerProtocol>,
    ) -> () {
        while let Some(message) = rx.recv().await {
            match message {
                CreateIdentity => {
                    match system.create_user().await {
                        Ok(credentials) => {
                            if let Err(err) = tx
                                .send(IdentityManagerProtocol::CredentialsCreated(credentials))
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

async fn run() -> Sender<IdentityManagerProtocol> {
    let (tx, mut rx) = channel::<IdentityManagerProtocol>(50);
    let client = Client::try_default()
        .await
        .expect("Unable to create K8S client");
    let api: Api<ServiceIdentity> = Api::namespaced(client, "sdp-system");
    let mut identity_manager = IdentityManager::new(api);
    tokio::spawn(async move {
        identity_manager.initialize().await;
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
            watch_deployments(client).await;
        });
    }
}
