use futures::Future;
use k8s_openapi::api::apps::v1::Deployment;
use kube::api::{DeleteParams, ListParams, PostParams};
use kube::{Api, Client, Error as KError};
use log::{error, info, warn};
pub use sdp_common::crd::{ServiceIdentity, ServiceIdentitySpec};
use sdp_common::service::{HasCredentials, ServiceCandidate};
use sdp_macros::{queue_debug, sdp_error, sdp_info, sdp_log, sdp_warn};
use std::collections::{HashMap, HashSet, VecDeque};
use std::fmt;
use std::iter::FromIterator;
use std::pin::Pin;
use tokio::sync::mpsc::{Receiver, Sender};

use crate::deployment_watcher::DeploymentWatcherProtocol;
use crate::identity_creator::{IdentityCreatorProtocol, ServiceCredentialsRef};

/// Trait that represents the pool of ServiceCredential entities
/// We can pop and push ServiceCredential entities
trait ServiceCredentialsPool {
    fn pop(&mut self) -> Option<ServiceCredentialsRef>;
    fn push(&mut self, user_credentials_ref: ServiceCredentialsRef) -> ();
    fn needs_new_credentials(&self) -> bool;
}

/// Trait for ServiceIdentity provider
/// This traits provides instances of To from instances of From
trait ServiceIdentityProvider {
    type From: ServiceCandidate + Send;
    type To: ServiceCandidate + HasCredentials + Send;
    fn register_identity(&mut self, to: Self::To) -> ();
    fn unregister_identity(&mut self, to: &Self::To) -> Option<Self::To>;
    fn next_identity(&mut self, from: &Self::From) -> Option<Self::To>;
    fn identity(&self, from: &Self::From) -> Option<&Self::To>;
    fn identities(&self) -> Vec<&Self::To>;
    fn extra_identities<'a>(&self, services: &HashSet<String>) -> Vec<&Self::To> {
        self.identities()
            .iter()
            .filter(|id| !services.contains(&id.service_id()))
            .map(|id| *id)
            .collect()
    }
    fn extra_user_credentials<'a>(
        &self,
        activated_credentials: &'a HashSet<String>,
    ) -> Vec<&'a String> {
        let current_activated_credentials = HashSet::<String>::from_iter(
            self.identities().iter().map(|i| i.credentials().id.clone()),
        );
        // Compute activated credentials not registered in any Self::To
        activated_credentials
            .into_iter()
            .filter(|c| !current_activated_credentials.contains(c.clone()))
            .collect()
    }

    fn orphan_identities<'a>(&self, activated_credentials: &'a HashSet<String>) -> Vec<&Self::To> {
        self.identities()
            .iter()
            .filter(|i| !activated_credentials.contains(&i.credentials().id))
            .map(|i| *i)
            .collect()
    }
}

/// This should not be needed once the GAT support is in stable
/// https://blog.rust-lang.org/2021/08/03/GATs-stabilization-push.html
trait ServiceIdentityAPI {
    fn create<'a>(
        &'a self,
        identity: &'a ServiceIdentity,
    ) -> Pin<Box<dyn Future<Output = Result<ServiceIdentity, KError>> + Send + '_>>;
    fn delete<'a>(
        &'a self,
        identity_name: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<(), KError>> + Send + '_>>;
    fn list<'a>(
        &'a self,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<ServiceIdentity>, KError>> + Send + '_>>;
}

/// Messages exchanged between the the IdentityCreator and IdentityManager
#[derive(Debug)]
pub enum IdentityManagerProtocol<From: ServiceCandidate, To: ServiceCandidate + HasCredentials> {
    /// Message used to request a new ServiceIdentity for ServiceCandidate
    RequestServiceIdentity {
        service_candidate: From,
    },
    DeleteServiceIdentity {
        service_identity: To,
    },
    FoundServiceCandidate {
        service_candidate: From,
    },
    /// Message to notify that a new ServiceCredential have been created
    /// IdentityCreator creates these ServiceCredentials
    FoundUserCredentials {
        user_credentials_ref: ServiceCredentialsRef,
        activated: bool,
    },
    IdentityCreatorReady,
    DeploymentWatcherReady,
    IdentityManagerInitialized,
    IdentityManagerStarted,
    IdentityManagerReady,
    IdentityManagerDebug(String),
}

pub enum IdentityMessageResponse {
    /// Message used to send a new user
    NewIdentity(ServiceCredentialsRef),
    IdentityUnavailable,
}

#[derive(Default)]
struct IdentityManagerPool {
    pool: VecDeque<ServiceCredentialsRef>,
    services: HashMap<String, ServiceIdentity>,
}

impl ServiceCredentialsPool for IdentityManagerPool {
    fn pop(&mut self) -> Option<ServiceCredentialsRef> {
        self.pool.pop_front()
    }

    fn push(&mut self, user_credentials_ref: ServiceCredentialsRef) -> () {
        self.pool.push_back(user_credentials_ref)
    }

    fn needs_new_credentials(&self) -> bool {
        self.pool.len() < 10
    }
}

impl ServiceIdentityProvider for IdentityManagerPool {
    type From = Deployment;
    type To = ServiceIdentity;

    fn register_identity(&mut self, to: Self::To) -> () {
        self.services.insert(to.service_id(), to);
    }

    fn unregister_identity(&mut self, to: &Self::To) -> Option<Self::To> {
        self.services.remove(&to.service_id())
    }

    fn next_identity(&mut self, from: &Self::From) -> Option<Self::To> {
        let service_id = from.service_id();
        self.services.get(&service_id)
            .map(|i| i.clone())
            .or_else(|| {
                if let Some(id) = self.pop().map(|service_crendetials_ref| {
                    let service_identity_spec = ServiceIdentitySpec {
                        service_name: ServiceCandidate::name(from),
                        service_namespace: ServiceCandidate::namespace(from),
                        service_credentials: service_crendetials_ref,
                        labels: ServiceCandidate::labels(from),
                        disabled: false,
                    };
                    ServiceIdentity::new(&service_id, service_identity_spec)
                }) {
                    info!(
                        "ServiceCandidate {} has not ServiceIdentities registered, registering one for it",
                        service_id
                    );
                    self.register_identity(id.clone());
                    Some(id)
                } else {
                    error!("Unable to get a new identity for service candidate {}, is the identities pool empty?", service_id);
                    None
                }
        })
    }

    fn identity(&self, from: &Self::From) -> Option<&Self::To> {
        self.services.get(&from.service_id())
    }

    fn identities(&self) -> Vec<&Self::To> {
        self.services.values().collect()
    }
}

#[sdp_proc_macros::identity_provider()]
#[derive(sdp_proc_macros::IdentityProvider)]
#[IdentityProvider(From = "Deployment", To = "ServiceIdentity")]
pub struct KubeIdentityManager {
    service_identity_api: Api<ServiceIdentity>,
}

impl ServiceIdentityAPI for KubeIdentityManager {
    fn create<'a>(
        &'a self,
        identity: &'a ServiceIdentity,
    ) -> Pin<Box<dyn Future<Output = Result<ServiceIdentity, KError>> + Send + '_>> {
        let fut = async move {
            match self
                .service_identity_api
                .get_opt(&identity.service_id())
                .await
            {
                Ok(None) => {
                    info!(
                        "ServiceIdentity {} does not exist, creating it.",
                        identity.service_id()
                    );
                    self.service_identity_api
                        .create(&PostParams::default(), identity)
                        .await
                }
                Ok(_) => {
                    info!("ServiceIdentity {} already exists.", identity.service_id());
                    Ok(identity.clone())
                }
                Err(e) => {
                    error!(
                        "Error checking if service identity {} exists.",
                        identity.service_id()
                    );
                    Err(e)
                }
            }
        };
        Box::pin(fut)
    }

    fn delete<'a>(
        &'a self,
        identity_name: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<(), KError>> + Send + '_>> {
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
    ) -> Pin<Box<dyn Future<Output = Result<Vec<ServiceIdentity>, KError>> + Send + '_>> {
        let fut = async move {
            self.service_identity_api
                .list(&ListParams::default())
                .await
                .map(|xs| xs.items)
        };
        Box::pin(fut)
    }
}

