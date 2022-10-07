use futures::Future;
use k8s_openapi::api::apps::v1::Deployment;
use kube::api::{DeleteParams, ListParams, PostParams};
use kube::{Api, Client};
use log::{error, info, warn};
pub use sdp_common::crd::{ServiceIdentity, ServiceIdentitySpec};
use sdp_common::service::{ServiceLookup, ServiceUser};
use sdp_common::traits::{HasCredentials, Labelled, Named, Namespaced, Service};
use sdp_macros::{queue_debug, sdp_error, sdp_info, sdp_log, sdp_warn, when_ok};
use std::collections::{HashMap, HashSet, VecDeque};
use std::fmt;
use std::iter::FromIterator;
use std::pin::Pin;
use tokio::sync::mpsc::{Receiver, Sender};

use crate::deployment_watcher::DeploymentWatcherProtocol;
use crate::errors::IdentityServiceError;
use crate::identity_creator::IdentityCreatorProtocol;

/// Trait that represents the pool of ServiceUser entities
/// We can pop and push ServiceUser entities
trait ServiceUsersPool {
    fn pop(&mut self) -> Option<ServiceUser>;
    fn push(&mut self, user_credentials_ref: ServiceUser) -> ();
    fn needs_new_credentials(&self) -> bool;
}

/// Trait for ServiceIdentity provider
/// This traits provides instances of To from instances of From
trait ServiceIdentityProvider {
    type From: Service + Labelled + Send;
    type To: Service + HasCredentials + Send;
    fn register_identity(&mut self, to: Self::To) -> ();
    fn unregister_identity(&mut self, to: &Self::To) -> Option<Self::To>;
    fn next_identity(&mut self, from: &Self::From) -> Option<Self::To>;
    fn identity(&self, from: &Self::From) -> Option<&Self::To>;
    fn identities(&self) -> Vec<&Self::To>;

    /// ServiceIdentity instances without known ServiceCandidate
    fn extra_service_identities<'a>(
        &self,
        current_service_candidates: &HashSet<String>,
    ) -> Vec<&Self::To> {
        self.identities()
            .iter()
            .filter(|id| {
                let service_id = id.service_id();
                service_id.is_ok() && !current_service_candidates.contains(&service_id.unwrap())
            })
            .map(|id| *id)
            .collect()
    }

    /// Active ServiceUser instances (names) not being in use
    fn orphan_service_users<'a>(
        &'a self,
        activated_service_users: &'a HashSet<String>,
    ) -> HashSet<String> {
        let current_activated_credentials: HashSet<String> = HashSet::from_iter(
            self.identities()
                .iter()
                .map(|&i| i.credentials().name.clone()),
        );
        activated_service_users
            .difference(&current_activated_credentials)
            .map(|s| s.clone())
            .collect()
    }

    /// ServiceIdentity instances that don't have active ServiceUsers
    fn orphan_service_identities<'a>(
        &self,
        activated_service_users: &'a HashSet<String>,
    ) -> Vec<&Self::To> {
        self.identities()
            .iter()
            .filter(|i| !activated_service_users.contains(&i.credentials().name))
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
    ) -> Pin<Box<dyn Future<Output = Result<ServiceIdentity, IdentityServiceError>> + Send + '_>>;
    fn update<'a>(
        &'a self,
        identity: &'a ServiceIdentity,
    ) -> Pin<Box<dyn Future<Output = Result<ServiceIdentity, IdentityServiceError>> + Send + '_>>;
    fn delete<'a>(
        &'a self,
        identity_name: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<(), IdentityServiceError>> + Send + '_>>;
    fn list<'a>(
        &'a self,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<ServiceIdentity>, IdentityServiceError>> + Send + '_>>;
}

/// Messages exchanged between the the IdentityCreator and IdentityManager
#[derive(Debug)]
pub enum IdentityManagerProtocol<From: Service, To: Service + HasCredentials> {
    /// Message used to request a new ServiceIdentity for ServiceCandidate
    RequestServiceIdentity {
        service_candidate: From,
    },
    DeleteServiceIdentity(To),
    FoundServiceCandidate(From),
    DeletedServiceCandidate(From),
    /// Message to notify that a new ServiceUser have been created
    /// IdentityCreator creates these ServiceUSer
    FoundServiceUser(ServiceUser, bool),
    IdentityCreatorReady,
    DeploymentWatcherReady,
    IdentityManagerInitialized,
    IdentityManagerStarted,
    IdentityManagerReady,
    IdentityManagerDebug(String),
    // service_user, service_ns, service_name
    ActivatedServiceUser(ServiceUser, String, String),
}

pub enum IdentityMessageResponse {
    /// Message used to send a new user
    NewIdentity(ServiceUser),
    IdentityUnavailable,
}

#[derive(Default)]
struct IdentityManagerPool {
    pool: VecDeque<ServiceUser>,
    services: HashMap<String, ServiceIdentity>,
}

impl ServiceUsersPool for IdentityManagerPool {
    fn pop(&mut self) -> Option<ServiceUser> {
        self.pool.pop_front()
    }

    fn push(&mut self, user_credentials_ref: ServiceUser) -> () {
        self.pool.push_back(user_credentials_ref)
    }

    fn needs_new_credentials(&self) -> bool {
        self.pool.len() < 10
    }
}

impl ServiceIdentityProvider for IdentityManagerPool {
    type From = ServiceLookup;
    type To = ServiceIdentity;

    fn register_identity(&mut self, to: Self::To) -> () {
        when_ok!((service_id = to.service_id()) {
            self.services.insert(service_id, to)
        });
    }

    fn unregister_identity(&mut self, to: &Self::To) -> Option<Self::To> {
        when_ok!((service_id: Self::To = to.service_id()) {
            self.services.remove(&service_id)
        })
    }

