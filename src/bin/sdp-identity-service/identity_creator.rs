use json_patch::PatchOperation::Add;
use json_patch::{AddOperation, Patch as JsonPatch};
use k8s_openapi::api::core::v1::Secret;
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
pub struct UserCredentialsRef {
    id: String,
    secret: String,
    user_field: String,
    password_field: String,
}

impl From<&ServiceUser> for UserCredentialsRef {
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

pub enum IdentityCreatorMessage {
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
    ) -> Result<UserCredentialsRef, IdentityServiceError> {
        let pw_field = format!("{}-pw", service_user.id);
        let user_field = format!("{}-user", service_user.id);
        let json_patch_user = Add(AddOperation {
            path: format!("/data/{}", user_field),
            value: serde_json::to_value(&service_user.name).map_err(|e| {
                IdentityServiceError::from_service(e.to_string(), SERVICE_NAME.to_string())
            })?,
        });
        let json_patch_pw = Add(AddOperation {
            path: format!("/data/{}", pw_field),
            value: serde_json::to_value(&service_user.password).map_err(|e| {
                IdentityServiceError::from_service(e.to_string(), SERVICE_NAME.to_string())
            })?,
        });
        let json_patch = JsonPatch(vec![json_patch_user, json_patch_pw]);
        let patch = KubePatch::Json::<()>(json_patch);
        let _ = self
            .secrets_api
            .patch(
                SDP_IDENTITY_MANAGER_SECRETS,
                &PatchParams::default(),
                &patch,
            )
            .await
            .map_err(|e| {
                IdentityServiceError::from_service(e.to_string(), SERVICE_NAME.to_string())
            })?;
        Ok(UserCredentialsRef::from(service_user))
    }

    async fn create_user(
        &self,
        system: &mut sdp::System,
    ) -> Result<UserCredentialsRef, IdentityServiceError> {
        let service_user = system.create_user().await.map_err(|e| {
            IdentityServiceError::from_service(e.to_string(), SERVICE_NAME.to_string())
        })?;
        self.create_user_credentials_ref(&service_user).await
    }

    async fn run_identity_creator(
        self,
        system: &mut sdp::System,
        mut rx: Receiver<IdentityCreatorMessage>,
        tx: Sender<IdentityManagerProtocol>,
    ) -> () {
        while let Some(message) = rx.recv().await {
            match message {
                IdentityCreatorMessage::CreateIdentity => {
                    match self.create_user(system).await {
                        Ok(user_credentials_ref) => {
                            let msg = IdentityManagerProtocol::IdentityCreated {
                                user_credentials_ref: user_credentials_ref,
                            };
                            if let Err(err) = tx.send(msg).await {
                                error!("Error notifying identity: {}", err);
                                // TODO: Try later, identity is already created
                            }
                        }
                        Err(err) => {
                            error!("Error creating new identity: {}", err);
                        }
                    };
                }
                IdentityCreatorMessage::DeleteIdentity(id) => match system.delete_user(id).await {
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
