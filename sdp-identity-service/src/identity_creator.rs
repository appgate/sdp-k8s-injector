use std::collections::{BTreeMap, HashMap};

use json_patch::{PatchOperation, RemoveOperation};
use k8s_openapi::api::apps::v1::Deployment;
use k8s_openapi::api::core::v1::Secret;
use k8s_openapi::ByteString;
use kube::api::{Patch as KubePatch, PatchParams};
use kube::{Api, Client};
use log::{error, info, warn};
use sdp_common::constants::{SDP_IDENTITY_MANAGER_SECRETS, SERVICE_NAME};
pub use sdp_common::crd::ServiceCredentialsRef;
use sdp_common::sdp::auth::ServiceUser;
use sdp_common::sdp::system::System;
use tokio::sync::mpsc::{Receiver, Sender};

use crate::errors::IdentityServiceError;
use crate::identity_manager::{IdentityManagerProtocol, ServiceIdentity};

trait ServiceCredentialsRefOps {}

#[derive(Debug)]
pub enum IdentityCreatorProtocol {
    StartService,
    CreateIdentity,
    ModifyIdentity {
        service_credentials: ServiceCredentialsRef,
        name: String,
        labels: HashMap<String, String>,
        active: bool,
    },
    DeleteIdentity(String),
}

pub struct IdentityCreator {
    secrets_api: Api<Secret>,
    credentials_pool_size: usize,
}

impl IdentityCreator {
    pub fn new(client: Client, credentials_pool_size: usize) -> IdentityCreator {
        let secrets_api: Api<Secret> = Api::namespaced(client, "sdp-system");
        IdentityCreator {
            secrets_api,
            credentials_pool_size,
        }
    }

    async fn exists_user_crendentials_ref(&self, service_user_id: &String) -> (bool, bool) {
        let pw_field = format!("{}-pw", service_user_id);
        let user_field = format!("{}-user", service_user_id);
        if let Ok(secret) = self.secrets_api.get(SDP_IDENTITY_MANAGER_SECRETS).await {
            secret
                .data
                .map(|data| {
                    (
                        data.get(&pw_field).is_some(),
                        data.get(&user_field).is_some(),
                    )
                })
                .unwrap_or((false, false))
        } else {
            error!(
                "Error getting UserCredentialRef with id {}",
                service_user_id
            );
            (false, false)
        }
    }

    async fn delete_user_credentials_ref(
        &self,
        service_user_id: &String,
        user_field_exists: bool,
        passwd_field_exists: bool,
    ) -> Result<(), IdentityServiceError> {
        let pw_field = format!("{}-pw", service_user_id);
        let user_field = format!("{}-user", service_user_id);
        let mut patch_operations: Vec<PatchOperation> = Vec::new();
        if user_field_exists {
            patch_operations.push(PatchOperation::Remove(RemoveOperation {
                path: format!("/data/{}", user_field),
            }));
        } else {
            info!(
                "User field in UserCredentials {} not found, ignoring it",
                service_user_id
            );
        }
        if passwd_field_exists {
            patch_operations.push(PatchOperation::Remove(RemoveOperation {
                path: format!("/data/{}", pw_field),
            }));
        } else {
            info!(
                "Password field in UserCredentials{} not found, ignoring it",
                service_user_id
            );
        }
        if patch_operations.len() > 0 {
            info!(
                "Deleting UserCredentials {} from K8S secret",
                service_user_id
            );
            let patch: KubePatch<Secret> = KubePatch::Json(json_patch::Patch(patch_operations));
            let _ = self
                .secrets_api
                .patch(
                    SDP_IDENTITY_MANAGER_SECRETS,
                    &PatchParams::default(),
                    &patch,
                )
                .await
                .map_err(|e| {
                    IdentityServiceError::from_service(
                        format!("Error patching secret: {}", e),
                        SERVICE_NAME.to_string(),
                    )
                })?;
        }
        Ok(())
    }

    async fn create_user_credentials_ref(
        &self,
        service_user: &ServiceUser,
        user_field_exists: bool,
        passwd_field_exists: bool,
    ) -> Result<ServiceCredentialsRef, IdentityServiceError> {
        let pw_field = format!("{}-pw", service_user.id);
        let user_field = format!("{}-user", service_user.id);
        let mut secret = Secret::default();
        let mut data = BTreeMap::new();
        if !user_field_exists {
            info!(
                "Create user entry in UserCredentials for ServiceUser {}",
                service_user.id
            );
            data.insert(
                user_field,
                ByteString(service_user.name.as_bytes().to_vec()),
            );
        }
        if !passwd_field_exists {
            info!(
                "Create password entry in UserCredentials for ServiceUser {}",
                service_user.id
            );
            let password = service_user
                .password
                .as_ref()
                .ok_or(IdentityServiceError::new(
                    format!(
                        "Password in ServiceUser with id {} not defined",
                        service_user.id
                    ),
                    Some("IdentityCreator".to_string()),
                ))?;
            data.insert(pw_field, ByteString(password.as_bytes().to_vec()));
        }
        if data.len() > 0 {
            secret.data = Some(data);
            info!(
                "Creating UserCredentials in K8S secret for ServiceUer: {}",
                service_user.id
            );
            let patch = KubePatch::Merge(secret);
            let _ = self
                .secrets_api
                .patch(
                    SDP_IDENTITY_MANAGER_SECRETS,
                    &PatchParams::default(),
                    &patch,
                )
                .await
                .map_err(|e| {
                    IdentityServiceError::from_service(
                        format!("Error patching secret: {}", e),
                        SERVICE_NAME.to_string(),
                    )
                })?;
        }
        Ok(ServiceCredentialsRef::from(service_user))
    }