trait IdentityManager<
    From: ServiceCandidate + Send + Sync,
    To: ServiceCandidate + HasCredentials + Send + Sync,
>: ServiceIdentityAPI + ServiceIdentityProvider<From = From, To = To> + ServiceCredentialsPool
{
}

pub struct IdentityManagerRunner<
    From: ServiceCandidate + Send,
    To: ServiceCandidate + HasCredentials + Send,
> {
    im: Box<dyn IdentityManager<From, To> + Send + Sync>,
}

/// Load all the current ServiceIdentity
/// Flow between services is:
/// - IM collects all ServiceIdentity defined
/// - IM asks IC to collect current ServiceCredentialsRef
/// - IC notifies IM with defined ServiceCredentialsRef (active and not active)
/// - DW notifies IM with the current defined ServiceCandidates
/// - IM cleans up extra ServiceCredentialsRef (credentials active in system without a ServiceIdentrity)
/// - IM cleans up ServiceIdentities that have ServiceCrendeitalsRef not active
/// - IM cleans up ServiceIdentities that don't have a ServiceCandidate attached
/// - IM asks DW to start
/// - DW sends the list of all CandidateServices (Deployments)
/// - IM creates ServiceIDentity for those CandidateServices that need it and dont have one.
impl IdentityManagerRunner<Deployment, ServiceIdentity> {
    pub fn kube_runner(client: Client) -> IdentityManagerRunner<Deployment, ServiceIdentity> {
        let service_identity_api: Api<ServiceIdentity> = Api::namespaced(client, "sdp-system");
        IdentityManagerRunner {
            im: Box::new(KubeIdentityManager {
                pool: IdentityManagerPool {
                    pool: VecDeque::with_capacity(30),
                    services: HashMap::new(),
                },
                service_identity_api: service_identity_api,
            }),
        }
    }