    fn next_identity(&mut self, from: &Self::From) -> Option<Self::To> {
        when_ok!((service_id: Self::To = from.service_id()) {
            let service_name = from.service_name().unwrap(); // Safe since service_name is defined
            self.services.get(&service_id)
                .map(|i| i.clone())
                .or_else(|| {
                    if let Some(id) = self.pop().map(|service_user| {
                        let service_identity_spec = ServiceIdentitySpec {
                            service_name: Named::name(from),
                            service_namespace: Namespaced::namespace(from).unwrap(), // Safe since if it has service_name it has namespace
                            service_user,
                            labels: Labelled::labels(from).unwrap(), // Safe since if it has service_name it has labels
                            disabled: false,
                        };
                        ServiceIdentity::new(&service_name, service_identity_spec)
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
        })
    }

    fn identity(&self, from: &Self::From) -> Option<&Self::To> {
        when_ok!((service_id: &Self::To = from.service_id()) {
            self.services.get(&service_id)
        })
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

async fn update_impl<'a>(
    api: &Api<ServiceIdentity>,
    identity: &'a ServiceIdentity,
) -> Result<ServiceIdentity, IdentityServiceError> {
    if let Some(mut obj) = api.get_opt(&identity.service_name()?).await? {
        obj.spec = identity.spec.clone();
        let new_obj = api
            .replace(&identity.service_name()?, &PostParams::default(), &obj)
            .await?;
        Ok(new_obj)
    } else {
        Err(IdentityServiceError::from(format!(
            "ServiceIdentity {} not found",
            identity.service_name().unwrap()
        )))
    }
}

impl ServiceIdentityAPI for KubeIdentityManager {
    fn create<'a>(
        &'a self,
        identity: &'a ServiceIdentity,
    ) -> Pin<Box<dyn Future<Output = Result<ServiceIdentity, IdentityServiceError>> + Send + '_>>
    {
        let fut = async move {
            when_ok!((service_id: Result<ServiceIdentity, IdentityServiceError> = identity.service_id()) {
                match self
                    .service_identity_api
                    .get_opt(&service_id)
                    .await
                    .map_err(IdentityServiceError::from)
                {
                    Ok(None) => {
                        info!(
                            "ServiceIdentity {} does not exist, creating it.",
                            service_id
                        );
                        Some(self.service_identity_api
                            .create(&PostParams::default(), identity)
                            .await.map_err(IdentityServiceError::from))
                    }
                    Ok(_) => {
                        info!("ServiceIdentity {} already exists.", service_id);
                        Some(Ok(identity.clone()))
                    }
                    Err(e) => {
                        error!(
                            "Error checking if service identity {} exists.",
                            service_id
                        );
                        Some(Err(e))
                    }
                }
            })
            .unwrap_or(Err(IdentityServiceError::from("ServiceIdentity is not a service!".to_string())))
        };
        Box::pin(fut)
    }

    fn update<'a>(
        &'a self,
        identity: &'a ServiceIdentity,
    ) -> Pin<Box<dyn Future<Output = Result<ServiceIdentity, IdentityServiceError>> + Send + '_>>
    {
        let fut = async move { update_impl(&self.service_identity_api, identity).await };
        Box::pin(fut)
    }

    fn delete<'a>(
        &'a self,
        identity_name: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<(), IdentityServiceError>> + Send + '_>> {
        let fut = async move {
            self.service_identity_api
                .delete(identity_name, &DeleteParams::default())
                .await
                .map(|_| ())
                .map_err(IdentityServiceError::from)
        };
        Box::pin(fut)
    }

    fn list<'a>(
        &'a self,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<ServiceIdentity>, IdentityServiceError>> + Send + '_>>
    {
        let fut = async move {
            self.service_identity_api
                .list(&ListParams::default())
                .await
                .map(|xs| xs.items)
                .map_err(IdentityServiceError::from)
        };
        Box::pin(fut)
    }
}

trait IdentityManager<From: Service + Send + Sync, To: Service + HasCredentials + Send + Sync>:
    ServiceIdentityAPI + ServiceIdentityProvider<From = ServiceLookup, To = To> + ServiceUsersPool
{
}

pub struct IdentityManagerRunner<From: Service + Send, To: Service + HasCredentials + Send> {
    im: Box<dyn IdentityManager<From, To> + Send + Sync>,
}

/// Load all the current ServiceIdentity
/// Flow between services is:
/// - IM collects all ServiceIdentity defined
/// - IM asks IC to collect current ServiceUser
/// - IC notifies IM with defined ServiceUser (active and not active)
/// - DW notifies IM with the current defined ServiceCandidates
/// - IM cleans up extra ServiceUser (credentials active in system without a ServiceIdentrity)
/// - IM cleans up ServiceIdentities that have ServiceUser not active
/// - IM cleans up ServiceIdentities that don't have a ServiceCandidate attached
/// - IM asks DW to start
/// - DW sends the list of all CandidateServices (Deployments)
/// - IM creates ServiceIDentity for those CandidateServices that need it and dont have one.
impl IdentityManagerRunner<ServiceLookup, ServiceIdentity> {
    pub fn kube_runner(client: Client) -> IdentityManagerRunner<ServiceLookup, ServiceIdentity> {
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

    async fn run_identity_manager<F: Service + Labelled + Clone + fmt::Debug + Send>(
        im: &mut Box<dyn IdentityManager<ServiceLookup, ServiceIdentity> + Send + Sync>,
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
                IdentityManagerProtocol::DeleteServiceIdentity(service_identity) => {
                    when_ok!((service_id = service_identity.service_id()) {
                        info!(
                            "Deleting ServiceIdentity with id {}",
                            service_id
                        );
                        match im.delete(&service_id).await {
                            Ok(_) => {
                                info!(
                                    "Deregistering ServiceIdentity with id {}",
                                    service_id
                                );

                                // Unregister the identity
                                if let Some(s) = im.unregister_identity(&service_identity) {
                                    sdp_info!(IdentityManagerProtocol::<F, ServiceIdentity>::IdentityManagerDebug |
                                        ("ServiceIdentity with id {} unregistered", s.service_id().unwrap_or(service_id.clone())
                                    ) => external_queue_tx);
                                } else {
                                    sdp_warn!(IdentityManagerProtocol::<F, ServiceIdentity>::IdentityManagerDebug | (
                                        "ServiceIdentity with id {} was not registered", service_id
                                    ) => external_queue_tx);
                                }

                                // Ask IdentityCreator to remove the IdentityCredential
                                info!(
                                    "Asking for deletion of IdentityCredential {} from SDP system",
                                    service_identity.credentials().name
                                );
                                if let Err(err) = identity_creator_tx
                                    .send(IdentityCreatorProtocol::DeleteServiceUser(
                                        service_identity.credentials().clone(),
                                        service_identity.namespace().unwrap(),
                                        service_identity.name(),
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
                                    service_id,
                                    err
                                ) => external_queue_tx);
                            }
                        }
                    });
                }
                IdentityManagerProtocol::RequestServiceIdentity { service_candidate } => {
                    when_ok!((service_id = service_candidate.service_id()) {
                        sdp_info!(IdentityManagerProtocol::<F, ServiceIdentity>::IdentityManagerDebug |(
                            "New ServiceIdentity requested for ServiceCandidate {}",
                            service_id
                        ) => external_queue_tx);
                        let a = ServiceLookup::try_from_service(&service_candidate).expect("Unable to conver service");
                        match im.next_identity(&a) {
                            Some(service_identity) => match im.create(&service_identity).await {
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
                                        .send(IdentityCreatorProtocol::ActivateServiceUser(
                                            service_identity.credentials().clone(),
                                            service_identity.namespace().unwrap(),
                                            service_identity.name(),
                                            service_candidate.labels().unwrap(),
                                        ))
                                        .await
                                    {
                                        error!("Error updating ServiceUser in ServiceUser with name {} for service {}: {}",
                                        service_identity.credentials().name, service_identity.service_id().unwrap(), err);
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
                        }
                    });
                }
                IdentityManagerProtocol::FoundServiceCandidate(service_candidate) => {
                    when_ok!((candidate_service_id = service_candidate.service_id()) {
                        let service_lookup = ServiceLookup::try_from_service(&service_candidate).unwrap();
                        if let Some(service_identity) = im.identity(&service_lookup) {
                            when_ok!((service_id = service_identity.service_id()) {
                                sdp_info!(IdentityManagerProtocol::<F, ServiceIdentity>::IdentityManagerDebug |(
                                    "Found registered ServiceCandidate {}",
                                    service_id
                                ) => external_queue_tx);
                            });
                        } else {
                                sdp_info!(IdentityManagerProtocol::<F, ServiceIdentity>::IdentityManagerDebug |(
                                    "Found unregistered ServiceCandidate {}, registering it",
                                    candidate_service_id
                                ) => external_queue_tx);

                                identity_manager_tx
                                    .send(IdentityManagerProtocol::RequestServiceIdentity {
                                        service_candidate: service_candidate.clone(),
                                    })
                                    .await
                                    .expect("Error requesting new ServiceIdentity");
                        }
                        existing_service_candidates.insert(candidate_service_id);
                    });
                }
                // Identity Creator notifies about fresh, unactivated User Credentials
                IdentityManagerProtocol::FoundServiceUser(service_user, activated)
                    if !activated =>
                {
                    sdp_info!(IdentityManagerProtocol::<F, ServiceIdentity>::IdentityManagerDebug |(
                        "Found deactivated ServiceUser with name {}",
                        service_user.name
                    ) => external_queue_tx);
                    existing_deactivated_credentials.insert(service_user.name.clone());
                    im.push(service_user);
                }
                // Identity Creator notifies about already activated User Credentials
                IdentityManagerProtocol::FoundServiceUser(service_user, activated) if activated => {
                    existing_activated_credentials.insert(service_user.name.clone());
                    sdp_info!(IdentityManagerProtocol::<F, ServiceIdentity>::IdentityManagerDebug |(
                        "Found activated ServiceUser with name {}",
                        service_user.name
                    ) => external_queue_tx);
                }
                // Identity Creator notifies about already activated User Credentials
                IdentityManagerProtocol::ActivatedServiceUser(
                    service_user,
                    service_ns,
                    service_name,
                ) => {
                    info!(
                        "ServiceUser {} has been activated [{}/{}]",
                        &service_user.name, service_ns, service_name
                    );
                    let _id = im.identity(&ServiceLookup::new(&service_ns, &service_name, None));
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
                    let mut removed_service_identities: HashSet<String> = HashSet::new();
                    info!("IdentityMAnager is ready");

                    sdp_info!(IdentityManagerProtocol::<F, ServiceIdentity>::IdentityManagerDebug |("Syncing UserCredentials") => external_queue_tx);

                    // Here we do a basic cleanup of the current state
                    // 1. First we ask for deletion for all the ServiceIdentity instances that don't have a known
                    //    ServiceCandidate. This will eventually remove the ServiceUser associated
                    // 2. Now we remove all the known active ServiceUser instances that don't belong to any ServiceIdentity
                    // 3. Finally we delete any ServiceIdentity instance that does not have a ServiceUser activated

                    sdp_info!(IdentityManagerProtocol::<F, ServiceIdentity>::IdentityManagerDebug |("Syncing ServiceIdentity instances") => external_queue_tx);

                    // 1. Delete IdentityService instances with not known ServiceCandidate
                    for service_identity in
                        im.extra_service_identities(&existing_service_candidates)
                    {
                        when_ok!((service_id = service_identity.service_id()) {
                            info!(
                                "Found ServiceIdentity {} with not known ServiceCandidate. Deleting it.",
                                service_id
                            );
                            if let Err(e) = identity_manager_tx
                                .send(IdentityManagerProtocol::DeleteServiceIdentity(
                                    service_identity.clone(),
                                ))
                                .await
                            {
                                error!(
                                    "Error requesting deleting of ServiceIdentity {}: {}",
                                    service_id,
                                    e.to_string()
                                )
                            } else {
                                // Make sure we dont try to delete it twice!
                                removed_service_identities
                                    .insert(service_id.clone());
                            }
                        });
                    }

                    // 2. Delete ServiceUser instances that don't belong to any ServiceIdentity
                    for sdp_user_name in im.orphan_service_users(&existing_activated_credentials) {
                        info!(
                            "SDPUser {} is active but not used by any ServiceIdentity, deleting it",
                            sdp_user_name
                        );
                        identity_creator_tx
                            .send(IdentityCreatorProtocol::DeleteSDPUser(sdp_user_name))
                            .await
                            .expect("Unable to delete obsolete SDPUser");
                    }

                    // 3. Delete IdentityService instances holding not active credentials
                    for service_identity in
                        im.orphan_service_identities(&existing_activated_credentials)
                    {
                        when_ok!((service_id = service_identity.service_id()) {
                            if !removed_service_identities.contains(&service_id) {
                                info!(
                                    "ServiceIdentity {} has deactivated ServiceUser. Deleting it.",
                                    service_id
                                );
                                if let Err(e) = identity_manager_tx
                                    .send(IdentityManagerProtocol::DeleteServiceIdentity(
                                        service_identity.clone(),
                                    ))
                                    .await
                                {
                                    error!(
                                        "Error requesting deleting of ServiceIdentity {}: {}",
                                        service_id,
                                        e.to_string()
                                    )
                                } else {
                                    // Make sure we dont try to delete it twice!
                                    removed_service_identities
                                        .insert(service_id.clone());
                                }
                            }
                        });
                    }

                    deployment_watcher_proto_tx
                        .send(DeploymentWatcherProtocol::IdentityManagerReady)
                        .await
                        .expect("Unable to notify DeploymentWatcher!");
                }
                IdentityManagerProtocol::DeletedServiceCandidate(service_candidate) => {
                    when_ok!((candidate_service_id = service_candidate.service_id()) {
                        let service_lookup = ServiceLookup::try_from_service(&service_candidate).unwrap();
                        match im.identity(&service_lookup) {
                            Some(identity_service) => {
                                if let Err(e) = identity_manager_tx
                                    .send(IdentityManagerProtocol::DeleteServiceIdentity(
                                        identity_service.clone(),
                                    ))
                                    .await
                                {
                                    // TODO: We should retry later
                                    error!(
                                        "Unable to queue message to delete ServiceIdentity {}: {}",
                                        identity_service.service_id().unwrap(),
                                        e.to_string()
                                    );
                                };
                            }
                            None => {
                                error!("Deleted ServiceCandidate {} has not IdentitityService attached, ignoring!",
                                candidate_service_id);
                            }
                        }
                    });
                }
                _ => {
                    warn!("Ignored message");
                }
            }
        }
    }

    async fn initialize<F: Service + Labelled + Send>(
        im: &mut Box<dyn IdentityManager<F, ServiceIdentity> + Send + Sync>,
    ) -> () {
        info!("Initializing Identity Manager service");
        match im.list().await {
            Ok(xs) => {
                info!("Restoring previous Service Identity instances");
                let n: u32 = xs
                    .iter()
                    .map(|s| {
                        info!("Restoring Service Identity {}", s.service_id().unwrap());
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
    use kube::{core::object::HasSpec, ResourceExt};
    use sdp_common::{
        service::ServiceLookup,
        traits::{HasCredentials, Service},
    };
    use sdp_macros::{deployment, service_identity, service_user};
    use sdp_test_macros::{assert_message, assert_no_message};
    use tokio::sync::mpsc::channel;
    use tokio::time::{sleep, timeout, Duration};

    use crate::{
        deployment_watcher::DeploymentWatcherProtocol, identity_creator::IdentityCreatorProtocol,
        identity_manager::IdentityManagerProtocol,
    };
    use crate::{errors::IdentityServiceError, identity_manager::ServiceUser};

    use super::{
        IdentityManager, IdentityManagerPool, IdentityManagerRunner, ServiceIdentity,
        ServiceIdentityAPI, ServiceIdentityProvider, ServiceIdentitySpec, ServiceUsersPool,
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
        update_calls: usize,
    }

    #[sdp_proc_macros::identity_provider()]
    #[derive(sdp_proc_macros::IdentityProvider, Default)]
    #[IdentityProvider(From = "ServiceLookup", To = "ServiceIdentity")]
    struct TestIdentityManager {
        api_counters: Arc<Mutex<APICounters>>,
    }

    impl TestIdentityManager {
        fn _reset_counters(&self) -> () {
            let mut api_counters = self.api_counters.lock().unwrap();
            api_counters.delete_calls = 0;
            api_counters.create_calls = 0;
            api_counters.list_calls = 0;
            api_counters.update_calls = 0;
        }
    }

    impl ServiceIdentityAPI for TestIdentityManager {
        fn create<'a>(
            &'a self,
            identity: &'a ServiceIdentity,
        ) -> Pin<Box<dyn Future<Output = Result<ServiceIdentity, IdentityServiceError>> + Send + '_>>
        {
            self.api_counters.lock().unwrap().create_calls += 1;
            Box::pin(future::ready(Ok(identity.clone())))
        }

        fn delete<'a>(
            &'a self,
            _: &'a str,
        ) -> Pin<Box<dyn Future<Output = Result<(), IdentityServiceError>> + Send + '_>> {
            self.api_counters.lock().unwrap().delete_calls += 1;
            Box::pin(future::ready(Ok(())))
        }

        fn list<'a>(
            &'a self,
        ) -> Pin<
            Box<
                dyn Future<Output = Result<Vec<ServiceIdentity>, IdentityServiceError>> + Send + '_,
            >,
        > {
            self.api_counters.lock().unwrap().list_calls += 1;
            Box::pin(future::ready(Ok(vec![])))
        }

        fn update<'a>(
            &'a self,
            identity: &'a ServiceIdentity,
        ) -> Pin<Box<dyn Future<Output = Result<ServiceIdentity, IdentityServiceError>> + Send + '_>>
        {
            self.api_counters.lock().unwrap().list_calls += 1;
            Box::pin(future::ready(Ok(identity.clone())))
        }
    }

    fn new_test_identity_runner(
        im: Box<TestIdentityManager>,
    ) -> IdentityManagerRunner<ServiceLookup, ServiceIdentity> {
        IdentityManagerRunner {
            im: im as Box<dyn IdentityManager<ServiceLookup, ServiceIdentity> + Send + Sync>,
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
        let d1 = deployment!("ns1", "dep1");
        let d2 = deployment!("ns1", "srv1");
        test_service_identity_provider! {
            im(identities) => {
                assert_eq!(im.identities().len(), 4);
                let mut is0: Vec<ServiceIdentity> = im.identities().iter().map(|&i| i.clone()).collect();
                let sorted_is0 = is0.sort_by(|a, b| a.spec().service_name.partial_cmp(&b.spec().service_name).unwrap());
                let mut is1: Vec<ServiceIdentity> = identities.iter().map(|i| i.clone()).collect();
                let sorted_is1 = is1.sort_by(|a, b| a.spec().service_name.partial_cmp(&b.spec().service_name).unwrap());
                assert_eq!(sorted_is0, sorted_is1);
                assert!(im.identity(&ServiceLookup::try_from_service(&d1).unwrap()).is_none());
                assert!(im.identity(&ServiceLookup::try_from_service(&d2).unwrap()).is_some());
            }
        }
    }

    fn check_service_identity(
        si: Option<ServiceIdentity>,
        c: &ServiceUser,
        service_name: &str,
        service_ns: &str,
    ) -> () {
        assert!(si.is_some());
        let si_spec = si.unwrap();
        assert_eq!(si_spec.spec().service_name, service_name);
        assert_eq!(si_spec.spec().service_namespace, service_ns);
        assert_eq!(si_spec.spec().service_user.name, c.name);
    }

    #[test]
    fn test_service_identity_provider_next_identity() {
        let id1 = service_identity!(1);
        let identities = vec![id1.clone()];
        let d1_1 = deployment!("ns1", "srv1");
        let d2_2 = deployment!("ns2", "srv2");
        let d1_2 = deployment!("ns2", "srv1");
        let d3_1 = deployment!("ns1", "srv3");

        test_service_identity_provider! {
            im(identities) => {
                // service not registered but we don't have any credentials so we can not create
                // new identities for it.
                assert_eq!(im.identities().len(), 1);
                assert!(im.next_identity(&ServiceLookup::try_from_service(&d1_2).unwrap()).is_none());
                assert!(im.next_identity(&ServiceLookup::try_from_service(&d2_2).unwrap()).is_none());

                // service already registered, we just return the credentials we have for it
                let d1_id = im.next_identity(&ServiceLookup::try_from_service(&d1_1).unwrap());
                check_service_identity(d1_id, &id1.spec().service_user, "srv1", "ns1");
            }
        }

        test_service_identity_provider! {
            im(identities) => {
                let c1 = service_user!(1);
                let c2 = service_user!(2);
                // push some credentials
                im.push(c1.clone());
                im.push(c2.clone());

                assert_eq!(im.identities().len(), 1);

                // ask for a new service identity, we can create it because we have creds
                let d2_2_id = im.next_identity(&ServiceLookup::try_from_service(&d2_2).unwrap());
                check_service_identity(d2_2_id, &c1, "srv2", "ns2");

                // service identity already registered
                let d1_1_id = im.next_identity(&ServiceLookup::try_from_service(&d1_1).unwrap());
                assert_eq!(d1_1_id.unwrap().spec(), id1.spec());

                // ask for a new service identity, we can create it because we have creds
                let d1_2_id = im.next_identity(&ServiceLookup::try_from_service(&d1_2).unwrap());
                check_service_identity(d1_2_id, &c2, "srv1", "ns2");

                // service not registered but we don't have any credentials so we can not create
                // new identities for it.
                assert!(im.next_identity(&ServiceLookup::try_from_service(&d3_1).unwrap()).is_none());

                // We can still get the service identities we have registered
                let d2_2_id = im.next_identity(&ServiceLookup::try_from_service(&d2_2).unwrap());
                check_service_identity(d2_2_id, &c1, "srv2", "ns2");
                let d1_2_id = im.next_identity(&ServiceLookup::try_from_service(&d1_2).unwrap());
                check_service_identity(d1_2_id, &c2, "srv1", "ns2");

                // We have created 2 ServiceIdentity so they are now registered
                assert_eq!(im.identities().len(), 3);
                let mut identities = im.identities();
                identities.sort_by(|a, b| a.service_id().unwrap().partial_cmp(&b.service_id().unwrap()).unwrap());
                let identities: Vec<String> = identities.iter().map(|i| i.service_id().unwrap()).collect();
                assert_eq!(identities, vec!["ns1_srv1", "ns2_srv1", "ns2_srv2"]);
            }
        }
    }

    fn service_identities_to_tuple<'a>(
        service_identities: Vec<&'a ServiceIdentity>,
    ) -> Vec<(&'a str, &'a str, &'a str)> {
        let mut xs: Vec<(&str, &str, &str)> = service_identities
            .iter()
            .map(|si| {
                let spec = si.spec();
                (
                    spec.service_namespace.as_str(),
                    spec.service_name.as_str(),
                    spec.service_user.name.as_str(),
                )
            })
            .collect();
        xs.sort();
        xs
    }

    #[test]
    fn test_service_identity_provider_extras_identities() {
        // extra_service_identities compute the service identities registered in memory
        // that dont have a counterpart in the system (k8s for example)
        test_service_identity_provider! {
            im => {
                let mut identities = im.identities();
                identities.sort_by(|a, b| a.name_any().as_str().partial_cmp(b.name_any().as_str()).unwrap());

                // 4 service identities registered but none found on start,
                let xs = service_identities_to_tuple(
                    im.extra_service_identities(&HashSet::new()));
                assert_eq!(xs.len(), 4);
                assert_eq!(xs, vec![
                    ("ns1", "srv1", "service_user1"),
                    ("ns2", "srv2", "service_user2"),
                    ("ns3", "srv3", "service_user3"),
                    ("ns4", "srv4", "service_user4"),
                ]);

                // 4 service identities registered and only 1 on start
                let xs = service_identities_to_tuple(
                    im.extra_service_identities(&HashSet::from(
                    [identities[0].service_id().unwrap()]
                )));
                assert_eq!(xs.len(), 3);
                assert_eq!(xs, vec![
                    ("ns2", "srv2", "service_user2"),
                    ("ns3", "srv3", "service_user3"),
                    ("ns4", "srv4", "service_user4"),
                ]);

                // 4 service identities registered and all 4 on start
                let xs: Vec<(&str, &str, &str)> = service_identities_to_tuple(
                    im.extra_service_identities(&HashSet::from(
                        [
                            identities[0].service_id().unwrap(),
                            identities[1].service_id().unwrap(),
                            identities[2].service_id().unwrap(),
                            identities[3].service_id().unwrap(),
                        ])));
                assert_eq!(xs.len(), 0);
                assert_eq!(xs, vec![]);

                // 5 service identities registered and all 4 on start
                let xs: Vec<(&str, &str, &str)> = service_identities_to_tuple(
                    im.extra_service_identities(&HashSet::from(
                        [
                            identities[0].service_id().unwrap(),
                            identities[1].service_id().unwrap(),
                            identities[2].service_id().unwrap(),
                            identities[3].service_id().unwrap(),
                            service_identity!(5).service_id().unwrap(),
                        ])));
                assert_eq!(xs.len(), 0);
                assert_eq!(xs, vec![]);
            }
        }
    }

    #[test]
    fn test_service_identity_provider_orphan_identities() {
        // orphan_service_identities computes the list of service identities holding
        // ServiceUsers that are not active anymore
        test_service_identity_provider! {
            im => {
                let mut identities = im.identities();
                identities.sort_by(|a, b| a.name_any().as_str().partial_cmp(b.name_any().as_str()).unwrap());

                // 4 service identities registered none active service users,
                let xs = service_identities_to_tuple(
                    im.orphan_service_identities(&HashSet::new()));
                assert_eq!(xs.len(), 4);
                assert_eq!(xs, vec![
                    ("ns1", "srv1", "service_user1"),
                    ("ns2", "srv2", "service_user2"),
                    ("ns3", "srv3", "service_user3"),
                    ("ns4", "srv4", "service_user4"),
                ]);

                // 4 service identities registered 4 active service users,
                let xs = service_identities_to_tuple(
                    im.orphan_service_identities(&HashSet::from([
                        identities[0].credentials().name.clone(),
                        identities[1].credentials().name.clone(),
                        identities[2].credentials().name.clone(),
                        identities[3].credentials().name.clone(),
                    ])));
                assert_eq!(xs.len(), 0);
                assert_eq!(xs, vec![]);

                // 4 service identities registered 2 active service users,
                let xs = service_identities_to_tuple(
                    im.orphan_service_identities(&HashSet::from([
                        identities[1].credentials().name.clone(), // service_user2
                        identities[3].credentials().name.clone(), // service_user4
                    ])));
                assert_eq!(xs.len(), 2);
                assert_eq!(xs, vec![
                    ("ns1", "srv1", "service_user1"),
                    ("ns3", "srv3", "service_user3"),
                ]);

                // 4 service identities registered 5 active service users,
                let xs = service_identities_to_tuple(
                    im.orphan_service_identities(&HashSet::from([
                        identities[0].credentials().name.clone(),
                        identities[1].credentials().name.clone(),
                        identities[2].credentials().name.clone(),
                        identities[3].credentials().name.clone(),
                        service_identity!(5).credentials().name.clone()
                    ])));
                assert_eq!(xs.len(), 0);
                assert_eq!(xs, vec![]);

            }
        }
    }

    #[test]
    fn test_service_identity_provider_orphan_service_users() {
        // orphan_service_users computes the list of active service users that are not being
        // used by any service
        test_service_identity_provider! {
            im => {
                let mut identities = im.identities();
                identities.sort_by(|a, b| a.name_any().as_str().partial_cmp(b.name_any().as_str()).unwrap());

                // 4 service identities registered none active service users,
                let xs = im.orphan_service_users(&HashSet::new());
                assert_eq!(xs.len(), 0);
                assert_eq!(xs, HashSet::<String>::new());

                // 4 service identities registered 4 active service users,
                let xs = im.orphan_service_users(&HashSet::from([
                    "service_user1".to_string(),
                    "service_user2".to_string(),
                    "service_user3".to_string(),
                    "service_user4".to_string(),
                ]));
                assert_eq!(xs.len(), 0);
                assert_eq!(xs, HashSet::<String>::new());

                // 4 service identities registered 1 active service user but not used,
                let xs = im.orphan_service_users(&HashSet::from([
                    "service_user5".to_string(),
                ]));
                assert_eq!(xs.len(), 1);
                assert_eq!(xs, HashSet::from(["service_user5".to_string()]));

                // 4 service identities registered 8 active service users, 4 not being used
                let xs = im.orphan_service_users(&HashSet::from([
                    "service_user1".to_string(),
                    "service_user2".to_string(),
                    "service_user3".to_string(),
                    "service_user4".to_string(),
                    "service_user5".to_string(),
                    "service_user6".to_string(),
                    "service_user7".to_string(),
                    "service_user8".to_string(),
                ]));
                assert_eq!(xs.len(), 4);
                assert_eq!(xs, HashSet::from([
                    "service_user5".to_string(),
                    "service_user6".to_string(),
                    "service_user7".to_string(),
                    "service_user8".to_string(),
                    ]));
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
                tx.send(IdentityManagerProtocol::DeleteServiceIdentity(service_identity!(1)))
                    .await.expect("Unable to send DeleteServiceIdentity message to IdentityManager");
                sleep(Duration::from_millis(10)).await;

                // We tried to delete it from the API
                assert_eq!(counters.lock().unwrap().delete_calls, 1);

                // We asked the IdentityCreator to delete it
                assert_message! {
                    (m :: IdentityCreatorProtocol::DeleteServiceUser(_, _, _) in identity_creator_rx) => {
                        if let IdentityCreatorProtocol::DeleteServiceUser(service_user, service_ns, service_name) = m {
                            assert_eq!(service_user.name, "service_user1".to_string());
                            assert_eq!(service_ns, "ns1".to_string());
                            assert_eq!(service_name, "srv1".to_string());
                        }
                    }
                };

                // We could not deregister the ServiceIdentity since we dont have any one registered
                assert_message! {
                    (m :: IdentityManagerProtocol::IdentityManagerDebug(_) in watcher_rx) => {
                     if let IdentityManagerProtocol::IdentityManagerDebug(msg) = m {
                            assert!(msg.eq("ServiceIdentity with id ns1_srv1 was not registered"),
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
                tx.send(IdentityManagerProtocol::DeleteServiceIdentity(service_identity!(1)))
                    .await.expect("Unable to send DeleteServiceIdentity message to IdentityManager");
                sleep(Duration::from_millis(10)).await;

                // We tried to delete it from the API
                assert_eq!(counters.lock().unwrap().delete_calls, 1);

                // We asked the IdentityCreator to delete it
                assert_message! {
                    (m :: IdentityCreatorProtocol::DeleteServiceUser(_, _, _) in identity_creator_rx) => {
                        if let IdentityCreatorProtocol::DeleteServiceUser(service_user, service_ns, service_name) = m {
                            assert_eq!(service_user.name, "service_user1".to_string());
                            assert_eq!(service_ns, "ns1".to_string());
                            assert_eq!(service_name, "srv1".to_string());
                        }
                    }
                };

                // We could not deregister the ServiceIdentity since we dont have any one registered
                assert_message! {
                    (m :: IdentityManagerProtocol::IdentityManagerDebug(_) in watcher_rx) => {
                     if let IdentityManagerProtocol::IdentityManagerDebug(msg) = m {
                            assert!(msg.eq("ServiceIdentity with id ns1_srv1 unregistered"),
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
            (im(vec![]), watcher_rx, identity_manager_tx, identity_creator_rx, _deployment_watched_rx, _counters) => {
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
                            assert!(msg.eq("New ServiceIdentity requested for ServiceCandidate ns1_srv1"),
                                    "Wrong message, got {}", msg);
                        }
                    }
                }
                assert_message! {
                    (m :: IdentityManagerProtocol::IdentityManagerDebug(_) in watcher_rx) => {
                     if let IdentityManagerProtocol::IdentityManagerDebug(msg) = m {
                            assert!(msg.eq("Unable to assign service identity for service ns1_srv1. Identities pool seems to be empty!"),
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
            (im(vec![]), watcher_rx, identity_manager_tx, identity_creator_rx, _deployment_watched_rx, counters) => {
                assert_message!(m :: IdentityManagerProtocol::IdentityManagerInitialized in watcher_rx);
                assert_message!(m :: IdentityManagerProtocol::IdentityManagerStarted in watcher_rx);
                assert_message!(m :: IdentityCreatorProtocol::StartService in identity_creator_rx);
                // Request a new ServiceIdentity and give it time to process it
                let tx = identity_manager_tx.clone();

                // Push 2 fresh ServiceUser
                tx.send(IdentityManagerProtocol::FoundServiceUser(service_user!(1), false))
                    .await.expect("Unable to send message!");
                tx.send(IdentityManagerProtocol::FoundServiceUser(service_user!(2), false))
                    .await.expect("Unable to send message!");

                // Check they were registered
                assert_message! {
                    (m :: IdentityManagerProtocol::IdentityManagerDebug(_) in watcher_rx) => {
                     if let IdentityManagerProtocol::IdentityManagerDebug(msg) = m {
                            assert!(msg.eq("Found deactivated ServiceUser with name service_user1"),
                                    "Wrong message, got {}", msg);
                        }
                    }
                }
                assert_message! {
                    (m :: IdentityManagerProtocol::IdentityManagerDebug(_) in watcher_rx) => {
                     if let IdentityManagerProtocol::IdentityManagerDebug(msg) = m {
                            assert!(msg.eq("Found deactivated ServiceUser with name service_user2"),
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
                            assert!(msg.eq("New ServiceIdentity requested for ServiceCandidate ns1_srv1"),
                                    "Wrong message, got {}", msg);
                        }
                    }
                }
                assert_message! {
                    (m :: IdentityManagerProtocol::IdentityManagerDebug(_) in watcher_rx) => {
                     if let IdentityManagerProtocol::IdentityManagerDebug(msg) = m {
                            assert!(msg.eq("ServiceIdentity created for service ns1_srv1"),
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
                    (m :: IdentityCreatorProtocol::ActivateServiceUser {..} in identity_creator_rx) => {
                        if let IdentityCreatorProtocol::ActivateServiceUser(service_user, service_ns, service_name, labels) = m {
                            assert_eq!(service_user.name, "service_user1");
                            assert_eq!(service_ns, "ns1");
                            assert_eq!(service_name, "srv1");
                            assert_eq!(labels, HashMap::from([
                                ("namespace".to_string(), "ns1".to_string()),
                                ("name".to_string(), "srv1".to_string())
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
                            assert!(msg.eq("New ServiceIdentity requested for ServiceCandidate ns2_srv2"),
                                    "Wrong message, got {}", msg);
                        }
                    }
                }
                assert_message! {
                    (m :: IdentityManagerProtocol::IdentityManagerDebug(_) in watcher_rx) => {
                     if let IdentityManagerProtocol::IdentityManagerDebug(msg) = m {
                            assert!(msg.eq("ServiceIdentity created for service ns2_srv2"),
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
                    (m :: IdentityCreatorProtocol::ActivateServiceUser {..} in identity_creator_rx) => {
                        if let IdentityCreatorProtocol::ActivateServiceUser(service_user, ns, name, labels) = m {
                            assert!(service_user.name == "service_user2");
                            assert!(ns == "ns2");
                            assert!(name == "srv2");
                            assert!(labels == HashMap::from([
                                ("namespace".to_string(), "ns2".to_string()),
                                ("name".to_string(), "srv2".to_string())
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
                            assert!(msg.eq("New ServiceIdentity requested for ServiceCandidate ns3_srv1"),
                                    "Wrong message, got {}", msg);
                        }
                    }
                }
                assert_message! {
                    (m :: IdentityManagerProtocol::IdentityManagerDebug(_) in watcher_rx) => {
                     if let IdentityManagerProtocol::IdentityManagerDebug(msg) = m {
                            assert!(msg.eq("Unable to assign service identity for service ns3_srv1. Identities pool seems to be empty!"),
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
                            assert!(msg.eq("New ServiceIdentity requested for ServiceCandidate ns1_srv1"),
                                    "Wrong message, got {}", msg);
                        }
                    }
                }
                assert_message! {
                    (m :: IdentityManagerProtocol::IdentityManagerDebug(_) in watcher_rx) => {
                     if let IdentityManagerProtocol::IdentityManagerDebug(msg) = m {
                            assert!(msg.eq("ServiceIdentity created for service ns1_srv1"),
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
                    (m :: IdentityCreatorProtocol::ActivateServiceUser {..} in identity_creator_rx) => {
                        if let IdentityCreatorProtocol::ActivateServiceUser(service_user, service_ns, service_name, labels) = m {
                            assert!(service_user.name == "service_user1");
                            assert_eq!(service_ns, "ns1");
                            assert_eq!(service_name, "srv1");
                            assert!(labels == HashMap::from([
                                ("namespace".to_string(),
                                 "ns1".to_string()),
                                ("name".to_string(), "srv1".to_string())
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

                // Push 11 fresh ServiceUser
                for i in 1..12 {
                    tx.send(IdentityManagerProtocol::FoundServiceUser(service_user!(i), false))
                    .await.expect("Unable to send message!");
                    assert_message! {
                        (m :: IdentityManagerProtocol::IdentityManagerDebug(_) in watcher_rx) => {
                        let expected_msg = format!("Found deactivated ServiceUser with name service_user{}", i);
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
                            assert!(msg.eq("New ServiceIdentity requested for ServiceCandidate ns1_srv1"),
                                    "Wrong message, got {}", msg);
                        }
                    }
                }
                assert_message! {
                    (m :: IdentityManagerProtocol::IdentityManagerDebug(_) in watcher_rx) => {
                        if let IdentityManagerProtocol::IdentityManagerDebug(msg) = m {
                            assert!(msg.eq("ServiceIdentity created for service ns1_srv1"),
                                    "Wrong message, got {}", msg);
                        }
                    }
                }
                // Create it in k8s
                assert_eq!(counters.lock().unwrap().create_calls, 1);

                // Note that now we dont ask IC to create a new crendential, since the pool has enough credentials
                // We add labels and we activate new credential
                assert_message! {
                    (m :: IdentityCreatorProtocol::ActivateServiceUser {..} in identity_creator_rx) => {
                        if let IdentityCreatorProtocol::ActivateServiceUser(service_user, service_ns, service_name, labels) = m {
                            assert_eq!(service_user.name, "service_user1");
                            assert_eq!(service_ns, "ns1");
                            assert_eq!(service_name, "srv1");
                            assert!(labels == HashMap::from([
                                ("namespace".to_string(), "ns1".to_string()),
                                ("name".to_string(), "srv1".to_string())
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
            (im(vec![]), watcher_rx, identity_manager_tx, identity_creator_rx, _deployment_watched_rx, _counters) => {
                assert_message!(m :: IdentityManagerProtocol::IdentityManagerInitialized in watcher_rx);
                assert_message!(m :: IdentityManagerProtocol::IdentityManagerStarted in watcher_rx);
                assert_message!(m :: IdentityCreatorProtocol::StartService in identity_creator_rx);
                // Request a new ServiceIdentity and give it time to process it
                let tx = identity_manager_tx.clone();

                // Push 11 activated ServiceUser, they should not be added to the pool
                for i in 1..12 {
                    tx.send(IdentityManagerProtocol::FoundServiceUser(service_user!(i), true))
                        .await.expect("Unable to send message!");
                    assert_message! {
                        (m :: IdentityManagerProtocol::IdentityManagerDebug(_) in watcher_rx) => {
                        let expected_msg = format!("Found activated ServiceUser with name service_user{}", i);
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
                            assert!(msg.eq("New ServiceIdentity requested for ServiceCandidate ns1_srv1"),
                                    "Wrong message, got {}", msg);
                        }
                    }
                }
                assert_message! {
                    (m :: IdentityManagerProtocol::IdentityManagerDebug(_) in watcher_rx) => {
                     if let IdentityManagerProtocol::IdentityManagerDebug(msg) = m {
                            assert!(msg.eq("Unable to assign service identity for service ns1_srv1. Identities pool seems to be empty!"),
                                    "Wrong message, got {}", msg);
                        }
                    }
                }
                assert_no_message!(identity_creator_rx);

                // Push a fresh credential now
                tx.send(IdentityManagerProtocol::FoundServiceUser(service_user!(13), false))
                    .await.expect("Unable to send message!");
                // Request a new ServiceIdentity
                tx.send(IdentityManagerProtocol::RequestServiceIdentity {
                    service_candidate: deployment!("ns1", "srv1"),
                }).await.expect("Unable to send RequestServiceIdentity message to IdentityManager");
                // We ask IC to create a new crendential
                assert_message!(m :: IdentityCreatorProtocol::CreateIdentity in identity_creator_rx);
                assert_message!(m :: IdentityCreatorProtocol::ActivateServiceUser{..} in identity_creator_rx);
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
                    tx.send(IdentityManagerProtocol::FoundServiceUser(service_user!(i), true))
                        .await.expect("Unable to send message!");
                    assert_message! {
                        (m :: IdentityManagerProtocol::IdentityManagerDebug(_) in watcher_rx) => {
                        let expected_msg = format!("Found activated ServiceUser with name service_user{}", i);
                         if let IdentityManagerProtocol::IdentityManagerDebug(msg) = m {
                                assert_eq!(msg, expected_msg.as_str());
                            }
                        }
                    }
                }
                // Add one deactivated
                tx.send(IdentityManagerProtocol::FoundServiceUser(service_user!(13), false))
                    .await.expect("Unable to send message!");
                // Notify that IdentityCreator is ready
                tx.send(IdentityManagerProtocol::IdentityCreatorReady).await.expect("Unable to send message!");
                // Notify the DeploymentWatcher is ready
                tx.send(IdentityManagerProtocol::DeploymentWatcherReady).await.expect("Unable to send message!");
                let mut extra_credentials_expected: HashSet<String> = HashSet::from_iter((1 .. 12).map(|i| format!("service_user{}", i)).collect::<Vec<_>>());
                for _ in 1 .. 12 {
                    assert_message!(m :: IdentityCreatorProtocol::DeleteSDPUser(_) in identity_creator_rx);
                    if let IdentityCreatorProtocol::DeleteSDPUser(service_user_name) = m {
                        if extra_credentials_expected.contains(&service_user_name) {
                            extra_credentials_expected.remove(&service_user_name);
                        } else {
                            assert!(false, "Deleted extra SDPUser with id {}", service_user_name);
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
        // In this test we have 4 IdentityService but no ServiceCandidate at all
        // This will remove those IdentityService and the ServiceUser instances associated
        test_identity_manager! {
            (watcher_rx, identity_manager_tx, identity_creator_rx, _deployment_watched_rx, counters) => {
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

                // Check first that IM unregistered the ServiceIdentity instances
                let mut extra_service_identities: HashSet<String> = HashSet::from_iter((1 .. 5)
                    .map(|i| format!("ServiceIdentity with id ns{}_srv{} unregistered", i, i))
                    .collect::<Vec<_>>());
                for i in 1 .. 5 {
                    assert_message!(m :: IdentityManagerProtocol::IdentityManagerDebug(_) in watcher_rx);
                    if let IdentityManagerProtocol::IdentityManagerDebug(msg) = m {
                        if extra_service_identities.contains(&msg) {
                            extra_service_identities.remove(&msg);
                        } else {
                            assert!(false, "Unregistered extra ServiceIdentity: {} - {}", msg, i);
                        }
                    } else {
                        assert!(false, "Got wrong message!");
                    }
                }
                assert_no_message!(watcher_rx);
                assert!(extra_service_identities.is_empty(),
                "There were ServiceIdentities that should be removed but they weren't: {:?}", extra_service_identities);
                assert_eq!(counters.lock().unwrap().delete_calls, 4);

                assert_message!(m :: IdentityCreatorProtocol::StartService in identity_creator_rx);
                // Check now that we asked IC to delete the ServiceUser
                let mut extra_service_identities: HashMap<String, u32> = HashMap::from_iter((1 .. 5)
                    .map(|i| (format!("service_user{}", i), i))
                    .collect::<Vec<(String, u32)>>());
                for i in 1 .. 5 {
                    assert_message!(m :: IdentityCreatorProtocol::DeleteServiceUser(_, _, _) in identity_creator_rx);
                    if let IdentityCreatorProtocol::DeleteServiceUser(service_user, service_ns, service_name) = m {
                        if let Some(i) = extra_service_identities.get(&service_user.name) {
                            assert_eq!(service_user.name, format!("service_user{}", i));
                            assert_eq!(service_ns, format!("ns{}", i));
                            assert_eq!(service_name, format!("srv{}", i));
                            extra_service_identities.remove(&service_user.name);
                        } else {
                            assert!(false, "Deleted extra ServiceUSer: {} - {}", service_user.name, i);
                        }
                    } else {
                        assert!(false, "Got wrong message!");
                    }
                }
                assert_eq!(extra_service_identities.len(), 0);
            }
        }
    }
}
