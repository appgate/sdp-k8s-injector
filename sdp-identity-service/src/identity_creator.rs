use std::collections::{HashMap, HashSet};

use json_patch::PatchOperation::Remove;
use json_patch::{Patch, RemoveOperation};
use k8s_openapi::api::apps::v1::Deployment;
use k8s_openapi::api::core::v1::{ConfigMap, Secret};
use kube::api::{Patch as KubePatch, PatchParams};
use kube::{Api, Client};
use sdp_common::constants::{IDENTITY_MANAGER_SECRET_NAME, SDP_CLUSTER_ID_ENV, SDP_IDP_NAME};
use sdp_common::kubernetes::SDP_K8S_NAMESPACE;
use sdp_common::sdp::auth::SDPUser;
use sdp_common::sdp::system::{ClientProfile, ClientProfileUrl, System};
use sdp_common::service::{get_profile_client_url_name, get_service_username, ServiceUser};
use sdp_macros::{logger, sdp_debug, sdp_error, sdp_info, sdp_log, sdp_warn, with_dollar_sign};
use tokio::sync::mpsc::{Receiver, Sender};

use crate::errors::IdentityServiceError;
use crate::identity_manager::{IdentityManagerProtocol, ServiceIdentity};

logger!("IdentityCreator");

#[derive(Debug)]
pub enum IdentityCreatorProtocol {
    StartService,
    CreateIdentity,
    // service user, cluster_id, service_ns, service_name, labels
    ActivateServiceUser(ServiceUser, String, String, String, HashMap<String, String>),
    // service user, service namespace, service name
    DeleteServiceUser(ServiceUser, String, String),
    // sdp user name
    DeleteSDPUser(String),
}

pub struct IdentityCreator {
    system: System,
    client: Client,
    service_users_pool_size: usize,
    cluster_id: String,
}

async fn get_or_create_client_profile_url(
    system: &mut System,
    cluster_id: &str,
) -> Result<ClientProfileUrl, IdentityServiceError> {
    // Create ClientProfile if needed
    let profile_name = get_profile_client_url_name(cluster_id);
    let ps = system
        .get_client_profiles(None)
        .await
        .map_err(|e| format!("Unable to get client profiles: {}", e.to_string()))?;
    let (profile_id, profile_name) = match ps.iter().filter(|p| p.name == profile_name).next() {
        Some(p) => (p.id.clone(), p.name.clone()),
        None => {
            warn!(
                "Unable to find client profile url for cluster {}, creating a new one",
                profile_name
            );
            let spa_key_name = profile_name.replace(" ", "").to_lowercase();
            let p = ClientProfile {
                id: uuid::Uuid::new_v4().to_string(),
                name: get_profile_client_url_name(cluster_id),
                spa_key_name: spa_key_name,
                identity_provider_name: SDP_IDP_NAME.to_string(),
                tags: vec![],
            };
            let p = system
                .create_client_profile(&p)
                .await
                .map_err(|e| format!("Unable to create a new client profile: {}", e))?;
            debug!("Client Profile: {:?}", p);
            (p.id, p.name)
        }
    };
    system
        .get_profile_client_url(&profile_id)
        .await
        .map_err(|e| {
            IdentityServiceError::from(format!(
                "Unable to get the client profile url for client profile {}: {}",
                profile_name,
                e.to_string()
            ))
        })
}

impl IdentityCreator {
    pub fn new(system: System, client: Client, credentials_pool_size: usize) -> IdentityCreator {
        let cluster_id = std::env::var(SDP_CLUSTER_ID_ENV);
        if cluster_id.is_err() {
            panic!(
                "Unable to get cluster id, make sure SDP_CLUSTER_ID environment variable is set."
            );
        }
        IdentityCreator {
            system,
            client: client,
            service_users_pool_size: credentials_pool_size,
            cluster_id: cluster_id.unwrap(),
        }
    }

    // TODO: We should avoid to clone this client all the time
    fn secrets_api(&self, service_ns: &str) -> Api<Secret> {
        Api::namespaced(self.client.clone(), service_ns)
    }

    // TODO: We should avoid to clone this client all the time
    fn configmap_api(&self, service_ns: &str) -> Api<ConfigMap> {
        Api::namespaced(self.client.clone(), service_ns)
    }