    async fn run_identity_manager<F: ServiceCandidate + Clone + fmt::Debug + Send>(
        im: &mut Box<dyn IdentityManager<F, ServiceIdentity> + Send + Sync>,
        mut identity_manager_rx: Receiver<IdentityManagerProtocol<F, ServiceIdentity>>,
        identity_manager_tx: Sender<IdentityManagerProtocol<F, ServiceIdentity>>,
        identity_creator_tx: Sender<IdentityCreatorProtocol>,
        deployment_watcher_proto_tx: Sender<DeploymentWatcherProtocol>,
        external_queue_tx: Option<&Sender<IdentityManagerProtocol<F, ServiceIdentity>>>,
    ) -> () {
        info!("Running Identity Manager main loop");
        let mut deployment_watcher_ready = false;
        let mut identity_creator_ready = false;
        let mut existing_service_candidates: HashSet<String> = HashSet::new();
        let mut existing_activated_credentials: HashSet<String> = HashSet::new();
        let mut existing_deactivated_credentials: HashSet<String> = HashSet::new();

        queue_debug! {
            IdentityManagerProtocol::<F, ServiceIdentity>::IdentityManagerStarted => external_queue_tx
        };

        while let Some(msg) = identity_manager_rx.recv().await {
            match msg {
                IdentityManagerProtocol::DeleteServiceIdentity { service_identity } => {
                    info!(
                        "Deleting ServiceIdentity with id {}",
                        service_identity.service_id()
                    );
                    match im.delete(&service_identity.service_id()).await {
                        Ok(_) => {
                            info!(
                                "Deregistering ServiceIdentity with id {}",
                                service_identity.service_id()
                            );

                            // Unregister the identity
                            if let Some(s) = im.unregister_identity(&service_identity) {
                                sdp_info!(IdentityManagerProtocol::<F, ServiceIdentity>::IdentityManagerDebug |
                                    ("ServiceIdentity with id {} unregistered", s.service_id()
                                ) => external_queue_tx);
                            } else {
                                sdp_warn!(IdentityManagerProtocol::<F, ServiceIdentity>::IdentityManagerDebug | (
                                    "ServiceIdentity with id {} was not registered", service_identity.service_id()
                                ) => external_queue_tx);
                            }

                            // Ask IdentityCreator to remove the IdentityCredential
                            info!(
                                "Asking for deletion of IdentityCredential {} from SDP system",
                                service_identity.credentials().id
                            );
                            if let Err(err) = identity_creator_tx
                                .send(IdentityCreatorProtocol::DeleteIdentity(
                                    service_identity.credentials().id.clone(),
                                ))
                                .await
                            {
                                sdp_error!(IdentityManagerProtocol::<F, ServiceIdentity>::IdentityManagerDebug |(
                                    "Error when sending event to delete IdentityCredential: {}", err
                                ) => external_queue_tx);
                            }
                        }
                        Err(err) => {
                            sdp_error!(IdentityManagerProtocol::<F, ServiceIdentity>::IdentityManagerDebug |(
                                "Error deleting ServiceIdentity for service {}: {}",
                                service_identity.service_id(),
                                err
                            ) => external_queue_tx);
                        }
                    }
                }
                IdentityManagerProtocol::RequestServiceIdentity { service_candidate } => {
                    let service_id = service_candidate.service_id();
                    sdp_info!(IdentityManagerProtocol::<F, ServiceIdentity>::IdentityManagerDebug |(
                        "New ServiceIdentity requested for ServiceCandidate {}",
                        service_id
                    ) => external_queue_tx);
                    match im.next_identity(&service_candidate) {
                        Some(identity) => match im.create(&identity).await {
                            Ok(service_identity) => {
                                sdp_info!(IdentityManagerProtocol::<F, ServiceIdentity>::IdentityManagerDebug |(
                                    "ServiceIdentity created for service {}",
                                    service_id
                                ) => external_queue_tx);

                                if im.needs_new_credentials() {
                                    info!("Requesting new UserCredentials to add to the pool");
                                    if let Err(err) = identity_creator_tx
                                        .send(IdentityCreatorProtocol::CreateIdentity)
                                        .await
                                    {
                                        sdp_error!(IdentityManagerProtocol::<F, ServiceIdentity>::IdentityManagerDebug |(
                                            "Error when sending IdentityCreatorMessage::CreateIdentity: {}",
                                            err
                                        ) => external_queue_tx);
                                    }
                                }
                                if let Err(err) = identity_creator_tx
                                    .send(IdentityCreatorProtocol::ModifyIdentity {
                                        service_credentials: service_identity.credentials().clone(),
                                        name: identity.service_id(),
                                        labels: identity.spec.labels,
                                        active: true,
                                    })
                                    .await
                                {
                                    error!("Error updating ServiceUser in UserCredentials with id {} for service {}: {}",
                                    service_identity.credentials().id, service_identity.service_id(), err);
                                }
                            }
                            Err(err) => {
                                sdp_error!(IdentityManagerProtocol::<F, ServiceIdentity>::IdentityManagerDebug |(
                                    "Error creating ServiceIdentity for service with id {}: {}",
                                    service_id, err
                            ) => external_queue_tx);
                            }
                        },
                        None => {
                            sdp_error!(IdentityManagerProtocol::<F, ServiceIdentity>::IdentityManagerDebug |(
                                "Unable to assign service identity for service {}. Identities pool seems to be empty!",
                                 service_id
                                ) => external_queue_tx);
                        }
                    };
                }
                IdentityManagerProtocol::FoundServiceCandidate { service_candidate } => {
                    if let Some(service_identity) = im.identity(&service_candidate) {
                        sdp_info!(IdentityManagerProtocol::<F, ServiceIdentity>::IdentityManagerDebug |(
                            "Found registered ServiceCandidate {}",
                            service_identity.service_id()
                        ) => external_queue_tx);
                    } else {
                        sdp_info!(IdentityManagerProtocol::<F, ServiceIdentity>::IdentityManagerDebug |(
                            "Found unregistered ServiceCandidate {}, registering it",
                            service_candidate.service_id()
                        ) => external_queue_tx);

                        identity_manager_tx
                            .send(IdentityManagerProtocol::RequestServiceIdentity {
                                service_candidate: service_candidate.clone(),
                            })
                            .await
                            .expect("Error requesting new ServiceIdentity");
                    }
                    existing_service_candidates.insert(service_candidate.service_id());
                }
                // Identity Creator notifies about fresh, unactivated User Credentials
                IdentityManagerProtocol::FoundUserCredentials {
                    user_credentials_ref,
                    activated,
                } if !activated => {
                    sdp_info!(IdentityManagerProtocol::<F, ServiceIdentity>::IdentityManagerDebug |(
                        "Found deactivated UserCredentialsRef with id {}",
                        user_credentials_ref.id
                    ) => external_queue_tx);
                    existing_deactivated_credentials.insert(user_credentials_ref.id.clone());
                    im.push(user_credentials_ref);
                }
                // Identity Creator notifies about already activated User Credentials
                IdentityManagerProtocol::FoundUserCredentials {
                    user_credentials_ref,
                    activated,
                } if activated => {
                    existing_activated_credentials.insert(user_credentials_ref.id.clone());
                    sdp_info!(IdentityManagerProtocol::<F, ServiceIdentity>::IdentityManagerDebug |(
                        "Found activated UserCredentialsRef with id {}",
                        user_credentials_ref.id
                    ) => external_queue_tx);
                }
                IdentityManagerProtocol::IdentityCreatorReady => {
                    info!("IdentityCreator is ready");
                    identity_creator_ready = true;
                    if deployment_watcher_ready {
                        info!("IdentityManager is ready");
                        if let Err(e) = identity_manager_tx
                            .send(IdentityManagerProtocol::IdentityManagerReady)
                            .await
                        {
                            let err_str = format!(
                                "Unable to create deployment watcher: {}. Exiting.",
                                e.to_string()
                            );
                            error!("{}", err_str);
                            panic!("{}", err_str);
                        }
                    }
                }
                IdentityManagerProtocol::DeploymentWatcherReady => {
                    info!("DeploymentWatcher is ready");
                    deployment_watcher_ready = true;
                    if identity_creator_ready {
                        if let Err(e) = identity_manager_tx
                            .send(IdentityManagerProtocol::IdentityManagerReady)
                            .await
                        {
                            let err_str = format!(
                                "Unable to create deployment watcher: {}. Exiting.",
                                e.to_string()
                            );
                            error!("{}", err_str);
                            panic!("{}", err_str);
                        }
                    }
                }

                // Identity Creator finished the initialization
                IdentityManagerProtocol::IdentityManagerReady => {
                    info!("IdentityMAnager is ready");

                    sdp_info!(IdentityManagerProtocol::<F, ServiceIdentity>::IdentityManagerDebug |("Syncing UserCredentials") => external_queue_tx);
                    // Delete active credentials not in use by any service
                    for user_credentials_id in
                        im.extra_user_credentials(&existing_activated_credentials)
                    {
                        info!("UserCredentials {} are active but not used by any IdentityService, deleteing it", &user_credentials_id);
                        identity_creator_tx
                            .send(IdentityCreatorProtocol::DeleteIdentity(
                                user_credentials_id.clone(),
                            ))
                            .await
                            .expect("Unable to delete obsolete UserCredentials");
                    }

                    sdp_info!(IdentityManagerProtocol::<F, ServiceIdentity>::IdentityManagerDebug |("Syncing IdentityServices") => external_queue_tx);
                    // Delete Identity Services holding not active credentials
                    let mut removed_service_identities: HashSet<String> = HashSet::new();
                    for identity_service in im.orphan_identities(&existing_activated_credentials) {
                        info!(
                            "ServiceIdentity {} has deactivated UserCredentialsRed. Deleting it.",
                            identity_service.service_id()
                        );
                        identity_manager_tx
                            .send(IdentityManagerProtocol::DeleteServiceIdentity {
                                service_identity: identity_service.clone(),
                            })
                            .await
                            .expect("Error requesting deletiong of ServiceIdentity");
                        // Make sure we dont try to delete it twice!
                        removed_service_identities.insert(identity_service.service_id().clone());
                    }

                    // Delete Identity Services with no ServiceCandidate
                    for identity in im.extra_identities(&existing_service_candidates) {
                        info!(
                            "ServiceIdentity {} has not attached ServiceCandidate. Deleting it.",
                            identity.service_id()
                        );
                        if !removed_service_identities.contains(&identity.service_id().clone()) {
                            identity_manager_tx
                                .send(IdentityManagerProtocol::DeleteServiceIdentity {
                                    service_identity: identity.clone(),
                                })
                                .await
                                .expect("Error requesting deletiong of ServiceIdentity");
                        }
                    }

                    deployment_watcher_proto_tx
                        .send(DeploymentWatcherProtocol::IdentityManagerReady)
                        .await
                        .expect("Unable to notify DeploymentWatcher!");
                }
                _ => {
                    warn!("Ignored message");
                }
            }
        }
    }

