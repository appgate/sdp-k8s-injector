use futures::Future;
use k8s_openapi::api::apps::v1::Deployment;
use kube::api::{DeleteParams, ListParams, PostParams};
use kube::{Api, Client, CustomResource, Error as KError, ResourceExt};
use log::{error, info, warn};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet, VecDeque};
use std::pin::Pin;
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
    service_credentials: ServiceCredentialsRef,
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
        ResourceExt::namespace(self) == Some("some-ns".to_string())
        //self.annotations().get("sdp-injector").map(|v| v.eq("true")).unwrap_or(false)
    }
}

/// Messages exchanged between the the IdentityCreator and IdentityManager
#[derive(Debug)]
pub enum IdentityManagerProtocol<From: ServiceCandidate, To: ServiceCandidate> {
    /// Message used to request a new ServiceIdentity for ServiceCandidate
    RequestServiceIdentity {
        service_candidate: From,
    },
    DeleteServiceIdentity {
        service_identity: To,
    },
    FoundServiceIdentity {
        service_candidate: From,
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
    DeploymentWatcherReady,
}

pub enum IdentityMessageResponse {
    /// Message used to send a new user
    NewIdentity(ServiceCredentialsRef),
    IdentityUnavailable,
}

pub struct KubeIdentityManager {
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
    fn register_identity(&mut self, from: Self::To) -> ();
    fn next_identity(&mut self, from: Self::From) -> Option<Self::To>;
    fn has_candidate(&self, from: &Self::From) -> bool;
    fn has_identity(&self, from: &Self::To) -> bool;
    fn identities(&self) -> Vec<&Self::To>;
    fn extra_identities<'a>(&self, services: &HashSet<String>) -> Vec<&Self::To> {
        self.identities()
            .iter()
            .filter(|id| !services.contains(&id.service_id()))
            .map(|id| *id)
            .collect()
    }
}

/// This should not be needed once the GAT support is in stable
/// https://blog.rust-lang.org/2021/08/03/GATs-stabilization-push.html
trait ServiceIdentityAPI {
    fn create<'a>(
        &'a self,
        identity: &'a ServiceIdentity,
    ) -> Pin<Box<dyn Future<Output = Result<ServiceIdentity, KError>> + '_>>;
    fn delete<'a>(
        &'a self,
        identity_name: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<(), KError>> + '_>>;
    fn list<'a>(
        &'a self,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<ServiceIdentity>, KError>> + '_>>;
}

impl ServiceCredentialsPool for KubeIdentityManager {
    fn pop(&mut self) -> Option<ServiceCredentialsRef> {
        self.pool.pop_front()
    }

    fn push(&mut self, user_credentials_ref: ServiceCredentialsRef) -> () {
        self.pool.push_back(user_credentials_ref)
    }
}

impl ServiceIdentityProvider for KubeIdentityManager {
    type From = Deployment;
    type To = ServiceIdentity;

    fn register_identity(&mut self, identity: Self::To) -> () {
        self.services.insert(identity.service_id(), identity);
    }

    fn next_identity(&mut self, from: Deployment) -> Option<ServiceIdentity> {
        //let service_id = derive_service_id(name, namespace);
        match self.services.get(&from.service_id()) {
            None => self.pop().map(|service_crendetials_ref| {
                let service_identity_spec = ServiceIdentitySpec {
                    service_name: ServiceCandidate::name(&from),
                    service_namespace: ServiceCandidate::namespace(&from),
                    service_credentials: service_crendetials_ref,
                    labels: vec![],
                    disabled: false,
                };
                ServiceIdentity::new(&from.service_id(), service_identity_spec)
            }),
            Some(_) => None,
        }
    }

    fn has_candidate(&self, candidate: &Self::From) -> bool {
        self.services.contains_key(&candidate.service_id())
    }

    fn has_identity(&self, identity: &Self::To) -> bool {
        self.services.contains_key(&identity.service_id())
    }

    fn identities(&self) -> Vec<&Self::To> {
        self.services.values().collect()
    }
}

impl ServiceIdentityAPI for KubeIdentityManager {
    fn create<'a>(
        &'a self,
        identity: &'a ServiceIdentity,
    ) -> Pin<Box<dyn Future<Output = Result<ServiceIdentity, KError>> + '_>> {
        let fut = async move {
            self.service_identity_api
                .create(&PostParams::default(), identity)
                .await
        };
        Box::pin(fut)
    }

    fn delete<'a>(
        &'a self,
        identity_name: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<(), KError>> + '_>> {
        let fut = async move {
            self.service_identity_api
                .delete(identity_name, &DeleteParams::default())
                .await
                .map(|_| ())
        };
        Box::pin(fut)
    }

    fn list<'a>(
        &'a self,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<ServiceIdentity>, KError>> + '_>> {
        let fut = async move {
            self.service_identity_api
                .list(&ListParams::default())
                .await
                .map(|xs| xs.items)
        };
        Box::pin(fut)
    }
}

trait IdentityManager:
    ServiceIdentityAPI
    + ServiceIdentityProvider<From = Deployment, To = ServiceIdentity>
    + ServiceCredentialsPool
{
}

impl IdentityManager for KubeIdentityManager {}

pub struct IdentityManagerRunner(Box<dyn IdentityManager>);

impl IdentityManagerRunner {
    pub fn kube_runner(client: Client) -> IdentityManagerRunner {
        let service_identity_api: Api<ServiceIdentity> = Api::namespaced(client, "sdp-system");
        IdentityManagerRunner(Box::new(KubeIdentityManager {
            pool: VecDeque::with_capacity(30),
            service_identity_api: service_identity_api,
            services: HashMap::new(),
        }))
    }

