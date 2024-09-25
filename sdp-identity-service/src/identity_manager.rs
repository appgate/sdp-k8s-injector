use async_trait::async_trait;
use kube::api::{DeleteParams, ListParams, PostParams};
use kube::Api;
use sdp_common::constants::SDP_CLUSTER_ID_ENV;
pub use sdp_common::crd::{ServiceIdentity, ServiceIdentitySpec};
use sdp_common::errors::SDPServiceError;
use sdp_common::service::{ServiceCandidate, ServiceLookup, ServiceUser};
use sdp_common::traits::{
    HasCredentials, Labeled, MaybeNamespaced, MaybeService, Named, Namespaced, Service,
};
use sdp_macros::{
    logger, queue_info, sdp_error, sdp_info, sdp_log, sdp_warn, when_ok, with_dollar_sign,
};
use std::collections::hash_map::Entry;
use std::collections::{HashMap, HashSet, VecDeque};
use std::iter::FromIterator;
use tokio::sync::broadcast::Sender as BroadcastSender;
use tokio::sync::mpsc::{Receiver, Sender};
use uuid::Uuid;

use crate::errors::IdentityServiceError;
use crate::identity_creator::IdentityCreatorProtocol;
use crate::service_candidate_watcher::ServiceCandidateWatcherProtocol;

logger!("IdentityManager");

const N_WATCHERS: u8 = 2;

/// Trait that represents the pool of ServiceUser entities
/// We can pop and push ServiceUser entities
trait ServiceUserPool {
    fn pop(&mut self) -> Option<ServiceUser>;
    fn push(&mut self, user_credentials_ref: ServiceUser) -> ();
    fn needs_new_credentials(&self) -> bool;
}

trait ServiceCredentialProvider: ServiceUserPool {
    type From: MaybeService + Labeled + Send;
    type To: Service + HasCredentials + Send;
    fn register_or_update_identity(&mut self, to: Self::To) -> ();
    fn unregister_identity(&mut self, to: &Self::To) -> Option<Self::To>;
    // true if identity is newly created, false if identity is already registered
    fn next_identity(&mut self, from: &Self::From) -> Option<(Self::To, bool)>;
    fn identity(&mut self, from: &Self::From) -> Option<Entry<String, Self::To>>;
    fn identities(&self) -> Vec<&Self::To>;

    /// ServiceIdentity instances without known ServiceCandidate
    fn extra_service_identities<'a>(
        &self,
        current_service_candidates: &HashSet<String>,
    ) -> Vec<&Self::To> {
        self.identities()
            .iter()
            .filter(|id| !current_service_candidates.contains(&id.service_id()))
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
                .map(|&i| i.credentials().id.clone()),
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
            .filter(|i| !activated_service_users.contains(&i.credentials().id))
            .map(|i| *i)
            .collect()
    }
}

/// This should not be needed once the GAT support is in stable
/// https://blog.rust-lang.org/2021/08/03/GATs-stabilization-push.html
#[async_trait]
trait ServiceIdentityAPI<'a> {
    async fn create(
        &self,
        identity: &'a ServiceIdentity,
    ) -> Result<ServiceIdentity, IdentityServiceError>;
    async fn update(
        &self,
        identity: ServiceIdentity,
    ) -> Result<ServiceIdentity, IdentityServiceError>;
    async fn delete(&self, identity: &'a ServiceIdentity) -> Result<(), IdentityServiceError>;
    async fn list(&self) -> Result<Vec<ServiceIdentity>, IdentityServiceError>;
}

/*
 * Protocol implemented to manage identities
 * IdentityManager is the Receiver and it has several Senders: IdentityManager, IdentityCreator and each of
 *  the ServiceCandidate Watchers
 */
#[derive(Debug, Clone)]
pub enum IdentityManagerProtocol<From, To>
where
    From: MaybeService + Send + Sync,
    To: Service + HasCredentials + Send + Sync,
{
    /// Message used to request a new ServiceIdentity for ServiceCandidate
    RequestServiceIdentity(From),
    DeleteServiceIdentity(To),
    FoundServiceCandidate(From),
    DeletedServiceCandidate(From),
    /// Message to notify that a new ServiceUser have been created
    /// IdentityCreator creates these ServiceUSer§
    FoundServiceUser(ServiceUser, bool),
    IdentityCreatorReady,
    DeploymentWatcherReady,
    IdentityManagerInitialized,
    IdentityManagerStarted,
    IdentityManagerReady,
    IdentityManagerDebug(String),
    // service_user, service_ns, service_name
    ActivatedServiceUser(ServiceUser, String, String),
    ReleaseDeviceId(ServiceLookup, Uuid),
}

pub enum IdentityMessageResponse {
    /// Message used to send a new user
    NewIdentity(ServiceUser),
    IdentityUnavailable,
}

#[derive(Default)]
pub struct IdentityManagerServiceCredentialsProvider {
    pool: VecDeque<ServiceUser>,
    services: HashMap<String, ServiceIdentity>,
}

impl ServiceUserPool for IdentityManagerServiceCredentialsProvider {
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

impl ServiceCredentialProvider for IdentityManagerServiceCredentialsProvider {
    type From = ServiceLookup;
    type To = ServiceIdentity;

    fn register_or_update_identity(&mut self, to: Self::To) -> () {
        *self.services.entry(to.service_id()).or_insert(to) = to.clone();
    }

    fn unregister_identity(&mut self, to: &Self::To) -> Option<Self::To> {
        self.services.remove(&to.service_id())
    }

    fn next_identity(&mut self, from: &Self::From) -> Option<(Self::To, bool)> {
        when_ok!((service_id: (Self::To, bool) = from.service_id()) {
            let service_name = from.service_name().unwrap(); // Safe since service_name is defined
            match self.services.get(&service_id) {
                Some(id) => {
                    Some((id.clone(), false))
                }
                None => {
                    if let Some(id) = self.pop().map(|service_user| {
                        let service_identity_spec = ServiceIdentitySpec {
                            service_name: Named::name(from),
                            service_namespace: MaybeNamespaced::namespace(from).unwrap(), // Safe since if it has service_name it has namespace
                            service_user,
                            labels: Labeled::labels(from).unwrap(), // Safe since if it has service_name it has labels
                            disabled: false,
                        };
                        ServiceIdentity::new(&service_name, service_identity_spec)
                    }) {
                        info!(
                            "[{}] ServiceCandidate {} has no associated ServiceIdentities. Registering.",
                            service_id, service_id
                        );
                        self.register_or_update_identity(id.clone());
                        Some((id, true))
                    } else {
                        error!("[{}] Unable to get a new identity for service candidate {}, is the identities pool empty?", service_id, service_id);
                        None
                    }
                }
            }
        })
    }

    fn identity(&mut self, from: &Self::From) -> Option<Entry<String, Self::To>> {
        when_ok!((service_id: Entry<String, ServiceIdentity> = from.service_id()) {
            Some(self.services.entry(service_id))
        })
    }

    fn identities(&self) -> Vec<&Self::To> {
        self.services.values().collect()
    }
}

pub struct IdentityManagerServiceIdentityAPI {
    api: Api<ServiceIdentity>,
    identity_creator_queue: IdentityCreatorProtocolSender,
}

impl IdentityManagerServiceIdentityAPI {
    pub fn new(
        api: Api<ServiceIdentity>,
        identity_creator_queue: IdentityCreatorProtocolSender,
    ) -> Self {
        IdentityManagerServiceIdentityAPI {
            api,
            identity_creator_queue,
        }
    }
}

#[async_trait]
impl<'a> ServiceIdentityAPI<'a> for IdentityManagerServiceIdentityAPI {
    async fn create(
        &self,
        identity: &'a ServiceIdentity,
    ) -> Result<ServiceIdentity, IdentityServiceError> {
        let service_id = identity.service_id();
        info!("[{}] Creating ServiceIdentity", &identity.service_name());
        match self.api.get_opt(&service_id).await? {
            None => {
                let service_identity = self.api.create(&PostParams::default(), identity).await?;
                Ok(service_identity)
            }
            Some(service_identity) => {
                warn!(
                    "[{}] ServiceIdentity {} already exists.",
                    service_id, service_id
                );
                Ok(service_identity)
            }
        }
    }

    async fn update(
        &self,
        identity: ServiceIdentity,
    ) -> Result<ServiceIdentity, IdentityServiceError> {
        match self.api.get_opt(&identity.service_name()).await? {
            Some(mut obj) => {
                obj.spec = identity.spec.clone();
                let new_obj = self
                    .api
                    .replace(&identity.service_name(), &PostParams::default(), &obj)
                    .await?;
                Ok(new_obj)
            }
            None => Ok(self.create(&identity).await?),
        }
    }