    async fn create_user(&mut self) -> Result<ServiceUser, IdentityServiceError> {
        let service_user = SDPUser::new();
        let profile_url =
            get_or_create_client_profile_url(&mut self.system, &self.cluster_id).await?;
        info!(
            "Creating ServiceUser {} (id: {})",
            service_user.name, service_user.id
        );
        if let Some(service_user) = self
            .system
            .create_user(&service_user)
            .await
            .map(|u| ServiceUser::from_sdp_user(&u, &profile_url, None))?
        {
            service_user
                .update_secrets_fields(
                    self.secrets_api(SDP_K8S_NAMESPACE),
                    IDENTITY_MANAGER_SECRET_NAME,
                )
                .await
                .map_err(|e| IdentityServiceError::from(e.to_string()))
                .map(|_| service_user)
        } else {
            Err(IdentityServiceError::from(
                "Unable to create ServiceUser from SDPUser (missing password?)".to_string(),
            ))
        }
    }

    async fn delete_sdp_user(&mut self, sdp_user_id: &str) -> Result<(), IdentityServiceError> {
        // Create a default SDPUser with the name of the one we want to delete
        let sdp_user = SDPUser::from_name(sdp_user_id.to_string());
        // Derive a ServiceUser that we can use to delete the secret fields
        if let Some(service_user) =
            ServiceUser::from_sdp_user(&sdp_user, &ClientProfileUrl::default(), None)
        {
            service_user
                .delete_secrets_fields(
                    self.secrets_api(SDP_K8S_NAMESPACE),
                    IDENTITY_MANAGER_SECRET_NAME,
                )
                .await
                .map_err(|e| IdentityServiceError::from(e.to_string()))?;
        }
        info!("Deleting SDPUser {} (id: {})", sdp_user.name, sdp_user.id);
        self.system
            .delete_user(sdp_user_id)
            .await
            .map_err(|e| IdentityServiceError::from(e.to_string()))
    }

    async fn recover_sdp_user(
        &mut self,
        sdp_user: &SDPUser,
        client_profile_url: &ClientProfileUrl,
    ) -> Option<ServiceUser> {
        let api = self.secrets_api(SDP_K8S_NAMESPACE);
        // Create first the ServiceUser with a random password
        let service_user = ServiceUser::from_sdp_user(
            sdp_user,
            client_profile_url,
            Some(&uuid::Uuid::new_v4().to_string()),
        )
        .unwrap();
        service_user
            .restore(api, IDENTITY_MANAGER_SECRET_NAME)
            .await
    }

    async fn cleanup_secret_entries(
        &mut self,
        known_fields: HashSet<String>,
    ) -> Result<(), IdentityServiceError> {
        let api = self.secrets_api(SDP_K8S_NAMESPACE);
        let secret = api.get(IDENTITY_MANAGER_SECRET_NAME).await?;
        let mut n: u32 = 0;
        let mut patches = vec![];
        if let Some(data) = secret.data {
            for (field, _) in data {
                if !known_fields.contains(&field) {
                    info!("Secret entry for SDPUser {} marked for deletion", field);
                    patches.push(Remove(RemoveOperation {
                        path: format!("/data/{}", field),
                    }));
                    n += 1;
                }
            }
            if !patches.is_empty() {
                info!(
                    "Removing {} old entries from global secret {}",
                    n, IDENTITY_MANAGER_SECRET_NAME
                );
                let patch: KubePatch<Secret> = KubePatch::Json(Patch(patches));
                api.patch(
                    IDENTITY_MANAGER_SECRET_NAME,
                    &PatchParams::default(),
                    &patch,
                )
                .await?;
            }
        };
        Ok(())
    }

