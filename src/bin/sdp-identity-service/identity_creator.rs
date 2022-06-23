use std::collections::BTreeMap;

use k8s_openapi::api::apps::v1::Deployment;
use k8s_openapi::api::core::v1::Secret;
use k8s_openapi::ByteString;
use kube::api::{Patch as KubePatch, PatchParams};
use kube::{Api, Client};
use log::{error, info};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc::{Receiver, Sender};

use crate::errors::IdentityServiceError;
use crate::{
    identity_manager::IdentityManagerProtocol,
    sdp::{self, ServiceUser},
};

const SDP_IDENTITY_MANAGER_SECRETS: &str = "sdp-identity-service-creds";
const SERVICE_NAME: &str = "identity-creator";

#[derive(Clone, JsonSchema, Debug, Serialize, Deserialize)]
pub struct ServiceCredentialsRef {
    pub id: String,
    pub secret: String,
    pub user_field: String,
    pub password_field: String,
}

impl From<&ServiceUser> for ServiceCredentialsRef {
    fn from(service_user: &ServiceUser) -> Self {
        let pw_field = format!("{}-pw", service_user.id);
        let user_field = format!("{}-user", service_user.id);
        Self {
            id: service_user.id.clone(),
            secret: SDP_IDENTITY_MANAGER_SECRETS.to_string(),
            user_field: user_field,
            password_field: pw_field,
        }
    }
}

pub enum IdentityCreatorProtocol {
    /// Message used to send a new user
    CreateIdentity,
    DeleteIdentity(String),
}

pub struct IdentityCreator {
    secrets_api: Api<Secret>,
}

impl IdentityCreator {
    pub fn new(client: Client) -> IdentityCreator {
        let secrets_api: Api<Secret> = Api::namespaced(client, "sdp-system");
        IdentityCreator {
            secrets_api: secrets_api,
        }
    }

    async fn create_user_credentials_ref(
        &self,
        service_user: &ServiceUser,
    ) -> Result<ServiceCredentialsRef, IdentityServiceError> {
        let pw_field = format!("{}-pw", service_user.id);
        let user_field = format!("{}-user", service_user.id);
        let mut secret = Secret::default();
        secret.data = Some(BTreeMap::from([
            (
                user_field,
                ByteString(service_user.name.as_bytes().to_vec()),
            ),
            (
                pw_field,
                ByteString(service_user.password.as_bytes().to_vec()),
            ),
        ]));
        info!(
            "Creating user credentials in K8S secret for service_user: {}",
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
        Ok(ServiceCredentialsRef::from(service_user))
    }

    async fn create_user(
        &self,
        system: &mut sdp::System,
    ) -> Result<ServiceCredentialsRef, IdentityServiceError> {
        let service_user = ServiceUser::new();
        let _ = system.create_user(&service_user).await.map_err(|e| {
            IdentityServiceError::from_service(e.to_string(), SERVICE_NAME.to_string())
        })?;
        self.create_user_credentials_ref(&service_user).await
    }

    pub async fn initialize(
        &self,
        system: &mut sdp::System,
        identity_manager_proto_tx: Sender<IdentityManagerProtocol<Deployment>>,
    ) -> Result<(), IdentityServiceError> {
        let users = system.get_users().await.map_err(|e| {
            IdentityServiceError::new(e.to_string(), Some("IdentityCreator".to_string()))
        })?;
        let n_missing_users = 10 - users.len();
        info!("Creating {} credentials in system", n_missing_users);
        for _i in 0..n_missing_users {
            let service_credentials_ref = self.create_user(system).await?;
            info!("New credentials with id {} created, notifying IdentityManager", service_credentials_ref.id);
            identity_manager_proto_tx.send(IdentityManagerProtocol::IdentityCreated {
                user_credentials_ref: service_credentials_ref,
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
        system: &mut sdp::System,
        mut identity_creator_proto_rx: Receiver<IdentityCreatorProtocol>,
        identity_manager_proto_tx: Sender<IdentityManagerProtocol<Deployment>>,
    ) -> () {
        info!("Intializing IdentityCreator");
        self.initialize(system, identity_manager_proto_tx.clone())
            .await
            .expect("Error while initializing IdentityCreator");

        // Notify IdentityManager that we are ready
        identity_manager_proto_tx.send(IdentityManagerProtocol::IdentityCreatorReady)
            .await
            .expect("Unable to notify IdentityManager");

        while let Some(message) = identity_creator_proto_rx.recv().await {
            match message {
                IdentityCreatorProtocol::CreateIdentity => {
                    match self.create_user(system).await {
                        Ok(user_credentials_ref) => {
                            info!("New credentials with id {} created, notifying IdentityManager", user_credentials_ref.id);
                            let msg = IdentityManagerProtocol::IdentityCreated {
                                user_credentials_ref: user_credentials_ref,
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
                IdentityCreatorProtocol::DeleteIdentity(id) => match system.delete_user(id).await {
                    Ok(_identity) => {
                        info!("identity deleted!");
                    }
                    Err(err) => {
                        error!("Unable to delete identity: {}", err);
                    }
                },
            }
        }
    }
}
