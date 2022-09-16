use std::collections::HashMap;

use k8s_openapi::api::apps::v1::Deployment;
use k8s_openapi::api::core::v1::Secret;
use kube::{Api, Client};
use log::{error, info, warn};
use sdp_common::constants::{
    CLIENT_PROFILE_TAG, IDENTITY_MANAGER_SECRET_NAME, SDP_IDP_NAME, SERVICE_NAME,
};
use sdp_common::kubernetes::SDP_K8S_NAMESPACE;
use sdp_common::sdp::auth::SDPUser;
use sdp_common::sdp::system::{ClientProfile, ClientProfileUrl, System};
use sdp_common::service::ServiceUser;
use tokio::sync::mpsc::{Receiver, Sender};
use uuid::Uuid;

use crate::errors::IdentityServiceError;
use crate::identity_manager::{IdentityManagerProtocol, ServiceIdentity};

#[derive(Debug)]
pub enum IdentityCreatorProtocol {
    StartService,
    CreateIdentity,
    // service user, service namespace, service name, labels
    ActivateServiceUser(ServiceUser, String, String, HashMap<String, String>),
    // service user, service namespace, service name
    DeleteServiceUser(ServiceUser, String, String),
    // sdp user name
    DeleteSDPUser(String),
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

    async fn delete_sdp_user(&mut self, sdp_user_name: &str) -> Result<(), IdentityServiceError> {
        info!("Deleting SDPUser with name {}", &sdp_user_name);
        // Create a default SDPUser with the name of the one we want to delete
        let sdp_user = SDPUser::from_name(sdp_user_name.to_string());
        let client_profile_url = ClientProfileUrl {
            url: Uuid::new_v4().to_string(),
        };
        // Derive a ServiceUser that we can use to delete the secret fields
        if let Some(service_user) = ServiceUser::from_sdp_user(&sdp_user, &client_profile_url, None)
        {
            service_user
                .delete_fields(
                    self.secrets_api(SDP_K8S_NAMESPACE),
                    IDENTITY_MANAGER_SECRET_NAME,
                    false,
                )
                .await
                .map_err(|e| {
                    IdentityServiceError::from_service(e.to_string(), SERVICE_NAME.to_string())
                })?;
        }
        self.system.delete_user(&sdp_user_name).await.map_err(|e| {
            IdentityServiceError::from_service(e.to_string(), SERVICE_NAME.to_string())
        })
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

    async fn delete_user(
        &mut self,
        service_user: &ServiceUser,
        service_ns: &str,
        service_name: &str,
    ) -> Result<(), IdentityServiceError> {
        self.delete_sdp_user(&service_user.name).await?;
        service_user
            .delete(self.secrets_api(service_ns), service_ns, service_name)
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
            // We got a SDPUSer. We dont have any wayt to recover passwords
            // from there (SDPUsers dont contain the password when fetched from a controller)
            // IM always saves those creds for ServiceUsers that are deactivated.
            // If we dont have a password for this user, don't recover it and ask IC to delete as soon as possible.

            // Derive now our ServiceUser from SDPUser, recovering passwords if needed.
            if let Some(service_user) = self.recover_sdp_user(&sdp_user, &client_profile_url).await
            {
                let activated = !sdp_user.disabled;
                if !activated {
                    n_missing_users -= 1;
                }
                let msg = IdentityManagerProtocol::FoundServiceUser(service_user, activated);
                identity_manager_proto_tx.send(msg).await?;
            } else {
                error!(
                    "Recovering ServiceUser information from SDPUser {}. Deleting SDPUSer.",
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

        // Create needed credentials until we reach the desired number of credentials pool
        info!("Creating {} ServiceUsers in system", n_missing_users);
        for _i in 0..n_missing_users {
            let service_user = self.create_user().await?;
            info!(
                "New ServiceUser with name {} created, notifying IdentityManager",
                service_user.name
            );
            identity_manager_proto_tx
                .send(IdentityManagerProtocol::FoundServiceUser(
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
                IdentityCreatorProtocol::ActivateServiceUser(
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

                    // Create secrets now
                    if let Err(e) = service_user
                        .create(self.secrets_api(&service_ns), &service_ns, &service_name)
                        .await
                    {
                        error!(
                            "Error creating secrets for ServiceUser {} [{}/{}]: {}",
                            service_user.name,
                            service_ns,
                            service_name,
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
                msg => warn!("Ignoring message: {:?}", msg),
            }
        }
    }
}