    async fn delete(&self, identity: &'a ServiceIdentity) -> Result<(), IdentityServiceError> {
        let _ = self
            .api
            .delete(&identity.service_name(), &DeleteParams::default())
            .await?;
        // Ask IdentityCreator to remove the IdentityCredential
        info!(
            "[{}] Asking for deletion of IdentityCredential {} from SDP system",
            identity.service_id(),
            identity.credentials().name
        );
        let _ = self
            .identity_creator_queue
            .send(IdentityCreatorProtocol::DeleteServiceUser(
                identity.credentials().clone(),
                identity.namespace(),
                identity.name(),
            ))
            .await
            .map_err(|error| IdentityServiceError::from(error.to_string()));
        Ok(())
    }

    async fn list(&self) -> Result<Vec<ServiceIdentity>, IdentityServiceError> {
        self.api
            .list(&ListParams::default())
            .await
            .map(|xs| xs.items)
            .map_err(IdentityServiceError::from)
    }
}

type IdentityCreatorProtocolSender = Sender<IdentityCreatorProtocol>;
type IdentityManagerProtocolReceiver =
    Receiver<IdentityManagerProtocol<ServiceCandidate, ServiceIdentity>>;
type IdentityManagerProtocolSender =
    Sender<IdentityManagerProtocol<ServiceCandidate, ServiceIdentity>>;
type ServiceCandidateWatcherProtocolSender = BroadcastSender<ServiceCandidateWatcherProtocol>;

pub struct IdentityManager<'a> {
    identity_creator_queue: IdentityCreatorProtocolSender,
    identity_manager_rx: IdentityManagerProtocolReceiver,
    identity_manager_tx: IdentityManagerProtocolSender,
    cluster_id: String,
    service_identity_api: IdentityManagerServiceIdentityAPI,
    service_credentials_provider: IdentityManagerServiceCredentialsProvider,
    service_candidate_watcher_proto_tx: ServiceCandidateWatcherProtocolSender,
    external_queue_tx: Option<&'a IdentityManagerProtocolSender>,
    existing_service_candidates: HashSet<String>,
    missing_service_candidates: HashMap<String, ServiceCandidate>,
    existing_activated_credentials: HashSet<String>,
    existing_deactivated_credentials: HashSet<String>,
    identity_creator_ready: bool,
    deployment_watchers_ready: u8,
}

impl<'a> IdentityManager<'a> {
    pub fn new(
        service_identity_api: IdentityManagerServiceIdentityAPI,
        identity_manager_rx: IdentityManagerProtocolReceiver,
        identity_manager_tx: IdentityManagerProtocolSender,
        service_candidate_watcher_proto_tx: ServiceCandidateWatcherProtocolSender,
        identity_creator_queue: IdentityCreatorProtocolSender,
        external_queue_tx: Option<&'a IdentityManagerProtocolSender>,
    ) -> Self {
        let cluster_id = std::env::var(SDP_CLUSTER_ID_ENV);
        if cluster_id.is_err() {
            panic!("Unable to get cluster ID from SDP_CLUSTER_ID environment variable");
        }
        IdentityManager {
            service_identity_api,
            identity_creator_queue,
            service_candidate_watcher_proto_tx,
            identity_manager_rx,
            external_queue_tx,
            identity_manager_tx,
            cluster_id: cluster_id.unwrap(),
            service_credentials_provider: IdentityManagerServiceCredentialsProvider::default(),
            existing_service_candidates: HashSet::default(),
            existing_activated_credentials: HashSet::default(),
            missing_service_candidates: HashMap::default(),
            existing_deactivated_credentials: HashSet::default(),
            identity_creator_ready: false,
            deployment_watchers_ready: 0,
        }
    }
}

#[async_trait]
impl<'a> ServiceIdentityAPI<'a> for IdentityManager<'a> {
    async fn create(
        &self,
        identity: &'a ServiceIdentity,
    ) -> Result<ServiceIdentity, IdentityServiceError> {
        self.service_identity_api.create(identity).await
    }

    async fn update(
        &self,
        identity: ServiceIdentity,
    ) -> Result<ServiceIdentity, IdentityServiceError> {
        self.service_identity_api.update(identity).await
    }

    async fn delete(&self, identity: &'a ServiceIdentity) -> Result<(), IdentityServiceError> {
        self.service_identity_api.delete(identity).await
    }

    async fn list(&self) -> Result<Vec<ServiceIdentity>, IdentityServiceError> {
        self.service_identity_api.list().await
    }
}

#[async_trait]
pub trait IdentityManagerRunner<From, To>: IdentityManagerService<From, To>
where
    From: MaybeService + Send + Sync,
    To: Service + HasCredentials + Send + Sync,
{
    async fn run(&mut self) {
        info!("Starting Identity Manager service");
        let _ = self.initialize().await;

        while let Some(msg) = self.get_message().await {
            match msg {
                IdentityManagerProtocol::DeleteServiceIdentity(service_identity) => {
                    if let Err(err) = self.delete_service_identity(service_identity).await {
                        error!("IdentityManagerProtocol message error: {}", err);
                    }
                }
                IdentityManagerProtocol::RequestServiceIdentity(service_lookup) => {
                    if let Err(err) = self.request_service_identity(service_lookup).await {
                        error!("RequestServiceIdentity message error: {}", err);
                    }
                }
                IdentityManagerProtocol::FoundServiceCandidate(service_lookup) => {
                    if let Err(err) = self.found_service_candidate(service_lookup).await {
                        error!("FoundServiceCandidate message error: {}", err);
                    }
                }
                IdentityManagerProtocol::FoundServiceUser(service_user, activated) => {
                    if let Err(err) = self.found_service_user(service_user, activated).await {
                        error!("FoundServiceUser message error: {}", err);
                    }
                }
                IdentityManagerProtocol::ActivatedServiceUser(
                    service_user,
                    service_ns,
                    service_name,
                ) => {
                    if let Err(err) = self
                        .activated_service_user(service_user, service_ns, service_name)
                        .await
                    {
                        error!("ActivatedServiceUser message error: {}", err);
                    }
                }
                IdentityManagerProtocol::IdentityCreatorReady => {
                    if let Err(err) = self.identity_creator_ready().await {
                        error!("IdentityCreatorReady message error: {}", err);
                    }
                }
                IdentityManagerProtocol::DeploymentWatcherReady => {
                    if let Err(err) = self.deployment_watcher_ready().await {
                        error!("DeploymentWatcherReady message error: {}", err);
                    }
                }
                IdentityManagerProtocol::IdentityManagerReady => {
                    if let Err(err) = self.identity_manager_ready().await {
                        error!("IdentityManagerReady message error: {}", err);
                    }
                }
                IdentityManagerProtocol::DeletedServiceCandidate(service_lookup) => {
                    if let Err(err) = self.deleted_service_candidate(service_lookup).await {
                        error!("DeletedServiceCandidate message error: {}", err);
                    }
                }
                IdentityManagerProtocol::ReleaseDeviceId(service_lookup, uuid) => {
                    if let Err(err) = self.release_device_id(service_lookup, uuid).await {
                        error!("ReleaseDeviceId message error: {}", err);
                    }
                }
                _ => {}
            }
        }
    }
}

#[async_trait]
pub trait IdentityManagerService<From, To>
where
    From: MaybeService + Send + Sync,
    To: Service + HasCredentials + Send + Sync,
{
    async fn delete_service_identity(
        &mut self,
        service_identity: To,
    ) -> Result<(), IdentityServiceError>;

    async fn request_service_identity(
        &mut self,
        service_lookup: From,
    ) -> Result<(), IdentityServiceError>;

    async fn found_service_candidate(
        &mut self,
        service_lookup: From,
    ) -> Result<(), IdentityServiceError>;

    async fn found_service_user(
        &mut self,
        service_user: ServiceUser,
        activated: bool,
    ) -> Result<(), IdentityServiceError>;

    async fn activated_service_user(
        &mut self,
        service_user: ServiceUser,
        service_ns: String,
        service_nane: String,
    ) -> Result<(), IdentityServiceError>;

    async fn identity_creator_ready(&mut self) -> Result<(), IdentityServiceError>;

    async fn deployment_watcher_ready(&mut self) -> Result<(), IdentityServiceError>;

    async fn identity_manager_ready(&mut self) -> Result<(), IdentityServiceError>;

    async fn deleted_service_candidate(
        &mut self,
        service_lookup: From,
    ) -> Result<(), IdentityServiceError>;

    async fn release_device_id(
        &mut self,
        service_lookup: ServiceLookup,
        uuid: Uuid,
    ) -> Result<(), IdentityServiceError>;

    async fn initialize(&mut self) -> Result<(), IdentityServiceError>;

    async fn get_message(&mut self) -> Option<IdentityManagerProtocol<From, To>>;
}

#[async_trait]
impl<'a> IdentityManagerService<ServiceCandidate, ServiceIdentity> for IdentityManager<'a> {
    async fn get_message(
        &mut self,
    ) -> Option<IdentityManagerProtocol<ServiceCandidate, ServiceIdentity>> {
        self.identity_manager_rx.recv().await
    }