    async fn create_user(
        &self,
        system: &mut System,
    ) -> Result<ServiceCredentialsRef, IdentityServiceError> {
        let service_user = ServiceUser::new();
        let (user_field_exists, passwd_field_exists) =
            self.exists_user_crendentials_ref(&service_user.id).await;
        info!("Creating ServiceUser with id {}", service_user.id);
        let _ = system.create_user(&service_user).await.map_err(|e| {
            IdentityServiceError::from_service(e.to_string(), SERVICE_NAME.to_string())
        })?;
        self.create_user_credentials_ref(&service_user, user_field_exists, passwd_field_exists)
            .await
    }

    async fn delete_user(
        &self,
        system: &mut System,
        service_user_id: &String,
        user_field_exists: bool,
        passwd_field_exists: bool,
    ) -> Result<(), IdentityServiceError> {
        info!("Deleting ServiceUser with id {}", service_user_id);
        let _ = system
            .delete_user(service_user_id.clone())
            .await
            .map_err(|e| {
                IdentityServiceError::from_service(e.to_string(), SERVICE_NAME.to_string())
            })?;
        self.delete_user_credentials_ref(service_user_id, user_field_exists, passwd_field_exists)
            .await
    }

    pub async fn initialize(
        &self,
        system: &mut System,
        identity_manager_proto_tx: Sender<IdentityManagerProtocol<Deployment, ServiceIdentity>>,
    ) -> Result<(), IdentityServiceError> {
        let users = system.get_users().await.map_err(|e| {
            IdentityServiceError::new(e.to_string(), Some("IdentityCreator".to_string()))
        })?;
        let n_users = users.iter().filter(|u| u.disabled).count();
        let mut n_missing_users = 0;
        if n_users <= self.credentials_pool_size {
            n_missing_users = self.credentials_pool_size - n_users;
        }
        // Notify ServiceIdentityManager about the actual credentials created in appgate
        // This could be actived credentials or deactivated ones.
        for user in users {
            let (user_field_exists, passwd_field_exists) =
                self.exists_user_crendentials_ref(&user.id).await;

            // When recovering users from controller we never get the password so if for some reason it's not
            // saved in the cluster we can not recover that user.
            // When this happens we need to delete the user (from appgate and whatever info we have about it
            // in the cluster)
            if !passwd_field_exists {
                info!(
                    "ServiceUser {} [{}] missing password field in storage, deleting it.",
                    user.name, user.id
                );
                if let Err(err) = self
                    .delete_user(system, &user.id, user_field_exists, passwd_field_exists)
                    .await
                {
                    error!(
                        "Error removing ServiceUser with id {} [{}]: {}",
                        user.name, user.id, err
                    );
                }
            } else {
                let service_credentials_ref = self
                    .create_user_credentials_ref(&user, user_field_exists, passwd_field_exists)
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
                let msg = IdentityManagerProtocol::FoundUserCredentials {
                    user_credentials_ref: service_credentials_ref,
                    activated: !user.disabled,
                };
                identity_manager_proto_tx.send(msg).await?;
            }
        }

        // Create needed credentials until we reach the desired number of credentials pool
        info!("Creating {} credentials in system", n_missing_users);
        for _i in 0..n_missing_users {
            let service_credentials_ref = self.create_user(system).await?;
            info!(
                "New credentials with id {} created, notifying IdentityManager",
                service_credentials_ref.id
            );
            identity_manager_proto_tx
                .send(IdentityManagerProtocol::FoundUserCredentials {
                    user_credentials_ref: service_credentials_ref,
                    activated: false,
                })
                .await
                .map_err(|e| {
                    IdentityServiceError::new(e.to_string(), Some("IdentityCreator".to_string()))
                })?;
        }
        Ok(())
    }

    pub async fn run(
        self,
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
        self.initialize(system, identity_manager_proto_tx.clone())
            .await
            .expect("Error while initializing IdentityCreator");

        // Notify IdentityManager that we are ready
        identity_manager_proto_tx
            .send(IdentityManagerProtocol::IdentityCreatorReady)
            .await
            .expect("Unable to notify IdentityManager");

        while let Some(message) = identity_creator_proto_rx.recv().await {
            match message {
                IdentityCreatorProtocol::CreateIdentity => {
                    match self.create_user(system).await {
                        Ok(user_credentials_ref) => {
                            info!(
                                "New credentials with id {} created, notifying IdentityManager",
                                user_credentials_ref.id
                            );
                            let msg = IdentityManagerProtocol::FoundUserCredentials {
                                user_credentials_ref: user_credentials_ref,
                                activated: false,
                            };
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
                IdentityCreatorProtocol::DeleteIdentity(identity_id) => {
                    info!(
                        "Deleting ServiceUser/UserCredentials with id {}",
                        identity_id
                    );
                    let (user_field_exists, passwd_field_exists) =
                        self.exists_user_crendentials_ref(&identity_id).await;
                    if let Err(err) = self
                        .delete_user(system, &identity_id, user_field_exists, passwd_field_exists)
                        .await
                    {
                        error!(
                            "Error deleting ServiceUser/UserCredentials with id {}: {}",
                            identity_id, err
                        )
                    }
                }
                IdentityCreatorProtocol::ModifyIdentity {
                    service_credentials,
                    name,
                    labels,
                    active,
                } => {
                    let mut service_user = ServiceUser::from(service_credentials);
                    service_user.name = name;
                    service_user.labels = labels;
                    service_user.disabled = !active;
                    if let Err(err) = system.modify_user(&service_user).await {
                        error!(
                            "Unable to modify ServiceUser with id {} [active: {}]: {}",
                            service_user.id, active, err
                        );
                    }
                }
                msg => warn!("Ignoring message: {:?}", msg),
            }
        }
    }
}
