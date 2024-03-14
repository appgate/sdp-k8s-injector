use std::collections::{HashMap, HashSet};

use json_patch::PatchOperation::Remove;
use json_patch::{Patch, RemoveOperation};
use k8s_openapi::api::core::v1::{ConfigMap, Secret};
use kube::api::{Patch as KubePatch, PatchParams};
use kube::{Api, Client};
use sdp_common::constants::{IDENTITY_MANAGER_SECRET_NAME, SDP_CLUSTER_ID_ENV, SDP_IDP_NAME};
use sdp_common::kubernetes::SDP_K8S_NAMESPACE;
use sdp_common::sdp::auth::SDPUser;
use sdp_common::sdp::system::{ClientProfile, ClientProfileUrl, System};
use sdp_common::service::{
    get_profile_client_url_name, get_service_username, ServiceCandidate, ServiceUser,
};
use sdp_macros::{logger, sdp_debug, sdp_error, sdp_info, sdp_log, sdp_warn, with_dollar_sign};
use tokio::sync::mpsc::{Receiver, Sender};
use uuid::Uuid;

use crate::errors::IdentityServiceError;
use crate::identity_manager::{IdentityManagerProtocol, ServiceIdentity};

logger!("IdentityCreator");

/*
 * Protocol exchanged between IdentityCreator and IdentityManager
 * IdentityManager sends messages to IdentityCreator
 */
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
    ReconcileSDPUsers(HashSet<String>),
    ReleaseDeviceId(ServiceUser, Uuid),
}

pub struct IdentityCreator {
    system: System,
    client: Client,
    service_users_pool_size: usize,
    cluster_id: String,
}

#[derive(PartialEq)]
enum ClientProfileType {
    FreshClientProfile(ClientProfile),
    ExistingClientProfile(ClientProfile),
}

fn get_or_create_client_profile_url<'a>(
    cluster_id: &str,
    ps: &'a Vec<ClientProfile>,
) -> (ClientProfileType, Option<Vec<&'a ClientProfile>>) {
    let (profile_name, prefix_name) = get_profile_client_url_name(cluster_id);
    let a: Vec<&ClientProfile> = ps
        .iter()
        .filter(|p| p.name.starts_with(&prefix_name))
        .collect();
    match a[..] {
        [] => {
            warn!(
                "Unable to find client profile url for cluster {}, creating a new one",
                cluster_id
            );
            let spa_key_name = profile_name.replace(" ", "").to_lowercase();
            let p = ClientProfile {
                id: uuid::Uuid::new_v4().to_string(),
                name: profile_name,
                spa_key_name: spa_key_name,
                identity_provider_name: SDP_IDP_NAME.to_string(),
                tags: vec![],
            };
            (ClientProfileType::FreshClientProfile(p), None)
        }
        [p] => (ClientProfileType::ExistingClientProfile(p.clone()), None),
        _ => (
            ClientProfileType::ExistingClientProfile(a[0].clone()),
            Some(a[1..].to_vec()),
        ),
    }
}