    async fn delete_user(
        &mut self,
        service_user: &ServiceUser,
        service_ns: &str,
        service_name: &str,
    ) -> Result<(), IdentityServiceError> {
        self.delete_sdp_user(&service_user.id).await?;
        service_user
            .delete_secrets(self.secrets_api(service_ns), service_ns, service_name)
            .await
            .map_err(|e| {
                IdentityServiceError::from(format!(
                    "[{}] Unable to delete secrets for ServiceUser {}: {}",
                    format!("{}_{}", service_ns, service_name),
                    service_user.name,
                    e.to_string()
                ))
            })?;
        service_user
            .delete_config(self.configmap_api(service_ns), service_ns, service_name)
            .await
            .map_err(|e| {
                IdentityServiceError::from(format!(
                    "[{}] Unable to delete secrets for ServiceUser {}: {}",
                    format!("{}_{}", service_ns, service_name),
                    service_user.name,
                    e.to_string()
                ))
            })?;
        Ok(())
    }

    pub async fn initialize(
        &mut self,
        system: &mut System,
        identity_manager_proto_tx: Sender<IdentityManagerProtocol<Deployment, ServiceIdentity>>,
    ) -> Result<ClientProfileUrl, IdentityServiceError> {
        let users = system.get_users().await?;
        let client_profile_url = get_or_create_client_profile_url(system, &self.cluster_id).await?;
        // Notify ServiceIdentityManager about the actual credentials created in appgate
        let mut n_missing_users = self.service_users_pool_size;
        // This could be activated credentials or deactivated ones.
        let mut known_service_users = HashSet::new();
        for sdp_user in users {
            // We got a SDPUser. We dont have any way to recover passwords
            // from there (SDPUsers dont contain the password when fetched from a controller)
            // IM always saves those creds for ServiceUsers that are deactivated.
            // If we dont have a password for this user, don't recover it and ask IC to delete as soon as possible.

            // Derive now our ServiceUser from SDPUser, recovering passwords if needed.
            if let Some(service_user) = self.recover_sdp_user(&sdp_user, &client_profile_url).await
            {
                let activated = !sdp_user.disabled;
                if !activated && n_missing_users > 0 {
                    debug!("Missing user count: {}", n_missing_users);
                    n_missing_users -= 1;
                }
                let (_, pw_field, _) = service_user.secrets_field_names(false);
                known_service_users.insert(pw_field);
                let msg = IdentityManagerProtocol::FoundServiceUser(service_user, activated);
                identity_manager_proto_tx.send(msg).await?;
            } else {
                error!(
                    "Error recovering ServiceUser information from SDPUser {}. Deleting SDPUser.",
                    sdp_user.name
                );
                if let Err(e) = self.delete_sdp_user(&sdp_user.name).await {
                    error!(
                        "Error deleting SDPUser {} from collective: {}",
                        sdp_user.name,
                        e.to_string()
                    );
                }
            }
        }

        // Delete old entries from the global secret
        self.cleanup_secret_entries(known_service_users).await?;

        // Create needed credentials until we reach the desired number of credentials pool
        info!("Creating {} ServiceUsers in system", n_missing_users);
        for _i in 0..n_missing_users {
            let service_user = self.create_user().await?;
            info!(
                "New ServiceUser {} (id: {}) created, notifying IdentityManager",
                service_user.name, service_user.id
            );
            identity_manager_proto_tx
                .send(IdentityManagerProtocol::FoundServiceUser(
                    service_user,
                    false,
                ))
                .await?;
        }
        Ok(client_profile_url)
    }