    async fn run_identity_manager(
        im: &mut Box<dyn IdentityManager>,
        mut identity_manager_rx: Receiver<IdentityManagerProtocol<Deployment, ServiceIdentity>>,
        identity_manager_tx: Sender<IdentityManagerProtocol<Deployment, ServiceIdentity>>,
        identity_creator_tx: Sender<IdentityCreatorProtocol>,
        deployment_watcher_proto_tx: Sender<DeploymentWatcherProtocol>,
    ) -> () {
        info!("Running Identity Manager ...");
        let mut deployment_watcher_ready = false;
        let mut identity_creator_ready = false;
        let mut existing_service_candidates: HashSet<String> = HashSet::new();
        while let Some(msg) = identity_manager_rx.recv().await {
            match msg {
                IdentityManagerProtocol::DeleteServiceIdentity { service_identity } => {
                    match im.delete(&service_identity.service_id()).await {
                        Ok(_) => {
                            info!(
                                "New ServiceIdentity deleted for service with id {}",
                                service_identity.service_id()
                            );
                            info!("Asking for deletion of IdentityCredential from SDP system");
                            if let Err(err) = identity_creator_tx
                                .send(IdentityCreatorProtocol::DeleteIdentity(
                                    service_identity.spec.service_credentials.id,
                                ))
                                .await
                            {
                                error!(
                                    "Error when sending event to delete IdentityCredential: {}",
                                    err
                                );
                            }
                        }
                        Err(err) => {
                            error!(
                                "Error deleting ServiceIdentity for service with id {}: {}",
                                service_identity.service_id(),
                                err
                            );
                        }
                    }
                }
                IdentityManagerProtocol::RequestServiceIdentity { service_candidate } => {
                    let service_id = service_candidate.service_id();
                    info!("New user requested for service {}", service_id);
                    match im.next_identity(service_candidate) {
                        Some(identity) => match im.create(&identity).await {
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
                        },
                        None => {
                            error!("Unable to assign service identity for service {}. Identities pool seems to be empty!", service_id);
                        }
                    };
                }
                IdentityManagerProtocol::FoundServiceIdentity { service_candidate } => {
                    if im.has_candidate(&service_candidate) {
                        info!(
                            "Found already registered ServiceCandidate in K8S cluster with id: {}",
                            service_candidate.service_id()
                        );
                    } else {
                        info!("Found not registered ServiceCandidate in K8S cluster with id: {}, registering it",
                         service_candidate.service_id());
                        identity_manager_tx
                            .send(IdentityManagerProtocol::RequestServiceIdentity {
                                service_candidate: service_candidate.clone(),
                            })
                            .await
                            .expect("Error requesting new ServiceIdentity");
                    }
                    existing_service_candidates.insert(service_candidate.service_id());
                }

                IdentityManagerProtocol::DeploymentWatcherReady if identity_creator_ready => {
                    info!("IdentityCreator and DeploymentWatcher are ready!");
                    info!("Validating ServiceCandidates agains ServiceCredentials");
                    let xs = im.extra_identities(&existing_service_candidates);
                    warn!("XS: {:?}", xs);
                    warn!("CS: {:?}", existing_service_candidates);
                    for identity in im.extra_identities(&existing_service_candidates) {
                        identity_manager_tx
                            .send(IdentityManagerProtocol::DeleteServiceIdentity {
                                service_identity: identity.clone(),
                            })
                            .await
                            .expect("Error requesting new ServiceIdentity");
                    }
                }
                IdentityManagerProtocol::DeploymentWatcherReady => {
                    panic!("DeploymentWatcher is ready but IdentityCreator is not. This should not happen!")
                }
                IdentityManagerProtocol::NewServiceCredentials {
                    user_credentials_ref,
                } => {
                    info!(
                        "Push new UserCredentialRef with id {}",
                        user_credentials_ref.id
                    );
                    // Got new fresh credentials, add them to the pool
                    im.push(user_credentials_ref);
                }
                IdentityManagerProtocol::IdentityCreatorReady if !deployment_watcher_ready => {
                    info!("IdentityCreator is ready!");
                    identity_creator_ready = true;
                    deployment_watcher_ready = true;
                    deployment_watcher_proto_tx
                        .send(DeploymentWatcherProtocol::IdentityManagerReady)
                        .await
                        .expect("Unable to notify DeploymentWatcher!");
                }
                IdentityManagerProtocol::IdentityCreatorReady => {
                    info!("IdentityCreator event ignored");
                    identity_creator_ready = true;
                }
                _ => {
                    warn!("Ignored message");
                }
            }
        }
    }

    /// Load all the current ServiceIdentity
    async fn initialize(im: &mut Box<dyn IdentityManager>) -> () {
        info!("Initializing Identity Manager ...");
        match im.list().await {
            Ok(xs) => {
                info!("Restoring previous service identities");
                let n: u32 = xs
                    .iter()
                    .map(|s| {
                        info!("Restoring service identity {}", s.service_id());
                        im.register_identity(s.clone());
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
        mut self,
        identity_manager_prot_rx: Receiver<IdentityManagerProtocol<Deployment, ServiceIdentity>>,
        identity_manager_prot_tx: Sender<IdentityManagerProtocol<Deployment, ServiceIdentity>>,
        identity_creater_proto_tx: Sender<IdentityCreatorProtocol>,
        deployment_watcher_proto_tx: Sender<DeploymentWatcherProtocol>,
    ) -> () {
        let im = &mut self.0;
        IdentityManagerRunner::initialize(im).await;
        IdentityManagerRunner::run_identity_manager(
            im,
            identity_manager_prot_rx,
            identity_manager_prot_tx,
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