    async fn initialize<F: ServiceCandidate + Send>(
        im: &mut Box<dyn IdentityManager<F, ServiceIdentity> + Send + Sync>,
    ) -> () {
        info!("Initializing Identity Manager service");
        match im.list().await {
            Ok(xs) => {
                info!("Restoring previous Service Identity instances");
                let n: u32 = xs
                    .iter()
                    .map(|s| {
                        info!("Restoring Service Ideantity {}", s.service_id());
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
        external_queue_tx: Option<Sender<IdentityManagerProtocol<Deployment, ServiceIdentity>>>,
    ) -> () {
        info!("Starting Identity Manager service");
        IdentityManagerRunner::initialize(&mut self.im).await;

        queue_debug! {
            IdentityManagerProtocol::<Deployment, ServiceIdentity>::IdentityManagerInitialized => external_queue_tx
        };

        // Ask Identity Creator to awake
        if let Err(err) = identity_creater_proto_tx
            .send(IdentityCreatorProtocol::StartService)
            .await
        {
            error!("Error awakening Identity Creator: {}", err);
            panic!();
        }
        // We are not ready to process events
        IdentityManagerRunner::run_identity_manager(
            &mut self.im,
            identity_manager_prot_rx,
            identity_manager_prot_tx,
            identity_creater_proto_tx,
            deployment_watcher_proto_tx,
            external_queue_tx.as_ref(),
        )
        .await;
    }
}

#[cfg(test)]
mod tests {
    use std::{
        collections::{HashMap, HashSet},
        future,
        pin::Pin,
        sync::{Arc, Mutex},
    };

    use futures::Future;
    use k8s_openapi::api::apps::v1::Deployment;
    use kube::{core::object::HasSpec, error::Error as KError};
    use sdp_macros::{credentials_ref, deployment, service_identity};
    use sdp_test_macros::{assert_message, assert_no_message};
    use tokio::sync::mpsc::channel;
    use tokio::time::{sleep, timeout, Duration};

    use crate::{
        deployment_watcher::DeploymentWatcherProtocol,
        identity_creator::{IdentityCreatorProtocol, ServiceCredentialsRef},
        identity_manager::IdentityManagerProtocol,
    };

    use super::{
        IdentityManager, IdentityManagerPool, IdentityManagerRunner, ServiceCandidate,
        ServiceCredentialsPool, ServiceIdentity, ServiceIdentityAPI, ServiceIdentityProvider,
        ServiceIdentitySpec,
    };

    macro_rules! test_identity_manager {
        (($im:ident($vs:expr), $watcher_rx:ident, $identity_manager_proto_tx:ident, $identity_creator_proto_rx:ident, $deployment_watched_proto_rx:ident, $counters:ident) => $e:expr) => {
            let ($identity_manager_proto_tx, identity_manager_proto_rx) =
                channel::<IdentityManagerProtocol<Deployment, ServiceIdentity>>(10);
            let (identity_creator_proto_tx, mut $identity_creator_proto_rx) =
                channel::<IdentityCreatorProtocol>(10);
            let (deployment_watched_proto_tx, mut $deployment_watched_proto_rx) =
                channel::<DeploymentWatcherProtocol>(10);
            let (watcher_tx, mut $watcher_rx) =
                channel::<IdentityManagerProtocol<Deployment, ServiceIdentity>>(10);
            let mut $im = Box::new(TestIdentityManager::default());
            for i in $vs.clone() {
                $im.register_identity(i);
            }
            let identity_manager_proto_tx_cp2 = $identity_manager_proto_tx.clone();
            let $counters = $im.api_counters.clone();
            tokio::spawn(async move {
                let im_runner = new_test_identity_runner($im);
                im_runner
                    .run(
                        identity_manager_proto_rx,
                        identity_manager_proto_tx_cp2,
                        identity_creator_proto_tx,
                        deployment_watched_proto_tx,
                        Some(watcher_tx),
                    )
                    .await
            });
            $e
        };

        (($watcher_rx:ident, $identity_manager_proto_tx:ident, $identity_creator_proto_rx:ident, $deployment_watched_proto_rx:ident, $counters:ident) => $e:expr) => {
            let vs = vec![
                service_identity!(1),
                service_identity!(2),
                service_identity!(3),
                service_identity!(4),
            ];
            test_identity_manager! {
                (im(vs), $watcher_rx, $identity_manager_proto_tx, $identity_creator_proto_rx, $deployment_watched_proto_rx, $counters) => {
                    $e
               }
            }
        }
    }

    macro_rules! test_service_identity_provider {
        ($im:ident($vs:expr) => $e:expr) => {
            let mut $im = Box::new(TestIdentityManager::default());
            assert_eq!($im.identities().len(), 0);
            // register new identities
            for i in $vs.clone() {
                $im.register_identity(i);
            }
            $e
        };
        ($im:ident => $e:expr) => {
            let vs = vec![
                service_identity!(1),
                service_identity!(2),
                service_identity!(3),
                service_identity!(4),
            ];
            test_service_identity_provider! {
                $im(vs) => $e
            }
        };
    }

    #[derive(Default)]
    struct APICounters {
        delete_calls: usize,
        create_calls: usize,
        list_calls: usize,
    }

    #[sdp_proc_macros::identity_provider()]
    #[derive(sdp_proc_macros::IdentityProvider, Default)]
    #[IdentityProvider(From = "Deployment", To = "ServiceIdentity")]
    struct TestIdentityManager {
        api_counters: Arc<Mutex<APICounters>>,
    }

    impl TestIdentityManager {
        fn _reset_counters(&self) -> () {
            let mut api_counters = self.api_counters.lock().unwrap();
            api_counters.delete_calls = 0;
            api_counters.create_calls = 0;
            api_counters.list_calls = 0;
        }
    }

    impl ServiceIdentityAPI for TestIdentityManager {
        fn create<'a>(
            &'a self,
            identity: &'a ServiceIdentity,
        ) -> Pin<Box<dyn Future<Output = Result<ServiceIdentity, KError>> + Send + '_>> {
            self.api_counters.lock().unwrap().create_calls += 1;
            Box::pin(future::ready(Ok(identity.clone())))
        }

        fn delete<'a>(
            &'a self,
            _: &'a str,
        ) -> Pin<Box<dyn Future<Output = Result<(), KError>> + Send + '_>> {
            self.api_counters.lock().unwrap().delete_calls += 1;
            Box::pin(future::ready(Ok(())))
        }

        fn list<'a>(
            &'a self,
        ) -> Pin<Box<dyn Future<Output = Result<Vec<ServiceIdentity>, KError>> + Send + '_>>
        {
            self.api_counters.lock().unwrap().list_calls += 1;
            Box::pin(future::ready(Ok(vec![])))
        }
    }

    fn new_test_identity_runner(
        im: Box<TestIdentityManager>,
    ) -> IdentityManagerRunner<Deployment, ServiceIdentity> {
        IdentityManagerRunner {
            im: im as Box<dyn IdentityManager<Deployment, ServiceIdentity> + Send + Sync>,
        }
    }

    #[test]
    #[should_panic]
    fn test_service_identity_provider_candidates_panic() {
        test_service_identity_provider! {
            im => {
                im.identity(&Deployment::default());
            }
        }
    }

    #[test]
    fn test_service_identity_provider_identities() {
        let identities = vec![
            service_identity!(1),
            service_identity!(2),
            service_identity!(3),
            service_identity!(4),
        ];
        let d1 = deployment!("dep1", "ns1");
        let d2 = deployment!("srv1", "ns1");
        test_service_identity_provider! {
            im(identities) => {
                assert_eq!(im.identities().len(), 4);
                assert!(im.identity(&d1).is_none());
                assert!(im.identity(&d2).is_some());
                let mut is0: Vec<ServiceIdentity> = im.identities().iter().map(|&i| i.clone()).collect();
                let sorted_is0 = is0.sort_by(|a, b| a.spec().service_name.partial_cmp(&b.spec().service_name).unwrap());
                let mut is1: Vec<ServiceIdentity> = identities.iter().map(|i| i.clone()).collect();
                let sorted_is1 = is1.sort_by(|a, b| a.spec().service_name.partial_cmp(&b.spec().service_name).unwrap());
                assert_eq!(sorted_is0, sorted_is1);
            }
        }
    }

    fn check_service_identity(
        si: Option<ServiceIdentity>,
        c: &ServiceCredentialsRef,
        service_name: &str,
        service_ns: &str,
    ) -> () {
        assert!(si.is_some());
        let si_spec = si.unwrap();
        assert_eq!(si_spec.spec().service_name, service_name);
        assert_eq!(si_spec.spec().service_namespace, service_ns);
        assert_eq!(si_spec.spec().service_credentials.id, c.id);
    }

    #[test]
    fn test_service_identity_provider_next_identity() {
        let id1 = service_identity!(1);
        let identities = vec![id1.clone()];
        let d1_1 = deployment!("srv1", "ns1");
        let d2_2 = deployment!("srv2", "ns2");
        let d1_2 = deployment!("srv1", "ns2");
        let d3_1 = deployment!("srv3", "ns1");

        test_service_identity_provider! {
            im(identities) => {
                // service not registered but we don't have any credentials so we can not create
                // new identities for it.
                id1.service_id();
                assert_eq!(im.identities().len(), 1);
                assert!(im.next_identity(&d1_2).is_none());
                assert!(im.next_identity(&d2_2).is_none());

                // service already registered, we just return the credentials we have for it
                let d1_id = im.next_identity(&d1_1);
                check_service_identity(d1_id, &id1.spec().service_credentials, "srv1", "ns1");
            }
        }

        test_service_identity_provider! {
            im(identities) => {
                let c1 = credentials_ref!(1);
                let c2 = credentials_ref!(2);
                // push some credentials
                im.push(c1.clone());
                im.push(c2.clone());

                assert_eq!(im.identities().len(), 1);

                // ask for a new service identity, we can create it because we have creds
                let d2_2_id = im.next_identity(&d2_2);
                check_service_identity(d2_2_id, &c1, "srv2", "ns2");

                // service identity already registered
                let d1_1_id = im.next_identity(&d1_1);
                assert_eq!(d1_1_id.unwrap().spec(), id1.spec());

                // ask for a new service identity, we can create it because we have creds
                let d1_2_id = im.next_identity(&d1_2);
                check_service_identity(d1_2_id, &c2, "srv1", "ns2");

                // service not registered but we don't have any credentials so we can not create
                // new identities for it.
                assert!(im.next_identity(&d3_1).is_none());

                // We can still get the service identities we have registered
                let d2_2_id = im.next_identity(&d2_2);
                check_service_identity(d2_2_id, &c1, "srv2", "ns2");
                let d1_2_id = im.next_identity(&d1_2);
                check_service_identity(d1_2_id, &c2, "srv1", "ns2");

                // We have created 2 ServiceIdentity so they are now registered
                assert_eq!(im.identities().len(), 3);
                let mut identities = im.identities();
                identities.sort_by(|a, b| a.service_id().partial_cmp(&b.service_id()).unwrap());
                let identities: Vec<String> = identities.iter().map(|i| i.service_id()).collect();
                assert_eq!(identities, vec!["ns1-srv1", "ns2-srv1", "ns2-srv2"]);
            }
        }
    }

    fn extra_identities_tuple<'a>(
        im: &'a TestIdentityManager,
        services: &HashSet<String>,
    ) -> Vec<(&'a str, &'a str, &'a str)> {
        let mut xs: Vec<(&str, &str, &str)> = im
            .extra_identities(services)
            .iter()
            .map(|si| {
                let spec = si.spec();
                (
                    spec.service_namespace.as_str(),
                    spec.service_name.as_str(),
                    spec.service_credentials.id.as_str(),
                )
            })
            .collect();
        xs.sort();
        xs
    }

    #[test]
    fn test_service_identity_provider_extras_identities() {
        test_service_identity_provider! {
            im => {
                let mut identities = im.identities();
                identities.sort_by(|a, b| a.name().as_str().partial_cmp(b.name().as_str()).unwrap());

                // 4 services registered but none found on start,
                let xs = extra_identities_tuple(&im, &HashSet::new());
                assert_eq!(xs.len(), 4);
                assert_eq!(xs, vec![
                    ("ns1", "srv1", "id1"),
                    ("ns2", "srv2", "id2"),
                    ("ns3", "srv3", "id3"),
                    ("ns4", "srv4", "id4"),
                ]);

                // 4 services registered and only 1 on start
                let xs = extra_identities_tuple(&im, &HashSet::from(
                    [identities[0].service_id()]
                ));
                assert_eq!(xs.len(), 3);
                assert_eq!(xs, vec![
                    ("ns2", "srv2", "id2"),
                    ("ns3", "srv3", "id3"),
                    ("ns4", "srv4", "id4"),
                ]);

                // 4 services registered and all 4 on start
                let xs: Vec<(&str, &str, &str)> = extra_identities_tuple(&im, &HashSet::from(
                    [
                        identities[0].service_id(),
                        identities[1].service_id(),
                        identities[2].service_id(),
                        identities[3].service_id(),
                    ]
                ));
                assert_eq!(xs.len(), 0);
                assert_eq!(xs, vec![]);
            }
        }
    }

    #[tokio::test]
    async fn test_identity_manager_initialization() {
        test_identity_manager! {
            (watcher_rx, _identity_manager_tx, _identity_creator_rx, _deployment_watched_rx, counters) => {
                assert_message! {
                    (m :: IdentityManagerProtocol::IdentityManagerInitialized in watcher_rx) => {
                        assert_eq!(counters.lock().unwrap().list_calls, 1);
                        assert_message!(m :: IdentityManagerProtocol::IdentityManagerStarted in watcher_rx);
                    }
                }
            }
        }
    }

    #[tokio::test]
    async fn test_identity_manager_delete_service_identity_0() {
        test_identity_manager! {
            (im(vec![]), watcher_rx, identity_manager_tx, identity_creator_rx, _deployment_watched_rx, counters) => {
                // Wait for IM to be initialized
                assert_message!(m :: IdentityManagerProtocol::IdentityManagerInitialized in watcher_rx);
                // Wait for the service to run
                assert_message!(m :: IdentityManagerProtocol::IdentityManagerStarted in watcher_rx);

                // API.list was called
                assert_eq!(counters.lock().unwrap().list_calls, 1);

                // First message for IC should be StartService from IM
                assert_message!(m :: IdentityCreatorProtocol::StartService in identity_creator_rx);

                // Ask to delete a ServiceIdentity and give it time to process it
                let tx = identity_manager_tx.clone();
                tx.send(IdentityManagerProtocol::DeleteServiceIdentity {
                    service_identity: service_identity!(1)
                }).await.expect("Unable to send DeleteServiceIdentity message to IdentityManager");
                sleep(Duration::from_millis(10)).await;

                // We tried to delete it from the API
                assert_eq!(counters.lock().unwrap().delete_calls, 1);

                // We asked the IdentityCreator to delete it
                assert_message! {
                    (m :: IdentityCreatorProtocol::DeleteIdentity(_) in identity_creator_rx) => {
                        if let IdentityCreatorProtocol::DeleteIdentity(id) = m {
                            assert!(id == "id1".to_string(), "Wrong id for ServiceIdentity: {:?}", id);
                        }
                    }
                };

                // We could not deregister the ServiceIdentity since we dont have any one registered
                assert_message! {
                    (m :: IdentityManagerProtocol::IdentityManagerDebug(_) in watcher_rx) => {
                     if let IdentityManagerProtocol::IdentityManagerDebug(msg) = m {
                            assert!(msg.eq("ServiceIdentity with id ns1-srv1 was not registered"),
                                    "Wrong message, got {}", msg);
                        }
                    }
                }
            }
        };
    }

    #[tokio::test]
    async fn test_identity_manager_delete_service_identity_1() {
        let s1 = service_identity!(1);
        test_identity_manager! {
            (im(vec![s1]), watcher_rx, identity_manager_tx, identity_creator_rx, _deployment_watched_rx, counters) => {
                // Wait for IM to be initialized
                assert_message!(m :: IdentityManagerProtocol::IdentityManagerInitialized in watcher_rx);
                // Wait for the service to run
                assert_message!(m :: IdentityManagerProtocol::IdentityManagerStarted in watcher_rx);

                // API.list was called
                assert_eq!(counters.lock().unwrap().list_calls, 1);

                // First message for IC should be StartService from IM
                assert_message!(m :: IdentityCreatorProtocol::StartService in identity_creator_rx);

                // Ask to delete a ServiceIdentity and give it time to process it
                let tx = identity_manager_tx.clone();
                tx.send(IdentityManagerProtocol::DeleteServiceIdentity {
                    service_identity: service_identity!(1)
                }).await.expect("Unable to send DeleteServiceIdentity message to IdentityManager");
                sleep(Duration::from_millis(10)).await;

                // We tried to delete it from the API
                assert_eq!(counters.lock().unwrap().delete_calls, 1);

                // We asked the IdentityCreator to delete it
                assert_message! {
                    (m :: IdentityCreatorProtocol::DeleteIdentity(_) in identity_creator_rx) => {
                        if let IdentityCreatorProtocol::DeleteIdentity(id) = m {
                            assert!(id == "id1".to_string(), "Wrong id for ServiceIdentity: {:?}", id);
                        }
                    }
                };

                // We could not deregister the ServiceIdentity since we dont have any one registered
                assert_message! {
                    (m :: IdentityManagerProtocol::IdentityManagerDebug(_) in watcher_rx) => {
                     if let IdentityManagerProtocol::IdentityManagerDebug(msg) = m {
                            assert!(msg.eq("ServiceIdentity with id ns1-srv1 unregistered"),
                                    "Wrong message, got {}", msg);
                        }
                    }
                }
            }
        };
    }

    #[tokio::test]
    async fn test_identity_manager_request_service_identity_0() {
        test_identity_manager! {
            (watcher_rx, identity_manager_tx, identity_creator_rx, _deployment_watched_rx, _counters) => {
                assert_message!(m :: IdentityManagerProtocol::IdentityManagerInitialized in watcher_rx);
                assert_message!(m :: IdentityManagerProtocol::IdentityManagerStarted in watcher_rx);
                // Request a new ServiceIdentity
                let tx = identity_manager_tx.clone();
                tx.send(IdentityManagerProtocol::RequestServiceIdentity {
                    service_candidate: deployment!("ns1", "srv1"),
                }).await.expect("Unable to send RequestServiceIdentity message to IdentityManager");
                assert_message!(m :: IdentityCreatorProtocol::StartService in identity_creator_rx);
                assert_no_message!(identity_creator_rx);

                // Since we have an empty pool we can not create identities
                assert_message! {
                    (m :: IdentityManagerProtocol::IdentityManagerDebug(_) in watcher_rx) => {
                     if let IdentityManagerProtocol::IdentityManagerDebug(msg) = m {
                            assert!(msg.eq("New ServiceIdentity requested for ServiceCandidate srv1-ns1"),
                                    "Wrong message, got {}", msg);
                        }
                    }
                }
                assert_message! {
                    (m :: IdentityManagerProtocol::IdentityManagerDebug(_) in watcher_rx) => {
                     if let IdentityManagerProtocol::IdentityManagerDebug(msg) = m {
                            assert!(msg.eq("Unable to assign service identity for service srv1-ns1. Identities pool seems to be empty!"),
                                    "Wrong message, got {}", msg);
                        }
                    }
                }
            }
        }
    }

    #[tokio::test]
    async fn test_identity_manager_request_service_identity_1() {
        test_identity_manager! {
            (watcher_rx, identity_manager_tx, identity_creator_rx, _deployment_watched_rx, counters) => {
                assert_message!(m :: IdentityManagerProtocol::IdentityManagerInitialized in watcher_rx);
                assert_message!(m :: IdentityManagerProtocol::IdentityManagerStarted in watcher_rx);
                assert_message!(m :: IdentityCreatorProtocol::StartService in identity_creator_rx);
                // Request a new ServiceIdentity and give it time to process it
                let tx = identity_manager_tx.clone();

                // Push 2 fresh UserCredentialsRef
                tx.send(IdentityManagerProtocol::FoundUserCredentials{
                    user_credentials_ref: credentials_ref!(1),
                    activated: false
                }).await.expect("Unable to send message!");
                tx.send(IdentityManagerProtocol::FoundUserCredentials{
                    user_credentials_ref: credentials_ref!(2),
                    activated: false
                }).await.expect("Unable to send message!");

                // Check they were registered
                assert_message! {
                    (m :: IdentityManagerProtocol::IdentityManagerDebug(_) in watcher_rx) => {
                     if let IdentityManagerProtocol::IdentityManagerDebug(msg) = m {
                            assert!(msg.eq("Found deactivated UserCredentialsRef with id id1"),
                                    "Wrong message, got {}", msg);
                        }
                    }
                }
                assert_message! {
                    (m :: IdentityManagerProtocol::IdentityManagerDebug(_) in watcher_rx) => {
                     if let IdentityManagerProtocol::IdentityManagerDebug(msg) = m {
                            assert!(msg.eq("Found deactivated UserCredentialsRef with id id2"),
                                    "Wrong message, got {}", msg);
                        }
                    }
                }

                // Request a new ServiceIdentity
                tx.send(IdentityManagerProtocol::RequestServiceIdentity {
                    service_candidate: deployment!("ns1", "srv1"),
                }).await.expect("Unable to send RequestServiceIdentity message to IdentityManager");

                // We have deactivated credentials so we can create it
                assert_message! {
                    (m :: IdentityManagerProtocol::IdentityManagerDebug(_) in watcher_rx) => {
                     if let IdentityManagerProtocol::IdentityManagerDebug(msg) = m {
                            assert!(msg.eq("New ServiceIdentity requested for ServiceCandidate srv1-ns1"),
                                    "Wrong message, got {}", msg);
                        }
                    }
                }
                assert_message! {
                    (m :: IdentityManagerProtocol::IdentityManagerDebug(_) in watcher_rx) => {
                     if let IdentityManagerProtocol::IdentityManagerDebug(msg) = m {
                            assert!(msg.eq("ServiceIdentity created for service srv1-ns1"),
                                    "Wrong message, got {}", msg);
                        }
                    }
                }
                // Create it in k8s
                assert_eq!(counters.lock().unwrap().create_calls, 1);

                // We ask IC to create a new crendential
                assert_message!(m :: IdentityCreatorProtocol::CreateIdentity in identity_creator_rx);
                // We add labels and we activate new credential
                assert_message! {
                    (m :: IdentityCreatorProtocol::ModifyIdentity {..} in identity_creator_rx) => {
                        if let IdentityCreatorProtocol::ModifyIdentity {
                            service_credentials,
                            name,
                            labels,
                            active,
                        } = m {
                            assert!(service_credentials.id == "id1");
                            assert!(name == "srv1-ns1");
                            assert!(active);
                            assert!(labels == HashMap::from([
                                ("namespace".to_string(), "srv1".to_string()),
                                ("name".to_string(), "ns1".to_string())
                            ]));
                        }
                    }
                }

                // Request a new ServiceIdentity, second one
                tx.send(IdentityManagerProtocol::RequestServiceIdentity {
                    service_candidate: deployment!("ns2", "srv2"),
                }).await.expect("Unable to send RequestServiceIdentity message to IdentityManager");

                // We have deactivated credentials so we can create it
                assert_message! {
                    (m :: IdentityManagerProtocol::IdentityManagerDebug(_) in watcher_rx) => {
                     if let IdentityManagerProtocol::IdentityManagerDebug(msg) = m {
                            assert!(msg.eq("New ServiceIdentity requested for ServiceCandidate srv2-ns2"),
                                    "Wrong message, got {}", msg);
                        }
                    }
                }
                assert_message! {
                    (m :: IdentityManagerProtocol::IdentityManagerDebug(_) in watcher_rx) => {
                     if let IdentityManagerProtocol::IdentityManagerDebug(msg) = m {
                            assert!(msg.eq("ServiceIdentity created for service srv2-ns2"),
                                    "Wrong message, got {}", msg);
                        }
                    }
                }
                // Create it in k8s
                assert_eq!(counters.lock().unwrap().create_calls, 2);

                // We ask IC to create a new crendential
                assert_message!(m :: IdentityCreatorProtocol::CreateIdentity in identity_creator_rx);

                // We add labels and we activate new credential
                assert_message! {
                    (m :: IdentityCreatorProtocol::ModifyIdentity {..} in identity_creator_rx) => {
                        if let IdentityCreatorProtocol::ModifyIdentity {
                            service_credentials,
                            name,
                            labels,
                            active,
                        } = m {
                            assert!(service_credentials.id == "id2");
                            assert!(name == "srv2-ns2");
                            assert!(active);
                            assert!(labels == HashMap::from([
                                ("namespace".to_string(), "srv2".to_string()),
                                ("name".to_string(), "ns2".to_string())
                            ]));
                        }
                    }
                }
                assert_no_message!(identity_creator_rx);

                // Request a new ServiceIdentity, no more credentials!
                tx.send(IdentityManagerProtocol::RequestServiceIdentity {
                    service_candidate: deployment!("ns3", "srv1"),
                }).await.expect("Unable to send RequestServiceIdentity message to IdentityManager");
                assert_message! {
                    (m :: IdentityManagerProtocol::IdentityManagerDebug(_) in watcher_rx) => {
                     if let IdentityManagerProtocol::IdentityManagerDebug(msg) = m {
                            assert!(msg.eq("New ServiceIdentity requested for ServiceCandidate srv1-ns3"),
                                    "Wrong message, got {}", msg);
                        }
                    }
                }
                assert_message! {
                    (m :: IdentityManagerProtocol::IdentityManagerDebug(_) in watcher_rx) => {
                     if let IdentityManagerProtocol::IdentityManagerDebug(msg) = m {
                            assert!(msg.eq("Unable to assign service identity for service srv1-ns3. Identities pool seems to be empty!"),
                                    "Wrong message, got {}", msg);
                        }
                    }
                }
                assert_no_message!(identity_creator_rx);

                // Request a new ServiceIdentity already created
                tx.send(IdentityManagerProtocol::RequestServiceIdentity {
                    service_candidate: deployment!("ns1", "srv1"),
                }).await.expect("Unable to send RequestServiceIdentity message to IdentityManager");
                assert_message! {
                    (m :: IdentityManagerProtocol::IdentityManagerDebug(_) in watcher_rx) => {
                     if let IdentityManagerProtocol::IdentityManagerDebug(msg) = m {
                            assert!(msg.eq("New ServiceIdentity requested for ServiceCandidate srv1-ns1"),
                                    "Wrong message, got {}", msg);
                        }
                    }
                }
                assert_message! {
                    (m :: IdentityManagerProtocol::IdentityManagerDebug(_) in watcher_rx) => {
                     if let IdentityManagerProtocol::IdentityManagerDebug(msg) = m {
                            assert!(msg.eq("ServiceIdentity created for service srv1-ns1"),
                                    "Wrong message, got {}", msg);
                        }
                    }
                }
                // TODO: Here we should not call this create API.
                // Create it in k8s
                assert_eq!(counters.lock().unwrap().create_calls, 3);

                // We ask IC to create a new crendential
                assert_message!(m :: IdentityCreatorProtocol::CreateIdentity in identity_creator_rx);
                // We add labels and we activate new credential
                assert_message! {
                    (m :: IdentityCreatorProtocol::ModifyIdentity {..} in identity_creator_rx) => {
                        if let IdentityCreatorProtocol::ModifyIdentity {
                            service_credentials,
                            name,
                            labels,
                            active,
                        } = m {
                            assert!(service_credentials.id == "id1");
                            assert!(name == "srv1-ns1");
                            assert!(active);
                            assert!(labels == HashMap::from([
                                ("namespace".to_string(),
                                 "srv1".to_string()),
                                ("name".to_string(), "ns1".to_string())
                            ]));
                        }
                    }
                }

                assert_no_message!(identity_creator_rx);
            }
        }
    }

    #[tokio::test]
    async fn test_identity_manager_request_service_identity_2() {
        test_identity_manager! {
            (watcher_rx, identity_manager_tx, identity_creator_rx, _deployment_watched_rx, counters) => {
                assert_message!(m :: IdentityManagerProtocol::IdentityManagerInitialized in watcher_rx);
                assert_message!(m :: IdentityManagerProtocol::IdentityManagerStarted in watcher_rx);
                assert_message!(m :: IdentityCreatorProtocol::StartService in identity_creator_rx);
                // Request a new ServiceIdentity and give it time to process it
                let tx = identity_manager_tx.clone();

                // Push 11 fresh UserCredentialsRef
                for i in 1..12 {
                    tx.send(IdentityManagerProtocol::FoundUserCredentials{
                        user_credentials_ref: credentials_ref!(i),
                        activated: false
                    }).await.expect("Unable to send message!");

                    assert_message! {
                        (m :: IdentityManagerProtocol::IdentityManagerDebug(_) in watcher_rx) => {
                        let expected_msg = format!("Found deactivated UserCredentialsRef with id id{}", i);
                         if let IdentityManagerProtocol::IdentityManagerDebug(msg) = m {
                                assert!(msg.eq(expected_msg.as_str()),
                                        "Wrong message, expected {} but got {}", expected_msg.as_str(), msg);
                            }
                        }
                    }
                }
                // Request a new ServiceIdentity
                tx.send(IdentityManagerProtocol::RequestServiceIdentity {
                service_candidate: deployment!("ns1", "srv1"),
                }).await.expect("Unable to send RequestServiceIdentity message to IdentityManager");

                // We have deactivated credentials so we can create it
                assert_message! {
                    (m :: IdentityManagerProtocol::IdentityManagerDebug(_) in watcher_rx) => {
                        if let IdentityManagerProtocol::IdentityManagerDebug(msg) = m {
                            assert!(msg.eq("New ServiceIdentity requested for ServiceCandidate srv1-ns1"),
                                    "Wrong message, got {}", msg);
                        }
                    }
                }
                assert_message! {
                    (m :: IdentityManagerProtocol::IdentityManagerDebug(_) in watcher_rx) => {
                        if let IdentityManagerProtocol::IdentityManagerDebug(msg) = m {
                            assert!(msg.eq("ServiceIdentity created for service srv1-ns1"),
                                    "Wrong message, got {}", msg);
                        }
                    }
                }
                // Create it in k8s
                assert_eq!(counters.lock().unwrap().create_calls, 1);

                // Note that now we dont ask IC to create a new crendential, since the pool has enough credentials
                // We add labels and we activate new credential
                assert_message! {
                    (m :: IdentityCreatorProtocol::ModifyIdentity {..} in identity_creator_rx) => {
                        if let IdentityCreatorProtocol::ModifyIdentity {
                            service_credentials,
                            name,
                            labels,
                            active,
                        } = m {
                            assert!(service_credentials.id == "id1");
                            assert!(name == "srv1-ns1");
                            assert!(active);
                            assert!(labels == HashMap::from([
                                ("namespace".to_string(), "srv1".to_string()),
                                ("name".to_string(), "ns1".to_string())
                            ]));
                        }
                    }
                }
            }
        }
    }

    #[tokio::test]
    async fn test_identity_manager_request_service_identity_3() {
        test_identity_manager! {
            (watcher_rx, identity_manager_tx, identity_creator_rx, _deployment_watched_rx, _counters) => {
                assert_message!(m :: IdentityManagerProtocol::IdentityManagerInitialized in watcher_rx);
                assert_message!(m :: IdentityManagerProtocol::IdentityManagerStarted in watcher_rx);
                assert_message!(m :: IdentityCreatorProtocol::StartService in identity_creator_rx);
                // Request a new ServiceIdentity and give it time to process it
                let tx = identity_manager_tx.clone();

                // Push 11 activated UserCredentialsRef, they should not be added to the pool
                for i in 1..12 {
                    tx.send(IdentityManagerProtocol::FoundUserCredentials{
                        user_credentials_ref: credentials_ref!(i),
                        activated: true
                    }).await.expect("Unable to send message!");

                    assert_message! {
                        (m :: IdentityManagerProtocol::IdentityManagerDebug(_) in watcher_rx) => {
                        let expected_msg = format!("Found activated UserCredentialsRef with id id{}", i);
                         if let IdentityManagerProtocol::IdentityManagerDebug(msg) = m {
                                assert!(msg.eq(expected_msg.as_str()),
                                        "Wrong message, expected {} but got {}", expected_msg.as_str(), msg);
                            }
                        }
                    }
                }
                // Request a new ServiceIdentity
                tx.send(IdentityManagerProtocol::RequestServiceIdentity {
                service_candidate: deployment!("ns1", "srv1"),
                }).await.expect("Unable to send RequestServiceIdentity message to IdentityManager");

                // Since we have an empty pool we can not create identities
                assert_message! {
                    (m :: IdentityManagerProtocol::IdentityManagerDebug(_) in watcher_rx) => {
                     if let IdentityManagerProtocol::IdentityManagerDebug(msg) = m {
                            assert!(msg.eq("New ServiceIdentity requested for ServiceCandidate srv1-ns1"),
                                    "Wrong message, got {}", msg);
                        }
                    }
                }
                assert_message! {
                    (m :: IdentityManagerProtocol::IdentityManagerDebug(_) in watcher_rx) => {
                     if let IdentityManagerProtocol::IdentityManagerDebug(msg) = m {
                            assert!(msg.eq("Unable to assign service identity for service srv1-ns1. Identities pool seems to be empty!"),
                                    "Wrong message, got {}", msg);
                        }
                    }
                }
                assert_no_message!(identity_creator_rx);

                // Push a fresh credential now
                tx.send(IdentityManagerProtocol::FoundUserCredentials{
                    user_credentials_ref: credentials_ref!(13),
                    activated: false
                }).await.expect("Unable to send message!");
                // Request a new ServiceIdentity
                tx.send(IdentityManagerProtocol::RequestServiceIdentity {
                    service_candidate: deployment!("ns1", "srv1"),
                }).await.expect("Unable to send RequestServiceIdentity message to IdentityManager");
                // We ask IC to create a new crendential
                assert_message!(m :: IdentityCreatorProtocol::CreateIdentity in identity_creator_rx);
                assert_message!(m :: IdentityCreatorProtocol::ModifyIdentity{..} in identity_creator_rx);
                assert_no_message!(identity_creator_rx);
            }
        }
    }

    #[tokio::test]
    async fn test_identity_manager_identity_creator_ready_0() {
        test_identity_manager! {
            (_watcher_rx, identity_manager_tx, identity_creator_rx, _deployment_watched_rx, _counters) => {
                // Normal startup, nothing to cleanup
                assert_message!(m :: IdentityCreatorProtocol::StartService in identity_creator_rx);
                assert_no_message!(identity_creator_rx);
            }
        }
    }

    #[tokio::test]
    async fn test_identity_manager_identity_creator_ready_1() {
        test_identity_manager! {
            (im(vec![]), watcher_rx, identity_manager_tx, identity_creator_rx, _deployment_watched_rx, _counters) => {
                let tx = identity_manager_tx.clone();
                assert_message!(m :: IdentityManagerProtocol::IdentityManagerInitialized in watcher_rx);
                assert_message!(m :: IdentityManagerProtocol::IdentityManagerStarted in watcher_rx);
                assert_message!(m :: IdentityCreatorProtocol::StartService in identity_creator_rx);
                // Push some a
                for i in 1..12 {
                    tx.send(IdentityManagerProtocol::FoundUserCredentials{
                        user_credentials_ref: credentials_ref!(i),
                        activated: true
                    }).await.expect("Unable to send message!");

                    assert_message! {
                        (m :: IdentityManagerProtocol::IdentityManagerDebug(_) in watcher_rx) => {
                        let expected_msg = format!("Found activated UserCredentialsRef with id id{}", i);
                         if let IdentityManagerProtocol::IdentityManagerDebug(msg) = m {
                                assert!(msg.eq(expected_msg.as_str()),
                                        "Wrong message, expected {} but got {}", expected_msg.as_str(), msg);
                            }
                        }
                    }
                }
                // Add one deactivated
                tx.send(IdentityManagerProtocol::FoundUserCredentials{
                    user_credentials_ref: credentials_ref!(13),
                    activated: false
                }).await.expect("Unable to send message!");

                // Notify that IdentityCreator is ready
                tx.send(IdentityManagerProtocol::IdentityCreatorReady).await.expect("Unable to send message!");
                // Notify the DeploymentWatcher is ready
                tx.send(IdentityManagerProtocol::DeploymentWatcherReady).await.expect("Unable to send message!");
                let mut extra_credentials_expected: HashSet<String> = HashSet::from_iter((1 .. 12).map(|i| format!("id{}", i)).collect::<Vec<_>>());
                for _ in 1 .. 12 {
                    assert_message!(m :: IdentityCreatorProtocol::DeleteIdentity(_) in identity_creator_rx);
                    if let IdentityCreatorProtocol::DeleteIdentity(id) = m {
                        if extra_credentials_expected.contains(&id) {
                            extra_credentials_expected.remove(&id);
                        } else {
                            assert!(false, "Deleted extra credential with id {}", id);
                        }
                    } else {
                        assert!(false, "Got wrong message!");
                    }
                }
                assert!(extra_credentials_expected.is_empty(),
                 "There were credentials that should be removed but they weren't: {:?}", extra_credentials_expected);
            }
        }
    }

    #[tokio::test]
    async fn test_identity_manager_identity_creator_ready_2() {
        test_identity_manager! {
            (watcher_rx, identity_manager_tx, _identity_creator_rx, _deployment_watched_rx, counters) => {
                // Notify that IdentityCreator is ready
                assert_eq!(counters.lock().unwrap().delete_calls, 0);
                assert_message!(m :: IdentityManagerProtocol::IdentityManagerInitialized in watcher_rx);
                assert_message!(m :: IdentityManagerProtocol::IdentityManagerStarted in watcher_rx);
                let tx = identity_manager_tx.clone();
                tx.send(IdentityManagerProtocol::IdentityCreatorReady).await.expect("Unable to send message!");
                tx.send(IdentityManagerProtocol::DeploymentWatcherReady).await.expect("Unable to send message!");
                // Syncing UserCredentials log message
                assert_message!(m :: IdentityManagerProtocol::IdentityManagerDebug(_) in watcher_rx);
                // Syncing ServiceIdentities log message
                assert_message!(m :: IdentityManagerProtocol::IdentityManagerDebug(_) in watcher_rx);
                let mut extra_service_identities: HashSet<String> = HashSet::from_iter((1 .. 5)
                    .map(|i| format!("ServiceIdentity with id ns{}-srv{} unregistered", i, i))
                    .collect::<Vec<_>>());
                for i in 1 .. 5 {
                    assert_message!(m :: IdentityManagerProtocol::IdentityManagerDebug(_) in watcher_rx);
                    if let IdentityManagerProtocol::IdentityManagerDebug(msg) = m {
                        if extra_service_identities.contains(&msg) {
                            extra_service_identities.remove(&msg);
                        } else {
                            assert!(false, "Deleted extra ServiceIdentity: {} - {}", msg, i);
                        }
                    } else {
                        assert!(false, "Got wrong message!");
                    }
                }
                assert_no_message!(watcher_rx);
                assert!(extra_service_identities.is_empty(),
                "There were ServiceIdentities that should be removed but they weren't: {:?}", extra_service_identities);
                assert_eq!(counters.lock().unwrap().delete_calls, 4);
            }
        }
    }
}