    async fn initialize(&mut self) -> Result<(), IdentityServiceError> {
        info!("Initializing Identity Manager service");
        match self.list().await {
            Ok(xs) => {
                info!("Restoring previous Service Identity instances");
                for x in xs {
                    info!("[{}] Restoring Service Identity", x.service_name());
                    self.service_credentials_provider
                        .register_or_update_identity(x.clone());
                }
            }
            Err(err) => {
                panic!("Error fetching list of current ServiceIdentity: {}", err);
            }
        }

        queue_info! {
            IdentityManagerProtocol::<ServiceCandidate, ServiceIdentity>::IdentityManagerInitialized => self.external_queue_tx
        };

        // Ask Identity Creator to awake
        if let Err(err) = self
            .identity_creator_queue
            .send(IdentityCreatorProtocol::StartService)
            .await
        {
            error!(
                "Error sending StartService message to Identity Creator: {}",
                err
            );
            panic!();
        }
        Ok(())
    }

    async fn delete_service_identity(
        &mut self,
        service_identity: ServiceIdentity,
    ) -> Result<(), IdentityServiceError> {
        let service_id = service_identity.service_id();
        let _service_name = service_identity.service_name();
        info!(
            "[{}] Deleting ServiceIdentity with id {}",
            service_id, service_id
        );
        if let Err(err) = self.delete(&service_identity).await {
            error!(IdentityManagerProtocol::<ServiceCandidate, ServiceIdentity>::IdentityManagerDebug |(
                "[{}] Error deleting ServiceIdentity for service {}: {}",
                service_id, service_id,
                err
            ) => self.external_queue_tx);
        } else {
            info!(
                "[{}] Unregistering ServiceIdentity with id {}",
                service_id, service_id
            );

            // Unregister the identity
            if let Some(s) = self
                .service_credentials_provider
                .unregister_identity(&service_identity)
            {
                info!(IdentityManagerProtocol::<ServiceCandidate, ServiceIdentity>::IdentityManagerDebug |
                    ("[{}] ServiceIdentity with id {} unregistered", s.service_id(), s.service_id()) => self.external_queue_tx);
            } else {
                warn!(IdentityManagerProtocol::<ServiceCandidate, ServiceIdentity>::IdentityManagerDebug |
                    ("[{}] ServiceIdentity with id {} was not registered", service_id, service_id) => self.external_queue_tx);
            }
        }
        Ok(())
    }

    async fn request_service_identity(
        &mut self,
        service_candidate: ServiceCandidate,
    ) -> Result<(), IdentityServiceError> {
        when_ok!((service_id = service_candidate.service_id()) {
            info!(IdentityManagerProtocol::<ServiceCandidate, ServiceIdentity>::IdentityManagerDebug |(
                "[{}] New ServiceIdentity requested for ServiceCandidate {}",
                service_id, service_id
            ) => self.external_queue_tx);
            let a = ServiceLookup::try_from_service(&service_candidate).expect("Unable to convert service");
            match self.service_credentials_provider.next_identity(&a) {
                Some((service_identity, true)) => match self.create(&service_identity).await {
                    Ok(service_identity) => {
                        info!(IdentityManagerProtocol::<ServiceCandidate, ServiceIdentity>::IdentityManagerDebug |(
                            "[{}] ServiceIdentity created for service {}",
                            service_id, service_id
                        ) => self.external_queue_tx);

                        if self.service_credentials_provider.needs_new_credentials() {
                            info!("[{}] Requesting new UserCredentials to add to the pool", service_id);
                            if let Err(err) = self.identity_creator_queue
                                .send(IdentityCreatorProtocol::CreateIdentity)
                                .await
                            {
                                error!(IdentityManagerProtocol::<ServiceCandidate, ServiceIdentity>::IdentityManagerDebug |(
                                    "[{}] Error when sending IdentityCreatorMessage::CreateIdentity: {}",
                                    service_id, err
                                ) => self.external_queue_tx);
                            }
                        }
                        if let Err(err) = self.identity_creator_queue
                            .send(IdentityCreatorProtocol::ActivateServiceUser(
                                service_identity.credentials().clone(),
                                self.cluster_id.clone(),
                                service_identity.namespace(),
                                service_identity.name(),
                                service_candidate.labels().unwrap(),
                            ))
                            .await
                        {
                            error!("[{}] Error activating ServiceUser for service {}: {}",
                            service_identity.service_id(), service_identity.service_id(), err);
                        }
                    }
                    Err(err) => {
                        error!(IdentityManagerProtocol::<ServiceCandidate, ServiceIdentity>::IdentityManagerDebug |(
                            "[{}] Error creating ServiceIdentity for service with id {}: {}",
                            service_id, service_id, err
                    ) => self.external_queue_tx);
                    }
                },
                Some((_, false)) => {
                    info!(IdentityManagerProtocol::<ServiceCandidate, ServiceIdentity>::IdentityManagerDebug |(
                        "[{}] ServiceIdentity already exists for service {}",
                        service_id, service_id
                    ) => self.external_queue_tx);
                }
                None => {
                    error!(IdentityManagerProtocol::<ServiceCandidate, ServiceIdentity>::IdentityManagerDebug |(
                        "[{}] Unable to assign service identity for service {}. Identities pool seems to be empty!",
                        service_id, service_id
                        ) => self.external_queue_tx);
                }
            }
        });
        Ok(())
    }

    async fn found_service_candidate(
        &mut self,
        service_candidate: ServiceCandidate,
    ) -> Result<(), IdentityServiceError> {
        when_ok!((candidate_service_id = service_candidate.service_id()) {
            let service_lookup = ServiceLookup::try_from_service(&service_candidate).unwrap();
            if let Some(Entry::Occupied(entry)) = self.service_credentials_provider.identity(&service_lookup) {
                info!(IdentityManagerProtocol::<ServiceCandidate, ServiceIdentity>::IdentityManagerDebug |(
                    "[{}] Found registered ServiceCandidate {}",
                    entry.get().service_id(), entry.get().service_id()
                ) => self.external_queue_tx);
                self.existing_service_candidates.insert(candidate_service_id.clone());
            } else {
                    info!(IdentityManagerProtocol::<ServiceCandidate, ServiceIdentity>::IdentityManagerDebug |(
                        "[{}] Found unregistered ServiceCandidate {}",
                        candidate_service_id, candidate_service_id
                    ) => self.external_queue_tx);
                    self.missing_service_candidates.insert(candidate_service_id, service_candidate);
                }
        });
        Ok(())
    }

    async fn found_service_user(
        &mut self,
        service_user: ServiceUser,
        activated: bool,
    ) -> Result<(), IdentityServiceError> {
        let user_name = service_user.name.clone();
        if !activated {
            info!(IdentityManagerProtocol::<ServiceCandidate, ServiceIdentity>::IdentityManagerDebug | ("[{} | {}] Found deactivated ServiceUser", user_name, service_user.id) => self.external_queue_tx);
            self.existing_deactivated_credentials
                .insert(service_user.id.clone());
            self.service_credentials_provider.push(service_user);
        } else {
            self.existing_activated_credentials
                .insert(service_user.id.clone());
            info!(IdentityManagerProtocol::<ServiceCandidate, ServiceIdentity>::IdentityManagerDebug |(
                "[{} | {}] Found activated ServiceUser",
                user_name,
                service_user.id
            ) => self.external_queue_tx);
        }
        Ok(())
    }

    async fn activated_service_user(
        &mut self,
        service_user: ServiceUser,
        service_ns: String,
        service_name: String,
    ) -> Result<(), IdentityServiceError> {
        info!(
            "[{}] ServiceUser {} (id: {}) has been activated, updating ServiceIdentity",
            format!("{}_{}", service_ns, service_name),
            &service_user.name,
            &service_user.id
        );
        if let Some(Entry::Occupied(entry)) = self
            .service_credentials_provider
            .identity(&ServiceLookup::new(&service_ns, &service_name, None))
        {
            let mut new_service_identity = entry.get().clone();
            let new_user_name = service_user.name.clone();
            let user_id = service_user.id.clone();
            new_service_identity.spec.service_user = service_user;
            if let Err(e) = self
                .service_identity_api
                .update(new_service_identity.clone())
                .await
            {
                error!("[{}] Error updating ServiceIdentity [{}/{}] after activating ServiceUser {} (id: {}): {}",
                        entry.get().name(), &service_ns, &service_name, &new_user_name, &user_id, e);
            }
            self.service_credentials_provider
                .register_or_update_identity(new_service_identity);
        }
        Ok(())
    }

