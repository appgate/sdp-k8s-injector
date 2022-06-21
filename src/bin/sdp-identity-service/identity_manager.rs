use k8s_openapi::api::{apps::v1::Deployment, core::v1::Secret};
use kube::api::{ListParams, PostParams};
use kube::{Api, Client, CustomResource, ResourceExt};
use log::{error, info};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, VecDeque};
use tokio::sync::mpsc::{Receiver, Sender};

use crate::identity_creator::{IdentityCreatorMessage, UserCredentialsRef};
pub use crate::sdp;

use self::sdp::ServiceUser;

#[derive(Debug, CustomResource, Serialize, Deserialize, Clone, JsonSchema)]
#[kube(
    group = "injector.sdp.com",
    version = "v1",
    kind = "ServiceIdentity",
    namespaced
)]
pub struct ServiceIdentitySpec {
    service_identity: UserCredentialsRef,
    service_name: String,
    service_namespace: String,
    labels: Vec<String>,
    disabled: bool,
}

pub trait ServiceCandidate {
    fn service_id(&self) -> String;
}

fn derive_service_id(name: &str, namespace: &str) -> String {
    format!("{}-{}", namespace, name).to_string()
}

impl ServiceCandidate for ServiceIdentity {
    fn service_id(&self) -> String {
        derive_service_id(&self.spec.service_namespace, &self.spec.service_name)
    }
}

impl ServiceCandidate for Deployment {
    fn service_id(&self) -> String {
        let name = self.name();
        let ns = self.namespace().unwrap_or("default".to_string());
        derive_service_id(&name, &ns)
    }
}

/// Messages exchanged between different components
pub enum IdentityManagerProtocol {
    /// Message used to request a new user
    RequestIdentity {
        service_name: String,
        service_ns: String,
    },
    IdentityCreated {
        user_credentials_ref: UserCredentialsRef,
    },
}

pub enum IdentityMessageResponse {
    /// Message used to send a new user
    NewIdentity(UserCredentialsRef),
    IdentityUnavailable,
}

pub struct IdentityManager {
    pool: VecDeque<UserCredentialsRef>,
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
            None => self.pool.pop_front().map(|service_identity| {
                let service_identity_spec = ServiceIdentitySpec {
                    service_name: name.clone(),
                    service_namespace: namespace.clone(),
                    service_identity: service_identity,
                    labels: vec![],
                    disabled: false,
                };
                ServiceIdentity::new(&service_id, service_identity_spec)
            }),
            Some(c) => None,
        }
    }

    async fn run_identity_manager(
        &mut self,
        mut receiver: Receiver<IdentityManagerProtocol>,
        sender: Sender<IdentityCreatorMessage>,
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
                                    if let Err(err) =
                                        sender.send(IdentityCreatorMessage::CreateIdentity).await
                                    {
                                        error!("Error when sending IdentityCreatorMessage::CreateIdentity: {}", err);
                                    }
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
                IdentityManagerProtocol::IdentityCreated {
                    user_credentials_ref,
                } => {
                    self.pool.push_back(user_credentials_ref);
                }
            }
        }
    }

    async fn initialize(&mut self) -> () {
        info!("Initializing Identity Manager ...");
        match self.service_identity_api.list(&ListParams::default()).await {
            Ok(xs) => {
                info!("Restoring previous service identities");
                let n: u32 = xs
                    .iter()
                    .map(|s| {
                        info!("Restoring service identity {}", s.service_id());
                        self.services.insert(s.name(), s.clone());
                        1
                    })
                    .sum();
                info!("Restored {} previous service identities", n);
            }
            Err(err) => {
                panic!("Error fetching list of current ServiceIdentity: {}", err);
            }
        }
    }

    pub async fn run(
        &mut self,
        receiver: Receiver<IdentityManagerProtocol>,
        sender: Sender<IdentityCreatorMessage>,
    ) -> () {
        self.initialize().await;
        self.run_identity_manager(receiver, sender).await;
    }
}
