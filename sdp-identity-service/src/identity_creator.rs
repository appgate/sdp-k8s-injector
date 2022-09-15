use std::collections::HashMap;

use k8s_openapi::api::apps::v1::Deployment;
use k8s_openapi::api::core::v1::Secret;
use kube::{Api, Client};
use log::{error, info, warn};
use sdp_common::constants::{CLIENT_PROFILE_TAG, SDP_IDP_NAME, SERVICE_NAME};
use sdp_common::sdp::auth::SDPUser;
use sdp_common::sdp::system::{ClientProfile, ClientProfileUrl, System};
use sdp_common::service::ServiceUser;
use tokio::sync::mpsc::{Receiver, Sender};

use crate::errors::IdentityServiceError;
use crate::identity_manager::{IdentityManagerProtocol, ServiceIdentity};

#[derive(Debug)]
pub enum IdentityCreatorProtocol {
    StartService,
    CreateIdentity,
    // service user, service namespace, service name, labels
    ActivateServiceIdentity(ServiceUser, String, String, HashMap<String, String>),
    // service user, service namespace, service name
    DeleteIdentity(ServiceUser, String, String),
}

pub struct IdentityCreator {
    system: System,
    client: Client,
    service_users_pool_size: usize,
}

async fn get_or_create_client_profile_url(
    system: &mut System,
) -> Result<ClientProfileUrl, IdentityServiceError> {
    // Create ClientProfile if needed
    let ps = system
        .get_client_profiles(Some(CLIENT_PROFILE_TAG))
        .await
        .map_err(|e| {
            IdentityServiceError::from_service(
                format!("Unable to get client profiles: {}", e.to_string()),
                "IdentityCreator".to_string(),
            )
        })?;
    let psn = ps.len();
    let mut p: ClientProfile;
    if psn > 0 {
        p = ps[0].clone();
        if psn > 1 {
            warn!(
                "Seems there are defined more than one service client profile with the tag {}",
                CLIENT_PROFILE_TAG
            );
            warn!("First one found will be used: {}", p.name);
        }
    } else {
        let profile_name = "K8S Service Profile".to_string();
        let spa_key_name = profile_name.replace(" ", "").to_lowercase();
        p = ClientProfile {
            id: uuid::Uuid::new_v4().to_string(),
            name: profile_name,
            spa_key_name: spa_key_name,
            identity_provider_name: SDP_IDP_NAME.to_string(),
            tags: vec![CLIENT_PROFILE_TAG.to_string()],
        };
        p = system.create_client_profile(&p).await.map_err(|e| {
            IdentityServiceError::from_service(
                format!("Unable to create a new client profile: {}", e),
                "IdentityCreator".to_string(),
            )
        })?;
    }
    system.get_profile_client_url(&p.id).await.map_err(|e| {
        IdentityServiceError::from_service(
            format!(
                "Unable to get the client profile url for client profile {}: {}",
                p.name,
                e.to_string()
            ),
            "IdentityCreator".to_string(),
        )
    })
}

impl IdentityCreator {
    pub fn new(system: System, client: Client, credentials_pool_size: usize) -> IdentityCreator {
        IdentityCreator {
            system,
            client: client,
            service_users_pool_size: credentials_pool_size,
        }
    }

    // TODO: We should avoid to clone this client all the time
    fn secrets_api(&self, service_ns: &str) -> Api<Secret> {
        Api::namespaced(self.client.clone(), service_ns)
    }

    async fn create_user(&mut self) -> Result<ServiceUser, IdentityServiceError> {
        let service_user = SDPUser::new();
        let profile_url = get_or_create_client_profile_url(&mut self.system).await?;
        info!("Creating ServiceUser with id {}", service_user.id);
        self.system
            .create_user(&service_user)
            .await
            .map_err(|e| {
                IdentityServiceError::from_service(e.to_string(), SERVICE_NAME.to_string())
            })
            .map(|u| ServiceUser::from((&u, &profile_url)))
    }

    async fn delete_user(
        &mut self,
        service_user: &ServiceUser,
        service_ns: &str,
        service_name: &str,
    ) -> Result<(), IdentityServiceError> {
        info!("Deleting SDPUser with name {}", &service_user.name);
        self.system
            .delete_user(&service_user.name)
            .await
            .map_err(|e| {
                IdentityServiceError::from_service(e.to_string(), SERVICE_NAME.to_string())
            })?;
        service_user
            .delete(self.secrets_api(service_ns))
            .await
            .map_err(|e| {
                IdentityServiceError::from_service(
                    format!(
                        "Unable to delete ServiceUser witn name {} [{}/{}]: {}",
                        service_user.name,
                        service_ns,
                        service_name,
                        e.to_string()
                    ),
                    "IdentityCreator".to_string(),
                )
            })?;
        Ok(())
    }