    async fn identity_creator_ready(&mut self) -> Result<(), IdentityServiceError> {
        info!("IdentityCreator is ready");
        self.identity_creator_ready = true;
        if self.deployment_watchers_ready == N_WATCHERS {
            info!("IdentityManager is ready");
            if let Err(e) = self
                .identity_manager_tx
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
        Ok(())
    }

    async fn deployment_watcher_ready(&mut self) -> Result<(), IdentityServiceError> {
        info!("DeploymentWatcher is ready");
        self.deployment_watchers_ready += 1;
        if self.deployment_watchers_ready == N_WATCHERS && self.identity_creator_ready {
            if let Err(e) = self
                .identity_manager_tx
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
        Ok(())
    }

    async fn identity_manager_ready(&mut self) -> Result<(), IdentityServiceError> {
        let mut removed_service_identities: HashSet<String> = HashSet::new();
        info!("IdentityManager is ready");

        // Here we do a basic cleanup of the current state
        // 1. First we ask for deletion for all the ServiceIdentity instances that don't have a known
        //    ServiceCandidate. This will eventually remove the ServiceUser associated
        // 2. Now we remove all the known active ServiceUser instances that don't belong to any ServiceIdentity
        // 3. Then we delete any ServiceIdentity instance that does not have a ServiceUser activated
        // 4. Finally we unregister all device ids for current services that have unused clients:
        //    Example: ns1_app1_XXXXX is active but we have device ids registered for ns1_app1_YYYYY and
        //             ns1_app1_ZZZZZ. Then ns1_app1_YYYYY and ns1_app1_ZZZZZ will be unregistered.

        info!(IdentityManagerProtocol::<ServiceCandidate, ServiceIdentity>::IdentityManagerDebug |("Syncing ServiceIdentity instances") => self.external_queue_tx);

        // 1. Delete IdentityService instances with unknown ServiceCandidate
        info!("Searching for ServiceIdentities with unknown ServiceCandidate");
        for service_identity in self
            .service_credentials_provider
            .extra_service_identities(&self.existing_service_candidates)
        {
            let service_id = service_identity.service_id();
            info!(
                "[{}] Found ServiceIdentity {} with unknown ServiceCandidate. Deleting it.",
                service_id, service_id
            );
            if let Err(e) = self
                .identity_manager_tx
                .send(IdentityManagerProtocol::DeleteServiceIdentity(
                    service_identity.clone(),
                ))
                .await
            {
                error!(
                    "[{}] Error deleting ServiceIdentity {} with unknown ServiceCandidate: {}",
                    service_id,
                    service_id,
                    e.to_string()
                );
            } else {
                // Make sure we dont try to delete it twice!
                removed_service_identities.insert(service_id.clone());
            }
        }

        // 2. Delete ServiceUser instances that don't belong to any ServiceIdentity
        info!("Searching for orphaned ServiceUsers");
        for sdp_user_name in self
            .service_credentials_provider
            .orphan_service_users(&self.existing_activated_credentials)
        {
            info!(
                "SDPUser {} is active but not used by any ServiceIdentity, deleting it",
                sdp_user_name
            );
            self.identity_creator_queue
                .send(IdentityCreatorProtocol::DeleteSDPUser(sdp_user_name))
                .await
                .expect("Error deleting orphaned SDPUser");
        }

        // 3. Delete IdentityService instances holding not active credentials
        info!("Searching for orphaned ServiceIdentities");
        for service_identity in self
            .service_credentials_provider
            .orphan_service_identities(&self.existing_activated_credentials)
        {
            let service_id = service_identity.service_id();
            if !removed_service_identities.contains(&service_id) {
                info!(
                    "[{}] ServiceIdentity {} has deactivated ServiceUser. Deleting it.",
                    service_id, service_id
                );
                if let Err(e) = self
                    .identity_manager_tx
                    .send(IdentityManagerProtocol::DeleteServiceIdentity(
                        service_identity.clone(),
                    ))
                    .await
                {
                    error!(
                        "[{}] Error requesting deleting of ServiceIdentity {}: {}",
                        service_id,
                        service_id,
                        e.to_string()
                    );
                } else {
                    // Make sure we dont try to delete it twice!
                    removed_service_identities.insert(service_id.clone());
                }
            }
        }

        // Reconcile sdp_user device ids registered for active users that have a ServiceIdentity
        self.identity_creator_queue
            .send(IdentityCreatorProtocol::ReconcileSDPUsers(
                HashSet::from_iter(
                    self.service_credentials_provider
                        .identities()
                        .iter()
                        .map(|s| s.credentials().name.clone()),
                ),
            ))
            .await
            .expect("Error reconciliating SDP users");

        // Request ServiceIdentity for candidates that dont have it
        for (service_candidate_id, service_candidate) in &self.missing_service_candidates {
            info!(
                "[{}] Requesting missing ServiceCandidate {}",
                service_candidate_id, service_candidate_id
            );
            self.identity_manager_tx
                .send(IdentityManagerProtocol::RequestServiceIdentity(
                    service_candidate.clone(),
                ))
                .await
                .expect("Error requesting new ServiceIdentity");
        }

        // Notify the ServiceCandidate watchers that we are ready to process process for new service candidates..
        self.service_candidate_watcher_proto_tx
            .send(ServiceCandidateWatcherProtocol::IdentityManagerReady)
            .expect("Unable to notify DeploymentWatcher!");
        Ok(())
    }

    async fn deleted_service_candidate(
        &mut self,
        service_candidate: ServiceCandidate,
    ) -> Result<(), IdentityServiceError> {
        when_ok!((candidate_service_id = service_candidate.service_id()) {
            let service_lookup = ServiceLookup::try_from_service(&service_candidate).unwrap();
            match self.service_credentials_provider.identity(&service_lookup) {
                Some(Entry::Occupied(entry)) => {
                    if let Err(e) = self.identity_manager_tx
                        .send(IdentityManagerProtocol::DeleteServiceIdentity(
                            entry.get().clone(),
                        ))
                        .await
                    {
                        // TODO: We should retry later
                        error!(
                            "[{}] Unable to queue message to delete ServiceIdentity {}: {}",
                            entry.get().service_id(),
                            entry.get().service_id(),
                            e.to_string()
                        );
                    };
                }
                _ => {
                    error!("[{}] Deleted ServiceCandidate {} has not IdentityService attached, ignoring!",
                        candidate_service_id, candidate_service_id);
                }
            }
        });
        Ok(())
    }

    async fn release_device_id(
        &mut self,
        service_lookup: ServiceLookup,
        uuid: Uuid,
    ) -> Result<(), IdentityServiceError> {
        if let Some(Entry::Occupied(mut entry)) =
            self.service_credentials_provider.identity(&service_lookup)
        {
            let uuid_s = uuid.to_string();
            let service_identity = entry.get_mut();
            if service_identity.spec.service_user.device_ids.is_some()
                && !service_identity
                    .spec
                    .service_user
                    .device_ids
                    .as_ref()
                    .unwrap()
                    .contains(&uuid_s)
            {
                service_identity
                    .spec
                    .service_user
                    .device_ids
                    .as_mut()
                    .unwrap()
                    .push(uuid.to_string());
            }
            self.service_identity_api
                .update(service_identity.clone())
                .await?;
            self.identity_creator_queue
                .send(IdentityCreatorProtocol::ReleaseDeviceId(
                    entry.get_mut().spec.service_user.clone(),
                    uuid,
                ))
                .await
                .map_err(SDPServiceError::from_error(
                    "Error asking IdentityCreator to release device id",
                ))?;
        } else {
            warn!(
                "[{}] Unable to release device id {}, ServiceIdentity not found.",
                service_lookup.name(),
                uuid
            );
        };
        Ok(())
    }
}

impl<'a> IdentityManagerRunner<ServiceCandidate, ServiceIdentity> for IdentityManager<'a> {}

#[cfg(test)]
mod tests {
    use std::{
        collections::{hash_map::Entry, HashMap, HashSet},
        sync::{Arc, Mutex},
        time,
    };

    use async_trait::async_trait;
    use k8s_openapi::api::apps::v1::Deployment;
    use kube::{core::object::HasSpec, ResourceExt};
    pub use sdp_common::crd::{SDPService, SDPServiceSpec};
    use sdp_common::{
        service::ServiceCandidate,
        service::ServiceLookup,
        traits::{HasCredentials, Service},
    };
    use sdp_macros::{deployment, sdp_service, service_identity, service_user};
    use tokio::sync::{broadcast::channel as broadcast_channel, mpsc::Receiver};
    use tokio::{
        sync::{mpsc::channel, RwLock},
        time::sleep,
    };
    use uuid::Uuid;

    use crate::{
        errors::IdentityServiceError,
        identity_manager::{ServiceUser, ServiceUserPool},
    };
    use crate::{
        identity_creator::IdentityCreatorProtocol,
        identity_manager::IdentityManagerProtocol,
        identity_manager::{IdentityManagerServiceCredentialsProvider, ServiceCredentialProvider},
        service_candidate_watcher::ServiceCandidateWatcherProtocol,
    };

    use super::{
        IdentityManagerRunner, IdentityManagerService, ServiceIdentity, ServiceIdentityAPI,
        ServiceIdentitySpec,
    };

    #[derive(Default)]
    struct APICounters {
        delete_calls: Arc<RwLock<usize>>,
        create_calls: Arc<RwLock<usize>>,
        list_calls: Arc<RwLock<usize>>,
        update_calls: Arc<RwLock<usize>>,
    }

    struct TestIdentityManager {
        _api_counters: Arc<Mutex<APICounters>>,
        methods: Arc<Mutex<HashMap<String, usize>>>,
        _queue: Receiver<IdentityManagerProtocol<ServiceCandidate, ServiceIdentity>>,
        messages: Vec<IdentityManagerProtocol<ServiceCandidate, ServiceIdentity>>,
        n_message: usize,
    }

    #[async_trait]
    impl<'a> ServiceIdentityAPI<'a> for APICounters {
        async fn create(
            &self,
            identity: &'a ServiceIdentity,
        ) -> Result<ServiceIdentity, IdentityServiceError> {
            let mut c = self.create_calls.write().await;
            *c += 1;
            Ok(identity.clone())
        }

        async fn delete(&self, _: &'a ServiceIdentity) -> Result<(), IdentityServiceError> {
            let mut c = self.delete_calls.write().await;
            *c += 1;
            Ok(())
        }

        async fn list(&self) -> Result<Vec<ServiceIdentity>, IdentityServiceError> {
            let mut c = self.list_calls.write().await;
            *c += 1;
            Ok(vec![])
        }

        async fn update(
            &self,
            identity: ServiceIdentity,
        ) -> Result<ServiceIdentity, IdentityServiceError> {
            let mut c = self.update_calls.write().await;
            *c += 1;
            Ok(identity.clone())
        }
    }

    impl TestIdentityManager {
        fn new(
            messages: Vec<IdentityManagerProtocol<ServiceCandidate, ServiceIdentity>>,
            queue: Receiver<IdentityManagerProtocol<ServiceCandidate, ServiceIdentity>>,
        ) -> Self {
            TestIdentityManager {
                _api_counters: Arc::new(Mutex::new(APICounters::default())),
                methods: Arc::new(Mutex::new(HashMap::default())),
                _queue: queue,
                messages,
                n_message: 0,
            }
        }
        fn _reset_counters(&self) -> () {
            let mut api_counters = self._api_counters.lock().unwrap();
            api_counters.delete_calls = Arc::new(RwLock::new(0));
            api_counters.create_calls = Arc::new(RwLock::new(0));
            api_counters.list_calls = Arc::new(RwLock::new(0));
            api_counters.update_calls = Arc::new(RwLock::new(0));
        }

        async fn inc_method(&mut self, method: &str) -> () {
            self.methods
                .lock()
                .unwrap()
                .entry(method.to_string())
                .or_insert(0);
            self.methods
                .lock()
                .unwrap()
                .entry(method.to_string())
                .and_modify(|v| *v += 1);
        }
    }

    #[async_trait]
    impl IdentityManagerService<ServiceCandidate, ServiceIdentity> for TestIdentityManager {
        async fn delete_service_identity(
            &mut self,
            _service_identity: ServiceIdentity,
        ) -> Result<(), IdentityServiceError> {
            Ok(self.inc_method("delete_service_identity").await)
        }

        async fn request_service_identity(
            &mut self,
            _service_lookup: ServiceCandidate,
        ) -> Result<(), IdentityServiceError> {
            Ok(self.inc_method("request_service_identity").await)
        }

        async fn found_service_candidate(
            &mut self,
            _service_lookup: ServiceCandidate,
        ) -> Result<(), IdentityServiceError> {
            Ok(self.inc_method("found_service_candidate").await)
        }

        async fn found_service_user(
            &mut self,
            _service_user: ServiceUser,
            _activated: bool,
        ) -> Result<(), IdentityServiceError> {
            Ok(self.inc_method("found_service_user").await)
        }

        async fn activated_service_user(
            &mut self,
            _service_user: ServiceUser,
            _service_ns: String,
            _service_nane: String,
        ) -> Result<(), IdentityServiceError> {
            Ok(self.inc_method("activated_service_user").await)
        }

        async fn identity_creator_ready(&mut self) -> Result<(), IdentityServiceError> {
            Ok(self.inc_method("identity_creator_ready").await)
        }

        async fn deployment_watcher_ready(&mut self) -> Result<(), IdentityServiceError> {
            Ok(self.inc_method("deployment_watcher_ready").await)
        }

        async fn identity_manager_ready(&mut self) -> Result<(), IdentityServiceError> {
            Ok(self.inc_method("identity_manager_ready").await)
        }

        async fn deleted_service_candidate(
            &mut self,
            _service_lookup: ServiceCandidate,
        ) -> Result<(), IdentityServiceError> {
            Ok(self.inc_method("deleted_service_candidate").await)
        }

        async fn release_device_id(
            &mut self,
            _service_lookup: ServiceLookup,
            _uuid: Uuid,
        ) -> Result<(), IdentityServiceError> {
            Ok(self.inc_method("release_device_id").await)
        }

        async fn initialize(&mut self) -> Result<(), IdentityServiceError> {
            Ok(self.inc_method("initialize").await)
        }

        async fn get_message(
            &mut self,
        ) -> Option<IdentityManagerProtocol<ServiceCandidate, ServiceIdentity>> {
            self.inc_method("get_message").await;
            if self.n_message >= self.messages.len() {
                None
            } else {
                self.n_message += 1;
                Some(self.messages[self.n_message - 1].clone())
            }
        }
    }

    impl IdentityManagerRunner<ServiceCandidate, ServiceIdentity> for TestIdentityManager {}

    macro_rules! test_identity_manager {
        (($im:ident($messages:expr), $api_counters:ident, $watcher_rx:ident) => $e:expr) => {
            let (_identity_manager_proto_tx, _identity_manager_proto_rx) =
                channel::<IdentityManagerProtocol<ServiceCandidate, ServiceIdentity>>(10);
            let (_identity_creator_proto_tx, mut _identity_creator_proto_rx) =
                channel::<IdentityCreatorProtocol>(10);
            let (_service_candidate_watcher_proto_tx, mut _service_candidate_watcher_proto_rx) =
                broadcast_channel::<ServiceCandidateWatcherProtocol>(10);
            let (_watcher_tx, $watcher_rx) =
                channel::<IdentityManagerProtocol<ServiceCandidate, ServiceIdentity>>(10);
            let $api_counters = Arc::new(Mutex::new(APICounters::default()));
            let $im = Arc::new(RwLock::new(TestIdentityManager::new(
                $messages,
                $watcher_rx,
            )));
            let im2 = Arc::clone(&$im);
            tokio::spawn(async move {
                im2.write().await.run().await;
            });
            sleep(time::Duration::from_secs(1)).await;
            $e
        };

        (($counters:ident, $watcher_rx:ident) => $e:expr) => {
            test_identity_manager! {
                (im(vec![]), $watcher_rx, $counters) => {
                    $e
               }
            }
        };
    }

    macro_rules! test_identity_manager_service_credentials_provider {
        ($service_credentials_provider:ident($vs:expr) => $e:expr) => {
            let mut $service_credentials_provider =
                IdentityManagerServiceCredentialsProvider::default();
            assert_eq!($service_credentials_provider.identities().len(), 0);
            // register new identities
            for i in $vs.clone() {
                $service_credentials_provider.register_or_update_identity(i);
            }
            $e
        };
        ($service_credentials_provider:ident => $e:expr) => {
            let vs = vec![
                service_identity!(1),
                service_identity!(2),
                service_identity!(3),
                service_identity!(4),
            ];
            test_identity_manager_service_credentials_provider! {
                $service_credentials_provider(vs) => $e
            }
        };
    }

    #[test]
    fn test_identity_manager_service_credentials_provider_identities() {
        let identities = vec![
            service_identity!(1),
            service_identity!(2),
            service_identity!(3),
            service_identity!(4),
        ];
        let d1 = deployment!("ns1", "dep1");
        let d2 = deployment!("ns1", "srv1");
        let ss1 = sdp_service!("ns2", "srv2", "customrunner");
        test_identity_manager_service_credentials_provider! {
            im(identities) => {
                assert_eq!(im.identities().len(), 4);
                let mut is0: Vec<ServiceIdentity> = im.identities().iter().map(|&i| i.clone()).collect();
                let sorted_is0 = is0.sort_by(|a, b| a.spec().service_name.partial_cmp(&b.spec().service_name).unwrap());
                let mut is1: Vec<ServiceIdentity> = identities.iter().map(|i| i.clone()).collect();
                let sorted_is1 = is1.sort_by(|a, b| a.spec().service_name.partial_cmp(&b.spec().service_name).unwrap());
                assert_eq!(sorted_is0, sorted_is1);
                assert!(matches!(
                    im.identity(&ServiceLookup::try_from_service(&d1).unwrap()),
                    Some(Entry::Vacant(_)))
                );
                assert!(matches!(
                    im.identity(&ServiceLookup::try_from_service(&d2).unwrap()),
                    Some(Entry::Occupied(_)))
                );
                assert!(matches!(
                    im.identity(&ServiceLookup::try_from_service(&ss1).unwrap()),
                    Some(Entry::Occupied(_)))
                );
            }
        }
    }

    fn check_service_identity(
        si: Option<(ServiceIdentity, bool)>,
        c: &ServiceUser,
        service_name: &str,
        service_ns: &str,
    ) -> () {
        assert!(si.is_some());
        let si_spec = si.unwrap().0;
        assert_eq!(si_spec.spec().service_name, service_name);
        assert_eq!(si_spec.spec().service_namespace, service_ns);
        assert_eq!(si_spec.spec().service_user.name, c.name);
    }

    #[test]
    fn test_identity_manager_service_credentials_provider_identities_next_identity() {
        let id1 = service_identity!(1);
        let identities = vec![id1.clone()];
        let d1_1 = deployment!("ns1", "srv1");
        let d2_2 = deployment!("ns2", "srv2");
        let d1_2 = deployment!("ns2", "srv1");
        let d3_1 = deployment!("ns1", "srv3");
        let ss3_1 = sdp_service!("ns1", "srv3", "customservice");

        test_identity_manager_service_credentials_provider! {
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

        test_identity_manager_service_credentials_provider! {
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
                assert_eq!(d1_1_id.unwrap().0.spec(), id1.spec());

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
                identities.sort_by(|a, b| a.service_id().partial_cmp(&b.service_id()).unwrap());
                let identities: Vec<String> = identities.iter().map(|i| i.service_id()).collect();
                assert_eq!(identities, vec!["ns1_srv1", "ns2_srv1", "ns2_srv2"]);
            }
        }

        test_identity_manager_service_credentials_provider! {
            im(identities) => {
                let c1 = service_user!(1);
                let c2 = service_user!(2);
                // push some credentials
                im.push(c1.clone());
                im.push(c2.clone());

                assert_eq!(im.identities().len(), 1);

                // ask for a new service identity from candidate SDPService, we can create it because we have creds
                let d2_2_id = im.next_identity(&ServiceLookup::try_from_service(&ss3_1).unwrap());
                check_service_identity(d2_2_id, &c1, "srv3", "ns1");
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
                    spec.service_user.id.as_str(),
                )
            })
            .collect();
        xs.sort();
        xs
    }

    #[test]
    fn test_identity_manager_service_credentials_provider_extras_identities() {
        // extra_service_identities compute the service identities registered in memory
        // that dont have a counterpart in the system (k8s for example)
        test_identity_manager_service_credentials_provider! {
            im => {
                let mut identities = im.identities();
                identities.sort_by(|a, b| a.name_any().as_str().partial_cmp(b.name_any().as_str()).unwrap());

                // 4 service identities registered but none found on start,
                let xs = service_identities_to_tuple(
                    im.extra_service_identities(&HashSet::new()));
                assert_eq!(xs.len(), 4);
                assert_eq!(xs, vec![
                    ("ns1", "srv1", "service_user_id1"),
                    ("ns2", "srv2", "service_user_id2"),
                    ("ns3", "srv3", "service_user_id3"),
                    ("ns4", "srv4", "service_user_id4"),
                ]);

                // 4 service identities registered and only 1 on start
                let xs = service_identities_to_tuple(
                    im.extra_service_identities(&HashSet::from(
                    [identities[0].service_id()]
                )));
                assert_eq!(xs.len(), 3);
                assert_eq!(xs, vec![
                    ("ns2", "srv2", "service_user_id2"),
                    ("ns3", "srv3", "service_user_id3"),
                    ("ns4", "srv4", "service_user_id4"),
                ]);

                // 4 service identities registered and all 4 on start
                let xs: Vec<(&str, &str, &str)> = service_identities_to_tuple(
                    im.extra_service_identities(&HashSet::from(
                        [
                            identities[0].service_id(),
                            identities[1].service_id(),
                            identities[2].service_id(),
                            identities[3].service_id(),
                        ])));
                assert_eq!(xs.len(), 0);
                assert_eq!(xs, vec![]);

                // 5 service identities registered and all 4 on start
                let xs: Vec<(&str, &str, &str)> = service_identities_to_tuple(
                    im.extra_service_identities(&HashSet::from(
                        [
                            identities[0].service_id(),
                            identities[1].service_id(),
                            identities[2].service_id(),
                            identities[3].service_id(),
                            service_identity!(5).service_id(),
                        ])));
                assert_eq!(xs.len(), 0);
                assert_eq!(xs, vec![]);
            }
        }
    }

    #[test]
    fn test_identity_manager_service_credentials_provider_orphan_identities() {
        // orphan_service_identities computes the list of service identities holding
        // ServiceUsers that are not active anymore
        test_identity_manager_service_credentials_provider! {
            im => {
                let mut identities = im.identities();
                identities.sort_by(|a, b| a.name_any().as_str().partial_cmp(b.name_any().as_str()).unwrap());

                // 4 service identities registered none active service users,
                let xs = service_identities_to_tuple(
                    im.orphan_service_identities(&HashSet::new()));
                assert_eq!(xs.len(), 4);
                assert_eq!(xs, vec![
                    ("ns1", "srv1", "service_user_id1"),
                    ("ns2", "srv2", "service_user_id2"),
                    ("ns3", "srv3", "service_user_id3"),
                    ("ns4", "srv4", "service_user_id4"),
                ]);

                // 4 service identities registered 4 active service users,
                let xs = service_identities_to_tuple(
                    im.orphan_service_identities(&HashSet::from([
                        identities[0].credentials().id.clone(),
                        identities[1].credentials().id.clone(),
                        identities[2].credentials().id.clone(),
                        identities[3].credentials().id.clone(),
                    ])));
                assert_eq!(xs.len(), 0);
                assert_eq!(xs, vec![]);

                // 4 service identities registered 2 active service users,
                let xs = service_identities_to_tuple(
                    im.orphan_service_identities(&HashSet::from([
                        identities[1].credentials().id.clone(), // service_user2
                        identities[3].credentials().id.clone(), // service_user4
                    ])));
                assert_eq!(xs.len(), 2);
                assert_eq!(xs, vec![
                    ("ns1", "srv1", "service_user_id1"),
                    ("ns3", "srv3", "service_user_id3"),
                ]);

                // 4 service identities registered 5 active service users,
                let xs = service_identities_to_tuple(
                    im.orphan_service_identities(&HashSet::from([
                        identities[0].credentials().id.clone(),
                        identities[1].credentials().id.clone(),
                        identities[2].credentials().id.clone(),
                        identities[3].credentials().id.clone(),
                        service_identity!(5).credentials().id.clone()
                    ])));
                assert_eq!(xs.len(), 0);
                assert_eq!(xs, vec![]);

            }
        }
    }

    #[test]
    fn test_identity_manager_service_credentials_provider_orphan_service_users() {
        // orphan_service_users computes the list of active service users that are not being
        // used by any service
        test_identity_manager_service_credentials_provider! {
            im => {
                let mut identities = im.identities();
                identities.sort_by(|a, b| a.name_any().as_str().partial_cmp(b.name_any().as_str()).unwrap());

                // 4 service identities registered none active service users,
                let xs = im.orphan_service_users(&HashSet::new());
                assert_eq!(xs.len(), 0);
                assert_eq!(xs, HashSet::<String>::new());

                // 4 service identities registered 4 active service users,
                let xs = im.orphan_service_users(&HashSet::from([
                    "service_user_id1".to_string(),
                    "service_user_id2".to_string(),
                    "service_user_id3".to_string(),
                    "service_user_id4".to_string(),
                ]));
                assert_eq!(xs.len(), 0);
                assert_eq!(xs, HashSet::<String>::new());

                // 4 service identities registered 1 active service user but not used,
                let xs = im.orphan_service_users(&HashSet::from([
                    "service_user_id5".to_string(),
                ]));
                assert_eq!(xs.len(), 1);
                assert_eq!(xs, HashSet::from(["service_user_id5".to_string()]));

                // 4 service identities registered 8 active service users, 4 not being used
                let xs = im.orphan_service_users(&HashSet::from([
                    "service_user_id1".to_string(),
                    "service_user_id2".to_string(),
                    "service_user_id3".to_string(),
                    "service_user_id4".to_string(),
                    "service_user_id5".to_string(),
                    "service_user_id6".to_string(),
                    "service_user_id7".to_string(),
                    "service_user_id8".to_string(),
                ]));
                assert_eq!(xs.len(), 4);
                assert_eq!(xs, HashSet::from([
                    "service_user_id5".to_string(),
                    "service_user_id6".to_string(),
                    "service_user_id7".to_string(),
                    "service_user_id8".to_string(),
                    ]));
            }
        }
    }

    #[tokio::test]
    async fn test_identity_manager_runner() {
        test_identity_manager! {
            (im(vec![]), _api_counters, _watcher) => {
                assert!(im.read().await.methods.lock().unwrap().get("initialize").is_some());
                assert_eq!(im.read().await.methods.lock().unwrap().get("initialize").unwrap(), &1);
            }
        }

        test_identity_manager! {
            (im(vec![
                IdentityManagerProtocol::DeleteServiceIdentity(service_identity!(0)),
                IdentityManagerProtocol::RequestServiceIdentity(ServiceCandidate::Deployment(deployment!("ns", "name"))),
                IdentityManagerProtocol::FoundServiceCandidate(ServiceCandidate::Deployment(deployment!("ns", "name"))),
                IdentityManagerProtocol::FoundServiceUser(service_user!(0), true),
                IdentityManagerProtocol::ActivatedServiceUser(service_user!(0), "ns".to_string(), "name".to_string()),
                IdentityManagerProtocol::IdentityCreatorReady,
                IdentityManagerProtocol::DeploymentWatcherReady,
                IdentityManagerProtocol::IdentityManagerReady,
                IdentityManagerProtocol::DeletedServiceCandidate(ServiceCandidate::Deployment(deployment!("ns", "name"))),
                IdentityManagerProtocol::ReleaseDeviceId(ServiceLookup::new("ns", "name", None), Uuid::new_v4()),
            ]), _api_counters, _watcher) => {
                assert_eq!(im.read().await.methods.lock().unwrap().get("initialize").unwrap(), &1);
                assert_eq!(im.read().await.methods.lock().unwrap().get("delete_service_identity").unwrap(), &1);
                assert_eq!(im.read().await.methods.lock().unwrap().get("request_service_identity").unwrap(), &1);
                assert_eq!(im.read().await.methods.lock().unwrap().get("found_service_candidate").unwrap(), &1);
                assert_eq!(im.read().await.methods.lock().unwrap().get("found_service_user").unwrap(), &1);
                assert_eq!(im.read().await.methods.lock().unwrap().get("activated_service_user").unwrap(), &1);
                assert_eq!(im.read().await.methods.lock().unwrap().get("identity_creator_ready").unwrap(), &1);
                assert_eq!(im.read().await.methods.lock().unwrap().get("deployment_watcher_ready").unwrap(), &1);
                assert_eq!(im.read().await.methods.lock().unwrap().get("identity_manager_ready").unwrap(), &1);
                assert_eq!(im.read().await.methods.lock().unwrap().get("deleted_service_candidate").unwrap(), &1);
                assert_eq!(im.read().await.methods.lock().unwrap().get("release_device_id").unwrap(), &1);
                assert_eq!(im.read().await.methods.lock().unwrap().get("get_message").unwrap(), &11);
            }
        }
    }
    /*
    #[tokio::test]
    async fn test_identity_manager_initialization() {
        test_identity_manager! {
            (watcher_rx, counters) => {
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
            (im(vec![]), watcher_rx, identity_creator_rx, _deployment_watched_rx, counters) => {
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
                            assert!(msg.eq("[ns1_srv1] ServiceIdentity with id ns1_srv1 was not registered"),
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
                            assert!(msg.eq("[ns1_srv1] ServiceIdentity with id ns1_srv1 unregistered"),
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
                    service_candidate: ServiceCandidate::Deployment(deployment!("ns1", "srv1")),
                }).await.expect("Unable to send RequestServiceIdentity message to IdentityManager");
                assert_message!(m :: IdentityCreatorProtocol::StartService in identity_creator_rx);
                assert_no_message!(identity_creator_rx);

                // Since we have an empty pool we can not create identities
                assert_message! {
                    (m :: IdentityManagerProtocol::IdentityManagerDebug(_) in watcher_rx) => {
                     if let IdentityManagerProtocol::IdentityManagerDebug(msg) = m {
                            assert!(msg.eq("[ns1_srv1] New ServiceIdentity requested for ServiceCandidate ns1_srv1"),
                                    "Wrong message, got {}", msg);
                        }
                    }
                }
                assert_message! {
                    (m :: IdentityManagerProtocol::IdentityManagerDebug(_) in watcher_rx) => {
                     if let IdentityManagerProtocol::IdentityManagerDebug(msg) = m {
                            assert!(msg.eq("[ns1_srv1] Unable to assign service identity for service ns1_srv1. Identities pool seems to be empty!"),
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
                            assert!(msg.eq("Found deactivated ServiceUser service_user1 (id: service_user_id1)"),
                                    "Wrong message, got {}", msg);
                        }
                    }
                }
                assert_message! {
                    (m :: IdentityManagerProtocol::IdentityManagerDebug(_) in watcher_rx) => {
                     if let IdentityManagerProtocol::IdentityManagerDebug(msg) = m {
                            assert!(msg.eq("Found deactivated ServiceUser service_user2 (id: service_user_id2)"),
                                    "Wrong message, got {}", msg);
                        }
                    }
                }

                // Request a new ServiceIdentity
                tx.send(IdentityManagerProtocol::RequestServiceIdentity {
                    service_candidate:  ServiceCandidate::Deployment(deployment!("ns1", "srv1")),
                }).await.expect("Unable to send RequestServiceIdentity message to IdentityManager");

                // We have deactivated credentials so we can create it
                assert_message! {
                    (m :: IdentityManagerProtocol::IdentityManagerDebug(_) in watcher_rx) => {
                     if let IdentityManagerProtocol::IdentityManagerDebug(msg) = m {
                            assert!(msg.eq("[ns1_srv1] New ServiceIdentity requested for ServiceCandidate ns1_srv1"),
                                    "Wrong message, got {}", msg);
                        }
                    }
                }
                assert_message! {
                    (m :: IdentityManagerProtocol::IdentityManagerDebug(_) in watcher_rx) => {
                     if let IdentityManagerProtocol::IdentityManagerDebug(msg) = m {
                            assert!(msg.eq("[ns1_srv1] ServiceIdentity created for service ns1_srv1"),
                                    "Wrong message, got {}", msg);
                        }
                    }
                }
                // Create it in k8s
                assert_eq!(counters.lock().unwrap().create_calls, 1);

                // We ask IC to create a new credential
                assert_message!(m :: IdentityCreatorProtocol::CreateIdentity in identity_creator_rx);
                // We add labels and we activate new credential
                assert_message! {
                    (m :: IdentityCreatorProtocol::ActivateServiceUser {..} in identity_creator_rx) => {
                        if let IdentityCreatorProtocol::ActivateServiceUser(service_user, cluster_id, service_ns, service_name, labels) = m {
                            assert_eq!(service_user.name, "service_user1");
                            assert_eq!(service_ns, "ns1");
                            assert_eq!(service_name, "srv1");
                            assert_eq!(cluster_id, "TestCluster");
                            assert_eq!(labels, HashMap::from([
                                ("namespace".to_string(), "ns1".to_string()),
                                ("name".to_string(), "srv1".to_string())
                            ]));
                        }
                    }
                }

                // Request a new ServiceIdentity, second one
                tx.send(IdentityManagerProtocol::RequestServiceIdentity {
                    service_candidate: ServiceCandidate::SDPService(sdp_service!("ns2", "srv2", "customservice")),
                }).await.expect("[ns2_srv2] Unable to send RequestServiceIdentity message to IdentityManager");

                // We have deactivated credentials so we can create it
                assert_message! {
                    (m :: IdentityManagerProtocol::IdentityManagerDebug(_) in watcher_rx) => {
                     if let IdentityManagerProtocol::IdentityManagerDebug(msg) = m {
                            assert!(msg.eq("[ns2_srv2] New ServiceIdentity requested for ServiceCandidate ns2_srv2"),
                                    "Wrong message, got {}", msg);
                        }
                    }
                }
                assert_message! {
                    (m :: IdentityManagerProtocol::IdentityManagerDebug(_) in watcher_rx) => {
                     if let IdentityManagerProtocol::IdentityManagerDebug(msg) = m {
                            assert!(msg.eq("[ns2_srv2] ServiceIdentity created for service ns2_srv2"),
                                    "Wrong message, got {}", msg);
                        }
                    }
                }
                // Create it in k8s
                assert_eq!(counters.lock().unwrap().create_calls, 2);

                // We ask IC to create a new credential
                assert_message!(m :: IdentityCreatorProtocol::CreateIdentity in identity_creator_rx);

                // We add labels and we activate new credential
                assert_message! {
                    (m :: IdentityCreatorProtocol::ActivateServiceUser {..} in identity_creator_rx) => {
                        if let IdentityCreatorProtocol::ActivateServiceUser(service_user, cluster_id, ns, name, labels) = m {
                            assert!(service_user.name == "service_user2");
                            assert!(ns == "ns2");
                            assert!(name == "srv2");
                            assert_eq!(cluster_id, "TestCluster");
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
                    service_candidate: ServiceCandidate::Deployment(deployment!("ns3", "srv1")),
                }).await.expect("[ns3_srv1] Unable to send RequestServiceIdentity message to IdentityManager");
                assert_message! {
                    (m :: IdentityManagerProtocol::IdentityManagerDebug(_) in watcher_rx) => {
                     if let IdentityManagerProtocol::IdentityManagerDebug(msg) = m {
                            assert!(msg.eq("[ns3_srv1] New ServiceIdentity requested for ServiceCandidate ns3_srv1"),
                                    "Wrong message, got {}", msg);
                        }
                    }
                }
                assert_message! {
                    (m :: IdentityManagerProtocol::IdentityManagerDebug(_) in watcher_rx) => {
                     if let IdentityManagerProtocol::IdentityManagerDebug(msg) = m {
                            assert!(msg.eq("[ns3_srv1] Unable to assign service identity for service ns3_srv1. Identities pool seems to be empty!"),
                                    "Wrong message, got {}", msg);
                        }
                    }
                }
                assert_no_message!(identity_creator_rx);

                // Request a new ServiceIdentity already created
                tx.send(IdentityManagerProtocol::RequestServiceIdentity {
                    service_candidate: ServiceCandidate::Deployment(deployment!("ns1", "srv1")),
                }).await.expect("[ns1_srv1] Unable to send RequestServiceIdentity message to IdentityManager");
                assert_message! {
                    (m :: IdentityManagerProtocol::IdentityManagerDebug(_) in watcher_rx) => {
                     if let IdentityManagerProtocol::IdentityManagerDebug(msg) = m {
                            assert!(msg.eq("[ns1_srv1] New ServiceIdentity requested for ServiceCandidate ns1_srv1"),
                                    "Wrong message, got {}", msg);
                        }
                    }
                }
                assert_message! {
                    (m :: IdentityManagerProtocol::IdentityManagerDebug(_) in watcher_rx) => {
                     if let IdentityManagerProtocol::IdentityManagerDebug(msg) = m {
                            assert!(msg.eq("[ns1_srv1] ServiceIdentity already exists for service ns1_srv1"),
                                    "Wrong message, got {}", msg);
                        }
                    }
                }
                // Create call count should remain the same because we are reusing ServiceIdentity
                assert_eq!(counters.lock().unwrap().create_calls, 2);

                assert_no_message!(identity_creator_rx);

                // Request a new ServiceIdentity (this time from SDPSercice) already created
                tx.send(IdentityManagerProtocol::RequestServiceIdentity {
                    service_candidate: ServiceCandidate::SDPService(sdp_service!("ns1", "srv1", "custom_service")),
                }).await.expect("[ns1_srv1] Unable to send RequestServiceIdentity message to IdentityManager");
                assert_message! {
                    (m :: IdentityManagerProtocol::IdentityManagerDebug(_) in watcher_rx) => {
                        if let IdentityManagerProtocol::IdentityManagerDebug(msg) = m {
                            assert!(msg.eq("[ns1_srv1] New ServiceIdentity requested for ServiceCandidate ns1_srv1"),
                                    "Wrong message, got {}", msg);
                        }
                    }
                }
                assert_message! {
                    (m :: IdentityManagerProtocol::IdentityManagerDebug(_) in watcher_rx) => {
                        if let IdentityManagerProtocol::IdentityManagerDebug(msg) = m {
                            assert!(msg.eq("[ns1_srv1] ServiceIdentity already exists for service ns1_srv1"),
                                    "Wrong message, got {}", msg);
                        }
                    }
                }
                // Create call count should remain the same because we are reusing ServiceIdentity
                assert_eq!(counters.lock().unwrap().create_calls, 2);

                assert_no_message!(identity_creator_rx);
            }
        }
    }

    #[tokio::test]
    async fn test_identity_manager_request_service_identity_2() {
        test_identity_manager! {
            (im(vec![]), watcher_rx, identity_manager_tx, identity_creator_rx, _deployment_watched_rx, counters) => {
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
                        let expected_msg = format!("Found deactivated ServiceUser service_user{} (id: service_user_id{})", i, i);
                         if let IdentityManagerProtocol::IdentityManagerDebug(msg) = m {
                                assert!(msg.eq(expected_msg.as_str()),
                                        "Wrong message, expected {} but got {}", expected_msg.as_str(), msg);
                            }
                        }
                    }
                }
                // Request a new ServiceIdentity
                tx.send(IdentityManagerProtocol::RequestServiceIdentity {
                service_candidate: ServiceCandidate::SDPService(sdp_service!("ns1", "srv1", "customservice")),
                }).await.expect("[ns1_srv1] Unable to send RequestServiceIdentity message to IdentityManager");

                // We have deactivated credentials so we can create it
                assert_message! {
                    (m :: IdentityManagerProtocol::IdentityManagerDebug(_) in watcher_rx) => {
                        if let IdentityManagerProtocol::IdentityManagerDebug(msg) = m {
                            assert!(msg.eq("[ns1_srv1] New ServiceIdentity requested for ServiceCandidate ns1_srv1"),
                                    "Wrong message, got {}", msg);
                        }
                    }
                }
                assert_message! {
                    (m :: IdentityManagerProtocol::IdentityManagerDebug(_) in watcher_rx) => {
                        if let IdentityManagerProtocol::IdentityManagerDebug(msg) = m {
                            assert!(msg.eq("[ns1_srv1] ServiceIdentity created for service ns1_srv1"),
                                    "Wrong message, got {}", msg);
                        }
                    }
                }
                // Create it in k8s
                assert_eq!(counters.lock().unwrap().create_calls, 1);

                // Note that now we dont ask IC to create a new credential, since the pool has enough credentials
                // We add labels and we activate new credential
                assert_message! {
                    (m :: IdentityCreatorProtocol::ActivateServiceUser {..} in identity_creator_rx) => {
                        if let IdentityCreatorProtocol::ActivateServiceUser(service_user, cluster_id, service_ns, service_name, labels) = m {
                            assert_eq!(service_user.name, "service_user1");
                            assert_eq!(service_ns, "ns1");
                            assert_eq!(service_name, "srv1");
                            assert_eq!(cluster_id, "TestCluster");
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
                        let expected_msg = format!("Found activated ServiceUser service_user{} (id: service_user_id{})", i, i);
                         if let IdentityManagerProtocol::IdentityManagerDebug(msg) = m {
                                assert!(msg.eq(expected_msg.as_str()),
                                        "Wrong message, expected {} but got {}", expected_msg.as_str(), msg);
                            }
                        }
                    }
                }
                // Request a new ServiceIdentity
                tx.send(IdentityManagerProtocol::RequestServiceIdentity {
                service_candidate: ServiceCandidate::Deployment(deployment!("ns1", "srv1")),
                }).await.expect("[ns1_srv1] Unable to send RequestServiceIdentity message to IdentityManager");

                // Since we have an empty pool we can not create identities
                assert_message! {
                    (m :: IdentityManagerProtocol::IdentityManagerDebug(_) in watcher_rx) => {
                     if let IdentityManagerProtocol::IdentityManagerDebug(msg) = m {
                            assert!(msg.eq("[ns1_srv1] New ServiceIdentity requested for ServiceCandidate ns1_srv1"),
                                    "Wrong message, got {}", msg);
                        }
                    }
                }
                assert_message! {
                    (m :: IdentityManagerProtocol::IdentityManagerDebug(_) in watcher_rx) => {
                     if let IdentityManagerProtocol::IdentityManagerDebug(msg) = m {
                            assert!(msg.eq("[ns1_srv1] Unable to assign service identity for service ns1_srv1. Identities pool seems to be empty!"),
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
                    service_candidate: ServiceCandidate::Deployment(deployment!("ns1", "srv1")),
                }).await.expect("[ns1_srv1] Unable to send RequestServiceIdentity message to IdentityManager");
                // We ask IC to create a new credential
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
                        let expected_msg = format!("Found activated ServiceUser service_user{} (id: service_user_id{})", i, i);
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
                // We expect 2 DeploymentWatchers to report readiness
                tx.send(IdentityManagerProtocol::DeploymentWatcherReady).await.expect("Unable to send message!");
                tx.send(IdentityManagerProtocol::DeploymentWatcherReady).await.expect("Unable to send message!");
                let mut extra_credentials_expected: HashSet<String> = HashSet::from_iter((1 .. 12).map(|i| format!("service_user_id{}", i)).collect::<Vec<_>>());
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
                // We expect 2 DeploymentWatchers to report readiness
                tx.send(IdentityManagerProtocol::DeploymentWatcherReady).await.expect("Unable to send message!");
                tx.send(IdentityManagerProtocol::DeploymentWatcherReady).await.expect("Unable to send message!");
                // Syncing UserCredentials log message
                assert_message!(m :: IdentityManagerProtocol::IdentityManagerDebug(_) in watcher_rx);
                // Syncing ServiceIdentities log message
                assert_message!(m :: IdentityManagerProtocol::IdentityManagerDebug(_) in watcher_rx);

                // Check first that IM unregistered the ServiceIdentity instances
                let mut extra_service_identities: HashSet<String> = HashSet::from_iter((1 .. 5)
                    .map(|i| format!("[ns{}_srv{}] ServiceIdentity with id ns{}_srv{} unregistered", i, i, i, i))
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
    */
}