    pub async fn run(
        mut self,
        system: &mut System,
        mut identity_creator_proto_rx: Receiver<IdentityCreatorProtocol>,
        identity_manager_proto_tx: Sender<IdentityManagerProtocol<Deployment, ServiceIdentity>>,
    ) -> () {
        while let Some(msg) = identity_creator_proto_rx.recv().await {
            match msg {
                IdentityCreatorProtocol::StartService => {
                    info!("Identity Creator is ready");
                    break;
                }
                msg => {
                    warn!("IdentityCreator is not ready, ignoring message {:?}", msg);
                }
            }
        }
        info!("Starting IdentityCreator");
        if let Err(e) = self
            .initialize(system, identity_manager_proto_tx.clone())
            .await
        {
            error!(
                "Error while initializing IdentityCreator: {}",
                e.to_string()
            );
            panic!();
        }
        // Notify IdentityManager that we are ready
        identity_manager_proto_tx
            .send(IdentityManagerProtocol::IdentityCreatorReady)
            .await
            .expect("Unable to notify IdentityManager");

        while let Some(message) = identity_creator_proto_rx.recv().await {
            match message {
                IdentityCreatorProtocol::CreateIdentity => {
                    match self.create_user().await {
                        Ok(service_user) => {
                            info!(
                                "New ServiceUser {} (id: {}) created, notifying IdentityManager",
                                service_user.name, service_user.id
                            );
                            let msg =
                                IdentityManagerProtocol::FoundServiceUser(service_user, false);
                            if let Err(err) = identity_manager_proto_tx.send(msg).await {
                                error!("Error notifying identity: {}", err);
                                // TODO: Try later, identity is already created
                            }
                        }
                        Err(err) => {
                            error!("Error creating new identity: {}", err);
                        }
                    };
                }
                IdentityCreatorProtocol::DeleteServiceUser(
                    service_user,
                    service_ns,
                    service_name,
                ) => {
                    info!(
                        "[{}] Deleting ServiceUser {}",
                        format!("{}_{}", service_ns, service_name),
                        service_user.name
                    );

                    if let Err(err) = self
                        .delete_user(&service_user, &service_ns, &service_name)
                        .await
                    {
                        error!(
                            "[{}] Error deleting ServiceUser {} (id: {}): {}",
                            format!("{}_{}", service_ns, service_name),
                            service_user.name,
                            service_user.id,
                            err
                        );
                    }
                }
                IdentityCreatorProtocol::ActivateServiceUser(
                    service_user,
                    cluster_id,
                    service_ns,
                    service_name,
                    labels,
                ) => {
                    let mut sdp_user = SDPUser::from(&service_user);
                    sdp_user.disabled = false;
                    sdp_user.labels = labels;
                    sdp_user.name = get_service_username(&cluster_id, &service_ns, &service_name);
                    let mut service_user = service_user.clone();
                    service_user.name = sdp_user.name.clone();

                    info!(
                        "[{}] Activating ServiceUser {} (id: {})",
                        format!("{}_{}", service_ns, service_name),
                        sdp_user.name,
                        sdp_user.id
                    );
                    if let Err(err) = system.modify_user(&sdp_user).await {
                        error!(
                            "[{}] Unable to activate ServiceUser {} (id: {}): {}",
                            format!("{}_{}", service_ns, service_name),
                            service_user.name,
                            service_user.id,
                            err
                        );
                    }

                    if let Err(err) = identity_manager_proto_tx
                        .send(IdentityManagerProtocol::ActivatedServiceUser(
                            service_user.clone(),
                            service_ns.to_string(),
                            service_name.to_string(),
                        ))
                        .await
                    {
                        error!(
                            "[{}] Unable to notify IdentityManager about activated ServiceUSer {}: {}",
                             format!("{}_{}", service_ns, service_name), service_user.name,  err
                        );
                    }

                    // Create secrets now
                    info!(
                        "[{}] Creating secrets for ServiceUser {} (id: {})",
                        format!("{}_{}", service_ns, service_name),
                        service_user.name,
                        service_user.id
                    );
                    if let Err(e) = service_user
                        .create_secrets(self.secrets_api(&service_ns), &service_ns, &service_name)
                        .await
                    {
                        error!(
                            "[{}] Error creating secrets for ServiceUser {}: {}",
                            format!("{}_{}", service_ns, service_name),
                            service_user.name,
                            e.to_string()
                        );
                    }

                    info!(
                        "[{}] Creating config for ServiceUser {}",
                        format!("{}_{}", service_ns, service_name),
                        service_name
                    );
                    if let Err(e) = service_user
                        .create_config(self.configmap_api(&service_ns), &service_ns, &service_name)
                        .await
                    {
                        error!(
                            "[{}] Error creating secrets for ServiceUser {}: {}",
                            format!("{}_{}", service_ns, service_name),
                            service_user.name,
                            e.to_string()
                        );
                    }
                }
                IdentityCreatorProtocol::DeleteSDPUser(sdp_user_name) => {
                    if let Err(e) = self.delete_sdp_user(&sdp_user_name).await {
                        error!(
                            "Error deleting SDPUser {}: {}",
                            sdp_user_name,
                            e.to_string()
                        );
                    }
                }
                _ => {}
            }
        }
    }
}