    pub async fn initialize(
        &mut self,
        system: &mut System,
        identity_manager_proto_tx: Sender<IdentityManagerProtocol<Deployment, ServiceIdentity>>,
    ) -> Result<ClientProfileUrl, IdentityServiceError> {
        let users = system.get_users().await.map_err(|e| {
            IdentityServiceError::new(e.to_string(), Some("IdentityCreator".to_string()))
        })?;
        let client_profile_url = get_or_create_client_profile_url(system).await?;
        // Notify ServiceIdentityManager about the actual credentials created in appgate
        let mut n_missing_users = self.service_users_pool_size;
        // This could be actived credentials or deactivated ones.
        for sdp_user in users {
            // Derive now our ServiceUser from SDPUser
            let service_user = ServiceUser::from((&sdp_user, &client_profile_url));
            let activated = !sdp_user.disabled;
            if activated {
                n_missing_users -= 1;
            }
            let msg = IdentityManagerProtocol::FoundUserCredentials(service_user, activated);
            identity_manager_proto_tx.send(msg).await?;
        }
        // When recovering users from controller we never get the password so if for some reason it's not
        // saved in the cluster we can not recover that user.
        // When this happens we need to delete the user (from appgate and whatever info we have about it
        // in the cluster)
        /*
        if !passwd_field_exists {
            info!(
                "ServiceUser {} [{}] missing password field in storage, deleting it.",
                user.name, user.id
            );
            if let Err(err) = self
                .delete_user(
                    system,
                    &user.id,
                    user_field_exists,
                    passwd_field_exists,
                    url_field_exists,
                )
                .await
            {
                error!(
                    "Error removing ServiceUser with id {} [{}]: {}",
                    user.name, user.id, err
                );
            }
        } else {
            let service_credentials_ref = self
                .create_user_credentials_ref(
                    &user,
                    &client_profile_url,
                    user_field_exists,
                    passwd_field_exists,
                    url_field_exists,
                )
                .await
                .unwrap();
            if user.disabled {
                info!("Found a fresh service user with id {}, using it", &user.id);
            } else {
                info!(
                    "Found an already activated service user with id {}, using it",
                    &user.id
                );
            }
            */
        // Create needed credentials until we reach the desired number of credentials pool
        info!("Creating {} ServiceUsers in system", n_missing_users);
        for _i in 0..n_missing_users {
            let service_user = self.create_user().await?;
            info!(
                "New ServiceUser with name {} created, notifying IdentityManager",
                service_user.name
            );
            identity_manager_proto_tx
                .send(IdentityManagerProtocol::FoundUserCredentials(
                    service_user,
                    false,
                ))
                .await
                .map_err(|e| {
                    IdentityServiceError::new(e.to_string(), Some("IdentityCreator".to_string()))
                })?;
        }
        Ok(client_profile_url)
    }

    pub async fn run(
        mut self,
        system: &mut System,
        mut identity_creator_proto_rx: Receiver<IdentityCreatorProtocol>,
        identity_manager_proto_tx: Sender<IdentityManagerProtocol<Deployment, ServiceIdentity>>,
    ) -> () {
        info!("Starting dormant Identity Creator service, waiting commands from Identity Manager service");
        while let Some(msg) = identity_creator_proto_rx.recv().await {
            match msg {
                IdentityCreatorProtocol::StartService => {
                    info!("Identity Creator awake! Ready to process messages");
                    break;
                }
                msg => warn!(
                    "IdentityCreator is still dormant, ignoring message {:?}",
                    msg
                ),
            }
        }
        info!("Intializing IdentityCreator");
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
                                "New ServiceUser with name {} created, notifying IdentityManager",
                                service_user.name
                            );
                            let msg =
                                IdentityManagerProtocol::FoundUserCredentials(service_user, false);
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
                IdentityCreatorProtocol::DeleteIdentity(service_user, service_ns, service_name) => {
                    info!(
                        "Deleting ServiceUser with name {} [{}/{}]",
                        service_user.name, service_ns, service_name
                    );

                    if let Err(err) = self
                        .delete_user(&service_user, &service_ns, &service_name)
                        .await
                    {
                        error!(
                            "Error deleting ServiceUser/UserCredentials with id {} [{}/{}]: {}",
                            service_user.name, service_ns, service_name, err
                        )
                    }
                }
                IdentityCreatorProtocol::ActivateServiceIdentity(
                    service_user,
                    service_ns,
                    service_name,
                    labels,
                ) => {
                    let mut sdp_user = SDPUser::from(&service_user);
                    sdp_user.disabled = false;
                    sdp_user.labels = labels;
                    info!(
                        "Activating ServiceUser with name {} [{}/{}]",
                        service_user.name, service_ns, service_name
                    );
                    if let Err(err) = system.modify_user(&sdp_user).await {
                        error!(
                            "Unable to activate ServiceUser with name {} [{}/{}]: {}",
                            service_user.name, service_ns, service_name, err
                        );
                    }
                    // Here we can create the secrets, now we not the namespace anyway
                }
                msg => warn!("Ignoring message: {:?}", msg),
            }
        }
    }
}
