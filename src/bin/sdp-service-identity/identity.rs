use std::{
    borrow::BorrowMut,
    collections::{HashMap, VecDeque},
};

use futures::StreamExt;
use k8s_openapi::api::apps::v1::Deployment;
use kube::{
    api::{ListParams, PostParams},
    Api, Client, ResourceExt,
};
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
#[kube(
    group = "injector.sdp.com",
    version = "v1",
    kind = "ServiceIdentity",
    namespaced
)]
pub struct ServiceIdentitySpec {
    service_credentials: ServiceCredentials,
    service_name: String,
    service_namespace: String,
    service_device_id: Option<String>,
}

pub trait ServiceCandidate {
    fn service_id(&self) -> Option<String>;
}

fn derive_service_id(name: &str, namespace: &str) -> String {
    format!("{}-{}", namespace, name).to_string()
}

impl ServiceCandidate for ServiceIdentity {
    fn service_id(&self) -> Option<String> {
        Some(derive_service_id(
            &self.spec.service_namespace,
            &self.spec.service_name,
        ))
    }
}

impl ServiceCandidate for Deployment {
    fn service_id(&self) -> Option<String> {
        self.metadata.namespace.as_ref().and_then(|ns| {
            self.metadata
                .name
                .as_ref()
                .map(|n| derive_service_id(&ns, &n))
        })
    }
}

#[derive(Clone, JsonSchema, Debug, Serialize, Deserialize)]
pub struct ServiceCredentials {
    secret: String,
    field: String,
}

impl Default for ServiceCredentials {
    fn default() -> Self {
        Self {
            secret: "sdp-service-secrets".to_string(),
            field: "sdp-default-secret".to_string(),
        }
    }
}