async fn get_client_profile_url(
    system: &mut System,
    cluster_id: &str,
) -> Result<ClientProfileUrl, IdentityServiceError> {
    // Create ClientProfile if needed
    let ps = system
        .get_client_profiles(None)
        .await
        .map_err(|e| format!("Unable to get client profiles: {}", e.to_string()))?;
    let (profile_id, profile_name) = match get_or_create_client_profile_url(cluster_id, &ps) {
        (ClientProfileType::FreshClientProfile(p), _) => {
            warn!(
                "Unable to find client profile url for cluster {}, creating a new one {}",
                cluster_id, p.name
            );
            let p = system
                .create_client_profile(&p)
                .await
                .map_err(|e| format!("Unable to create a new client profile: {}", e))?;
            (p.id, p.name)
        }
        (ClientProfileType::ExistingClientProfile(p), maybe_ps) => {
            info!(
                "Found existing client profile: {} for cluster {}",
                p.name, cluster_id
            );
            if let Some(ps) = maybe_ps {
                let ss: Vec<String> = ps.iter().map(|p| p.name.clone()).collect();
                warn!("Found several client profiles associated with this cluster {}: {}. They should be deleted",
                    cluster_id,
                    ss[..].join(",")
                );
            }
            (p.id.clone(), p.name.clone())
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
    pub fn new(system: System, client: Client, service_users_pool_size: usize) -> IdentityCreator {
        let cluster_id = std::env::var(SDP_CLUSTER_ID_ENV);
        if cluster_id.is_err() {
            panic!(
                "Unable to get cluster id, make sure SDP_CLUSTER_ID environment variable is set."
            );
        }
        IdentityCreator {
            system,
            client,
            service_users_pool_size,
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
        let service_user = SDPUser::new(Uuid::new_v4().to_string(), None, None);
        let profile_url = get_client_profile_url(&mut self.system, &self.cluster_id).await?;
        info!(
            "Creating ServiceUser {} (id: {})",
            service_user.name, service_user.id
        );
        if let Some(service_user) = self
            .system
            .create_user(&service_user)
            .await
            .map(|u| ServiceUser::from_sdp_user(&u, &profile_url, None, Some(vec![])))?
        {
            service_user
                .update_secrets_fields(
                    self.secrets_api(SDP_K8S_NAMESPACE),
                    IDENTITY_MANAGER_SECRET_NAME,
                    false,
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

    async fn delete_sdp_user(&mut self, sdp_user: SDPUser) -> Result<(), IdentityServiceError> {
        // Derive a ServiceUser that we can use to delete the secret fields
        if let Some(service_user) =
            ServiceUser::from_sdp_user(&sdp_user, &ClientProfileUrl::default(), None, Some(vec![]))
        {
            service_user
                .delete_secrets_fields(
                    self.secrets_api(SDP_K8S_NAMESPACE),
                    IDENTITY_MANAGER_SECRET_NAME,
                )
                .await
                .map_err(|e| IdentityServiceError::from(e.to_string()))?;
        }
        info!(
            "Deleting SDPUser and associated device ids {} (id: {})",
            sdp_user.name, sdp_user.id
        );
        let user_name = sdp_user.prefix_name();
        if let Err(e) = self
            .system
            .unregister_device_ids_for_username(&user_name, None, None, None, false)
            .await
        {
            error!(
                "[{}] Unable to unregister device ids: {}",
                &user_name,
                e.to_string()
            );
        }
        self.system
            .delete_user(&sdp_user.id)
            .await
            .map_err(|e| IdentityServiceError::from(e.to_string()))
    }

    async fn recover_sdp_user(
        &mut self,
        sdp_user: &SDPUser,
        client_profile_url: &ClientProfileUrl,
        device_ids: Vec<Uuid>,
    ) -> Option<ServiceUser> {
        let api = self.secrets_api(SDP_K8S_NAMESPACE);
        // Create first the ServiceUser with a random password
        let service_user = ServiceUser::from_sdp_user(
            sdp_user,
            client_profile_url,
            Some(&uuid::Uuid::new_v4().to_string()),
            Some(device_ids.iter().map(|uuid| uuid.to_string()).collect()),
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

    pub async fn initialize(
        &mut self,
        system: &mut System,
        identity_manager_proto_tx: Sender<
            IdentityManagerProtocol<ServiceCandidate, ServiceIdentity>,
        >,
    ) -> Result<ClientProfileUrl, IdentityServiceError> {
        let users = system.get_users().await?;
        let client_profile_url = get_client_profile_url(system, &self.cluster_id).await?;
        // Notify ServiceIdentityManager about the actual credentials created in appgate
        let mut n_missing_users = self.service_users_pool_size;
        // This could be activated credentials or deactivated ones.
        let mut known_service_users = HashSet::new();
        for sdp_user in users {
            // We got a SDPUser. We dont have any way to recover passwords
            // from there (SDPUsers dont contain the password when fetched from a controller)
            // IM always saves those creds for ServiceUsers that are deactivated.
            // If we dont have a password for this user, don't recover it and ask IC to delete it as soon as possible.

            // Get the current registered device ids for use
            info!("Recovering SDPUser {}", &sdp_user.name);
            let mut device_ids = vec![];
            if let Ok(on_boarded_users) = system.get_registered_device_ids_for_user(&sdp_user).await
            {
                device_ids = on_boarded_users
                    .iter()
                    .map(|u| {
                        if let Ok(device_id) = Uuid::parse_str(&u.device_id) {
                            Some(device_id)
                        } else {
                            error!(
                                "[{}] Unable to parse device id: {}",
                                sdp_user.name, &u.device_id
                            );
                            None
                        }
                    })
                    .filter(Option::is_some)
                    .map(Option::unwrap)
                    .collect();
            } else {
                error!("[{}] Unable to recover device ids for user", sdp_user.name);
            }

            // Derive now our ServiceUser from SDPUser, recovering passwords if needed.
            if let Some(service_user) = self
                .recover_sdp_user(&sdp_user, &client_profile_url, device_ids)
                .await
            {
                let activated = !sdp_user.disabled;
                if !activated && n_missing_users > 0 {
                    debug!("Missing user count: {}", n_missing_users);
                    n_missing_users -= 1;
                }
                let (_, pw_field, _) = service_user.secrets_field_names(false);
                known_service_users.insert(pw_field);
                let msg: IdentityManagerProtocol<ServiceCandidate, ServiceIdentity> =
                    IdentityManagerProtocol::FoundServiceUser(service_user, activated);
                identity_manager_proto_tx.send(msg).await?;
            } else {
                let sdp_user_name = sdp_user.name.clone();
                error!(
                    "Error recovering ServiceUser information from SDPUser {}. Deleting SDPUser.",
                    sdp_user_name
                );
                if let Err(e) = self.delete_sdp_user(sdp_user).await {
                    error!(
                        "Error deleting SDPUser {} from collective: {}",
                        sdp_user_name,
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
        identity_manager_proto_tx: Sender<
            IdentityManagerProtocol<ServiceCandidate, ServiceIdentity>,
        >,
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
                            let user_name = service_user.name.clone();
                            info!(
                                "New ServiceUser {} (id: {}) created, notifying IdentityManager",
                                user_name, service_user.id
                            );
                            let msg: IdentityManagerProtocol<ServiceCandidate, ServiceIdentity> =
                                IdentityManagerProtocol::FoundServiceUser(service_user, false);
                            if let Err(err) = identity_manager_proto_tx.send(msg).await {
                                error!("[{}] Error notifying identity: {}", user_name, err);
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
                        "[{}|{}] Deleting ServiceUser",
                        service_user.name, service_user.id
                    );
                    let sdp_user = SDPUser::new(
                        service_user.id.clone(),
                        Some(service_user.name.clone()),
                        None,
                    );
                    if let Err(err) = self.delete_sdp_user(sdp_user).await {
                        error!(
                            "[{}|{}] Error deleting ServiceUser: {}",
                            service_user.name, service_user.id, err
                        );
                    }
                    info!("[{}] Deleting secrets", service_user.name);
                    if let Err(err) = service_user
                        .delete_secrets(self.secrets_api(&service_ns), &service_ns, &service_name)
                        .await
                    {
                        error!("[{}] Error deleting secrets: {}", service_user.name, err);
                    }
                    info!("[{}] Deleting config", service_user.name);
                    if let Err(err) = service_user
                        .delete_config(self.configmap_api(&service_ns), &service_ns, &service_name)
                        .await
                    {
                        error!("[{}] Error deleting config: {}", service_user.name, err);
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
                        .create_or_update_secrets(
                            self.secrets_api(&service_ns),
                            &service_ns,
                            &service_name,
                        )
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
                IdentityCreatorProtocol::DeleteSDPUser(sdp_user_id) => {
                    if let Err(e) = self
                        .delete_sdp_user(SDPUser::new(sdp_user_id.clone(), None, None))
                        .await
                    {
                        error!(
                            "[{}] Error deleting SDPUser: {}",
                            sdp_user_id,
                            e.to_string()
                        );
                    }
                }
                IdentityCreatorProtocol::ReconcileSDPUsers(sdp_users) => {
                    info!("Reconciling SDPUsers: {:?}", sdp_users);
                    for sdp_user in &sdp_users {
                        let sdp_user_name =
                            SDPUser::new(sdp_user.clone(), Some(sdp_user.clone()), Some(false))
                                .prefix_name();
                        info!("[{}] Reconciling SDPUser by releasing device ids", &sdp_user);
                        if let Err(e) = self
                            .system
                            .unregister_device_ids_for_username(
                                &sdp_user_name,
                                Some(&sdp_users),
                                None,
                                None,
                                false,
                            )
                            .await
                        {
                            error!(
                                "[{}] Error reconciling SDPUser by releasing device ids: {}",
                                sdp_user_name,
                                e.to_string()
                            );
                        }
                    }
                }
                IdentityCreatorProtocol::ReleaseDeviceId(service_user, device_id) => {
                    if let Err(e) = system
                        .unregister_device_id_for_user(
                            &SDPUser::new(service_user.id, Some(service_user.name.clone()), None),
                            &device_id,
                            false,
                        )
                        .await
                    {
                        error!(
                            "[{}] Error releasing device ids owned by SDPUser: {}",
                            service_user.name,
                            e.to_string()
                        );
                    }
                }
                _ => {}
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::get_or_create_client_profile_url;
    use crate::identity_creator::{ClientProfile, ClientProfileType};

    macro_rules! client_profile {
        ($id:expr, $name:expr) => {
            ClientProfile {
                id: format!("xxxxxx{}", $id),
                name: format!("{}-{}", $name, $id),
                spa_key_name: format!("my-spa-key-{}", $id),
                identity_provider_name: format!("service"),
                tags: vec![],
            }
        };
    }

    #[test]
    fn test_get_or_create_client_profile_url_0() {
        if let (ClientProfileType::FreshClientProfile(p), None) =
            get_or_create_client_profile_url("my-cluster-1", &vec![])
        {
            assert!(p.name.starts_with("my-cluster-1"))
        } else {
            assert!(false, "Not a FreshClientProfile without leftovers!");
        }
    }

    #[test]
    fn test_get_or_create_client_profile_url_1() {
        let ps = vec![client_profile!("2", "my-cluster-1")];
        if let (ClientProfileType::ExistingClientProfile(p), None) =
            get_or_create_client_profile_url("my-cluster-1", &ps)
        {
            assert!(p.name.starts_with("my-cluster-1-2"))
        } else {
            assert!(false, "Not a ExistingClientProfile without leftovers");
        }
    }

    #[test]
    fn test_get_or_create_client_profile_url_2() {
        let ps = vec![
            client_profile!("2", "my-cluster-1"),
            client_profile!("3", "my-cluster-1"),
            client_profile!("4", "my-cluster-1"),
        ];
        let leftovers_expected = vec!["my-cluster-1-3", "my-cluster-1-4"];
        if let (ClientProfileType::ExistingClientProfile(p), Some(pss)) =
            get_or_create_client_profile_url("my-cluster-1", &ps)
        {
            assert!(p.name.starts_with("my-cluster-1-2"));
            let a: Vec<&String> = pss.iter().map(|p| &p.name).collect();
            assert!(
                a == leftovers_expected,
                "expected {:?}, got {:?}",
                leftovers_expected,
                a
            );
        } else {
            assert!(
                false,
                "Not a ExistingClientProfile with leftovers {:?}",
                leftovers_expected
            );
        }
    }

    #[test]
    fn test_get_or_create_client_profile_url_3() {
        let ps = vec![
            client_profile!("2", "my-cluster-2"),
            client_profile!("3", "my-cluster-3"),
            client_profile!("4", "my-cluster-4"),
        ];
        if let (ClientProfileType::FreshClientProfile(p), None) =
            get_or_create_client_profile_url("my-cluster-1", &ps)
        {
            assert!(p.name.starts_with("my-cluster-1"));
        } else {
            assert!(false, "Not a FreshClientProfile without leftovers");
        }
    }

    #[test]
    fn test_get_or_create_client_profile_url_4() {
        let ps = vec![
            client_profile!("1", "my-cluster-1"),
            client_profile!("2", "my-cluster-2"),
            client_profile!("3", "my-cluster-3"),
            client_profile!("4", "my-cluster-4"),
        ];
        if let (ClientProfileType::ExistingClientProfile(p), None) =
            get_or_create_client_profile_url("my-cluster-1", &ps)
        {
            assert!(p.name.starts_with("my-cluster-1-1"));
        } else {
            assert!(false, "Not a ExistingClientProfile without leftovers");
        }
    }
}
