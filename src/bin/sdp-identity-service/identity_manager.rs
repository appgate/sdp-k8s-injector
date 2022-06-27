use k8s_openapi::api::apps::v1::Deployment;
use kube::api::{ListParams, PostParams};
use kube::{Api, Client, CustomResource, ResourceExt};
use log::{error, info, warn};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, VecDeque};
use tokio::sync::mpsc::{Receiver, Sender};

use crate::deployment_watcher::DeploymentWatcherProtocol;
use crate::identity_creator::{IdentityCreatorProtocol, ServiceCredentialsRef};
pub use crate::sdp;

/// ServiceIdentity CRD
/// This is the CRD where we store the credentials for the services
#[derive(Debug, CustomResource, Serialize, Deserialize, Clone, JsonSchema)]
#[kube(
    group = "injector.sdp.com",
    version = "v1",
    kind = "ServiceIdentity",
    namespaced
)]

/// Spec for ServiceIdentity CRD
/// This CRD defines the credentials and the labels used by a specific k8s service
/// The credentials are stored in a k8s secret entity
/// The labels in the service are used to determine what kind of access the service
///   will have
/// service_namespace + service_name indentify each service
pub struct ServiceIdentitySpec {
    service_crendetials_ref: ServiceCredentialsRef,
    service_name: String,
    service_namespace: String,
    labels: Vec<String>,
    disabled: bool,
}

/// Trait that defines entities that are candidates to be services
/// Basically a service candidate needs to be able to define :
///  - namespace
///  - name
/// and the combination of both needs to be unique
pub trait ServiceCandidate {
    fn name(&self) -> String;
    fn namespace(&self) -> String;
    fn is_candidate(&self) -> bool;
    fn service_id(&self) -> String {
        format!("{}-{}", self.namespace(), self.name()).to_string()
    }
}

/// ServiceIdentity is a ServiceCandidate by definition :D
impl ServiceCandidate for ServiceIdentity {
    fn name(&self) -> String {
        self.spec.service_name.clone()
    }

    fn namespace(&self) -> String {
        self.spec.service_namespace.clone()
    }

    fn is_candidate(&self) -> bool {
        true
    }
}

/// Deployment are the main source of ServiceCandidate
/// Final ServiceIdentity are created from Deployments
impl ServiceCandidate for Deployment {
    fn name(&self) -> String {
        ResourceExt::name(self)
    }

    fn namespace(&self) -> String {
        ResourceExt::namespace(self).unwrap_or("default".to_string())
    }

    fn is_candidate(&self) -> bool {
        ServiceCandidate::namespace(self) == "some-ns"
    }
}

/// Messages exchanged between the the IdentityCreator and IdentityManager
#[derive(Debug)]
pub enum IdentityManagerProtocol<D: ServiceCandidate> {
    /// Message used to request a new ServiceIdentity for ServiceCandidate
    RequestIdentity {
        service_candidate: D,
    },
    /// Message to notify that a new ServiceCredential have been created
    /// IdentityCreator creates these ServiceCredentials
    NewServiceCredentials {
        user_credentials_ref: ServiceCredentialsRef,
    },
    NewActiveServiceCredentials {
        user_credentials_ref: ServiceCredentialsRef,
    },
    IdentityCreatorReady,
}

pub enum IdentityMessageResponse {
    /// Message used to send a new user
    NewIdentity(ServiceCredentialsRef),
    IdentityUnavailable,
}

pub struct IdentityManager {
    pool: VecDeque<ServiceCredentialsRef>,
    service_identity_api: Api<ServiceIdentity>,
    services: HashMap<String, ServiceIdentity>,
}

/// Trait that represents the pool of ServiceCredential entities
/// We can pop and push ServiceCredential entities
trait ServiceCredentialsPool {
    fn pop(&mut self) -> Option<ServiceCredentialsRef>;
    fn push(&mut self, user_credentials_ref: ServiceCredentialsRef) -> ();
}

/// Trait for ServiceIdentity provider
/// This traits provides instances of To from instances of From
trait ServiceIdentityProvider {
    type From: ServiceCandidate;
    type To: ServiceCandidate;
    fn next_identity(&mut self, from: Self::From) -> Option<Self::To>;
}

impl ServiceCredentialsPool for IdentityManager {
    fn pop(&mut self) -> Option<ServiceCredentialsRef> {
        self.pool.pop_front()
    }

    fn push(&mut self, user_credentials_ref: ServiceCredentialsRef) -> () {
        self.pool.push_back(user_credentials_ref)
    }
}

impl ServiceIdentityProvider for IdentityManager {
    type From = Deployment;
    type To = ServiceIdentity;

    fn next_identity(&mut self, from: Deployment) -> Option<ServiceIdentity> {
        //let service_id = derive_service_id(name, namespace);
        match self.services.get(&from.service_id()) {
            None => self.pop().map(|service_crendetials_ref| {
                let service_identity_spec = ServiceIdentitySpec {
                    service_name: ServiceCandidate::name(&from),
                    service_namespace: ServiceCandidate::name(&from),
                    service_crendetials_ref: service_crendetials_ref,
                    labels: vec![],
                    disabled: false,
                };
                ServiceIdentity::new(&from.service_id(), service_identity_spec)
            }),
            Some(_) => None,
        }
    }
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

    async fn run_identity_manager(
        &mut self,
        mut identity_manager_rx: Receiver<IdentityManagerProtocol<Deployment>>,
        identity_creator_tx: Sender<IdentityCreatorProtocol>,
        deployment_watcher_proto_tx: Sender<DeploymentWatcherProtocol>,
    ) -> () {
        info!("Running Identity Manager ...");
        while let Some(msg) = identity_manager_rx.recv().await {
            match msg {
                IdentityManagerProtocol::RequestIdentity { service_candidate } => {
                    let service_id = service_candidate.service_id();
                    info!("New user requested for service {}", service_id);
                    match self.next_identity(service_candidate) {
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
                                    if let Err(err) = identity_creator_tx
                                        .send(IdentityCreatorProtocol::CreateIdentity)
                                        .await
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
                IdentityManagerProtocol::NewServiceCredentials {
                    user_credentials_ref,
                } => {
                    info!(
                        "Push new UserCredentialRef with id {}",
                        user_credentials_ref.id
                    );
                    self.push(user_credentials_ref);
                }
                IdentityManagerProtocol::IdentityCreatorReady => {
                    info!("IdentityCreator is ready!");
                    let msg = DeploymentWatcherProtocol::IdentityManagerReady;
                    deployment_watcher_proto_tx
                        .send(msg)
                        .await
                        .expect("Unable to notify DeploymentWatcher!");
                }
                _ => {
                    warn!("Ignored message");
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
                        self.services.insert(ServiceCandidate::name(s), s.clone());
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
        identity_manager_prot_rx: Receiver<IdentityManagerProtocol<Deployment>>,
        identity_creater_proto_tx: Sender<IdentityCreatorProtocol>,
        deployment_watcher_proto_tx: Sender<DeploymentWatcherProtocol>,
    ) -> () {
        self.initialize().await;
        self.run_identity_manager(
            identity_manager_prot_rx,
            identity_creater_proto_tx,
            deployment_watcher_proto_tx,
        )
        .await;
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn test_identity_manager_1() {}
}