/// Messages exchanged between different components
pub enum IdentityManagerProtocol {
    /// Message used to request a new user
    RequestIdentity {
        service_name: String,
        service_ns: String,
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
    pub fn new(client: Client) -> IdentityManager {
        let service_identity_api: Api<ServiceIdentity> = Api::namespaced(client, "sdp-system");
        IdentityManager {
            pool: VecDeque::with_capacity(30),
            service_identity_api: service_identity_api,
            services: HashMap::new(),
        }
    }

    fn next_identity(&mut self, name: &String, namespace: &String) -> Option<ServiceIdentity> {
        let service_id = derive_service_id(name, namespace);
        match self.services.get(&service_id) {
            None => self.pool.pop_front().map(|service_credentials| {
                let service_identity_spec = ServiceIdentitySpec {
                    service_name: name.clone(),
                    service_namespace: name.clone(),
                    service_credentials: service_credentials,
                    service_device_id: None,
                };
                ServiceIdentity::new(&service_id, service_identity_spec)
            }),
            Some(c) => None,
        }
    }

    async fn run_identity_manager(
        &mut self,
        mut receiver: Receiver<IdentityManagerProtocol>,
    ) -> () {
        info!("Running Identity Manager ...");
        while let Some(msg) = receiver.recv().await {
            match msg {
                IdentityManagerProtocol::RequestIdentity {
                    service_name,
                    service_ns,
                } => {
                    let service_id = derive_service_id(&service_name, &service_ns);
                    info!("New user requested for service {}", service_id);
                    match self.next_identity(&service_name, &service_ns) {
                        Some(identity) => {
                            match self
                                .service_identity_api
                                .create(&PostParams::default(), &identity)
                                .await
                            {
                                Ok(_) => {
                                    info!(
                                        "New ServiceIdentity created for service with id {}",
                                        service_id
                                    );
                                }
                                Err(err) => {
                                    error!(
                                        "Error creating ServiceIdentity for service with id {}: {}",
                                        service_id, err
                                    );
                                }
                            }
                        }
                        None => {
                            error!("Unable to assign service identity for service {}. Identities pool seems to be empty!", service_id);
                        }
                    };
                }
                IdentityManagerProtocol::CredentialsCreated(credentials) => {
                    // TODO: Convert Crendetials => ServiceIdentity
                    self.pool.push_back(ServiceCredentials {
                        secret: "THESECRET".to_string(),
                        field: "THEFIELD".to_string(),
                    });
                }
            }
        }
    }

    async fn initialize(&mut self) -> () {
        info!("Initializing Identity Manager ...");
        match self.service_identity_api.list(&ListParams::default()).await {
            Ok(xs) => {
                info!("Restoring previous service identities");
                let mut n = 0;
                xs.iter().for_each(|s| {
                    if let Some(service_id) = s.service_id() {
                        n += 1;
                        info!("Restoring service identity {}", service_id);
                        self.services.insert(s.name(), s.clone());
                    } else {
                        error!("Unable to restore service identity {}", s.name());
                    }
                });
                info!("Restored {} previous service identities", n);
            }
            Err(err) => {
                panic!("Error fetching list of current ServiceIdentity: {}", err);
            }
        }
    }

    async fn run_identity_creator(
        self,
        system: sdp::System,
        mut rx: Receiver<IdentityCreatorMessage>,
        tx: Sender<IdentityManagerProtocol>,
    ) -> () {
        while let Some(message) = rx.recv().await {
            match message {
                IdentityCreatorMessage::CreateIdentity => {
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
                IdentityCreatorMessage::DeleteIdentity => match system.delete_user().await {
                    Ok(_credentials) => {
                        info!("Credentials deleted!");
                    }
                    Err(err) => {
                        error!("Unable to delete credentials: {}", err);
                    }
                },
            }
        }
    }

    pub async fn run(&mut self, receiver: Receiver<IdentityManagerProtocol>) -> () {
        self.initialize().await;
        self.run_identity_manager(receiver).await;
    }
}

pub struct DeploymentWatcher {
    deployment_api: Api<Deployment>,
}

impl<'a> DeploymentWatcher {
    pub fn new(client: Client) -> Self {
        let deployment_api: Api<Deployment> = Api::all(client);
        DeploymentWatcher {
            deployment_api: deployment_api,
        }
    }

    pub async fn watch_deployments(self, queue: Sender<IdentityManagerProtocol>) -> () {
        info!("Starting Deployments watcher!");
        let tx = &queue;
        watcher(self.deployment_api, ListParams::default())
            .for_each_concurrent(5, |res| async move {
                match res {
                    Ok(Event::Restarted(deployments)) => {
                        for deployment in deployments {
                            info!("Found new service candidate: {}", deployment.service_id().unwrap());
                            if let Err(err) = tx
                                .send(IdentityManagerProtocol::RequestIdentity {
                                    service_name: deployment.name(),
                                    service_ns: deployment.namespace().unwrap(),
                                }).await
                            {
                                error!("Error requesting new ServiceIdentity")
                            }
                        }
                    }
                    Ok(Event::Applied(deployment)) if deployment.service_id().is_some() => {
                        info!("Found new service candidate: {}", deployment.service_id().unwrap());
                        if let Err(err) = tx
                            .send(IdentityManagerProtocol::RequestIdentity {
                                service_name: deployment.name(),
                                service_ns: deployment.namespace().unwrap(),
                            })
                            .await
                        {
                            error!("Error requesting new ServiceIdentity")
                        }
                    }
                    Ok(Event::Applied(deployment)) => {
                        info!("Ignoring service {}, not a candidate since we can not guess the service id", deployment.name());
                    }
                    Ok(Event::Deleted(deployment)) if deployment.service_id().is_some() => {
                        info!("Deleted service candidate {}", deployment.service_id().unwrap());
                    }
                    Ok(Event::Deleted(deployment)) => {
                        info!("Ignoring deleted service {}", deployment.name());
                    }
                    Err(err) => {
                        error!("Some error: {}", err);
                    }
                }
            })
            .await
    }
}
