use futures::StreamExt;
use json_patch::PatchOperation::Add;
use json_patch::{AddOperation, Patch as JsonPatch};
use k8s_openapi::api::{apps::v1::Deployment, core::v1::Secret};
use kube::api::{ListParams, Patch as KubePatch, PatchParams, PostParams};
use kube::runtime::watcher::{self, Event};
use kube::{Api, Client, CustomResource, ResourceExt};
use log::{error, info};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, VecDeque};
use std::fmt::Display;
use tokio::sync::mpsc::{Receiver, Sender};

pub use crate::sdp;

use self::sdp::ServiceUser;

const SDP_IDENTITY_MANAGER_SECRETS: &str = "sdp-identity-service-creds";

#[derive(Debug)]
struct IdentityManagerError {
    error: String,
}

impl IdentityManagerError {
    fn new(error: String) -> Self {
        IdentityManagerError { error: error }
    }
}

impl Display for IdentityManagerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "IdentityManager error: {}", self.error)
    }
}

#[derive(CustomResource, Serialize, Deserialize, Clone, Debug, JsonSchema)]
#[kube(
    group = "injector.sdp.com",
    version = "v1",
    kind = "ServiceIdentity",
    namespaced
)]
pub struct ServiceIdentitySpec {
    service_identity: UserCredentialsRef,
    service_name: String,
    service_namespace: String,
    labels: Vec<String>,
    disabled: bool,
}

pub trait ServiceCandidate {
    fn service_id(&self) -> String;
}

fn derive_service_id(name: &str, namespace: &str) -> String {
    format!("{}-{}", namespace, name).to_string()
}

impl ServiceCandidate for ServiceIdentity {
    fn service_id(&self) -> String {
        derive_service_id(&self.spec.service_namespace, &self.spec.service_name)
    }
}

impl ServiceCandidate for Deployment {
    fn service_id(&self) -> String {
        let name = self.name();
        let ns = self.namespace().unwrap_or("default".to_string());
        derive_service_id(&name, &ns)
    }
}

#[derive(Clone, JsonSchema, Debug, Serialize, Deserialize)]
pub struct UserCredentialsRef {
    id: String,
    secret: String,
    user_field: String,
    password_field: String,
}

/// Messages exchanged between different components
pub enum IdentityManagerProtocol {
    /// Message used to request a new user
    RequestIdentity {
        service_name: String,
        service_ns: String,
    },
    IdentityCreated {
        id: String,
        user_credentials_ref: UserCredentialsRef,
    },
}

pub enum IdentityMessageResponse {
    /// Message used to send a new user
    NewIdentity(UserCredentialsRef),
    IdentityUnavailable,
}

pub enum IdentityCreatorMessage {
    /// Message used to send a new user
    CreateIdentity,
    DeleteIdentity,
}

pub struct IdentityManager {
    pool: VecDeque<UserCredentialsRef>,
    service_identity_api: Api<ServiceIdentity>,
    secrets_api: Api<Secret>,
    services: HashMap<String, ServiceIdentity>,
}

impl IdentityManager {
    pub fn new(client: Client) -> IdentityManager {
        let secret_client = client.clone();
        let service_identity_api: Api<ServiceIdentity> = Api::namespaced(client, "sdp-system");
        let secrets_api: Api<Secret> = Api::namespaced(secret_client, "sdp-system");
        IdentityManager {
            pool: VecDeque::with_capacity(30),
            service_identity_api: service_identity_api,
            secrets_api: secrets_api,
            services: HashMap::new(),
        }
    }

    fn next_identity(&mut self, name: &String, namespace: &String) -> Option<ServiceIdentity> {
        let service_id = derive_service_id(name, namespace);
        match self.services.get(&service_id) {
            None => self.pool.pop_front().map(|service_identity| {
                let service_identity_spec = ServiceIdentitySpec {
                    service_name: name.clone(),
                    service_namespace: namespace.clone(),
                    service_identity: service_identity,
                    labels: vec![],
                    disabled: false,
                };
                ServiceIdentity::new(&service_id, service_identity_spec)
            }),
            Some(c) => None,
        }
    }

    async fn run_identity_manager(
        &mut self,
        mut receiver: Receiver<IdentityManagerProtocol>,
    ) -> () {
        info!("Running Identity Manager ...");
        while let Some(msg) = receiver.recv().await {
            match msg {
                IdentityManagerProtocol::RequestIdentity {
                    service_name,
                    service_ns,
                } => {
                    let service_id = derive_service_id(&service_name, &service_ns);
                    info!("New user requested for service {}", service_id);
                    match self.next_identity(&service_name, &service_ns) {
                        Some(identity) => {
                            match self
                                .service_identity_api
                                .create(&PostParams::default(), &identity)
                                .await
                            {
                                Ok(_) => {
                                    info!(
                                        "New ServiceIdentity created for service with id {}",
                                        service_id
                                    );
                                }
                                Err(err) => {
                                    error!(
                                        "Error creating ServiceIdentity for service with id {}: {}",
                                        service_id, err
                                    );
                                }
                            }
                        }
                        None => {
                            error!("Unable to assign service identity for service {}. Identities pool seems to be empty!", service_id);
                        }
                    };
                }
                IdentityManagerProtocol::IdentityCreated {
                    id,
                    user_credentials_ref,
                } => {
                    self.pool.push_back(UserCredentialsRef {
                        id: id,
                        secret: "THESECRET".to_string(),
                        user_field: "THEFIELD".to_string(),
                        password_field: "TEHFIELD".to_string(),
                    });
                }
            }
        }
    }

    async fn initialize(&mut self) -> () {
        info!("Initializing Identity Manager ...");
        match self.service_identity_api.list(&ListParams::default()).await {
            Ok(xs) => {
                info!("Restoring previous service identities");
                let n: u32 = xs
                    .iter()
                    .map(|s| {
                        info!("Restoring service identity {}", s.service_id());
                        self.services.insert(s.name(), s.clone());
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

    async fn create_user_credentials_ref(
        &self,
        service_user: &ServiceUser,
    ) -> Result<UserCredentialsRef, IdentityManagerError> {
        let pw_field = format!("{}-pw", service_user.id);
        let user_field = format!("{}-user", service_user.id);
        let json_patch_user = Add(AddOperation {
            path: format!("/data/{}", user_field),
            value: serde_json::to_value(&service_user.name)
                .map_err(|e| IdentityManagerError::new(e.to_string()))?,
        });
        let json_patch_pw = Add(AddOperation {
            path: format!("/data/{}", pw_field),
            value: serde_json::to_value(&service_user.password)
                .map_err(|e| IdentityManagerError::new(e.to_string()))?,
        });
        let json_patch = JsonPatch(vec![json_patch_user, json_patch_pw]);
        let patch = KubePatch::Json::<()>(json_patch);
        let secret = self
            .secrets_api
            .patch(
                SDP_IDENTITY_MANAGER_SECRETS,
                &PatchParams::default(),
                &patch,
            )
            .await.map_err(|e| IdentityManagerError::new(e.to_string()))?;
        Ok(UserCredentialsRef {
            id: service_user.id.clone(),
            secret: SDP_IDENTITY_MANAGER_SECRETS.to_string(),
            user_field: user_field,
            password_field: pw_field,
        })
    }

    async fn run_identity_creator(
        self,
        mut system: sdp::System,
        mut rx: Receiver<IdentityCreatorMessage>,
        tx: Sender<IdentityManagerProtocol>,
    ) -> () {
        while let Some(message) = rx.recv().await {
            match message {
                IdentityCreatorMessage::CreateIdentity => {
                    let service_user = &system.create_user().await.expect("ERROR");
                    let user_credentials_ref = self
                        .create_user_credentials_ref(service_user)
                        .await
                        .expect("SDD");
                    match system.create_user().await {
                        Ok(identity) => {
                            let msg = IdentityManagerProtocol::IdentityCreated {
                                id: service_user.id.clone(),
                                user_credentials_ref: user_credentials_ref,
                            };
                            if let Err(err) = tx.send(msg).await {
                                error!("Error notifying identity: {}", err);
                                // TODO: Try later, identity are already created
                            }
                        }
                        Err(err) => {
                            error!("Error creating new identity: {}", err);
                        }
                    };
                }
                IdentityCreatorMessage::DeleteIdentity => {
                    match system.delete_user("ID HERE".to_string()).await {
                        Ok(_identity) => {
                            info!("identity deleted!");
                        }
                        Err(err) => {
                            error!("Unable to delete identity: {}", err);
                        }
                    }
                }
            }
        }
    }

    pub async fn run(&mut self, receiver: Receiver<IdentityManagerProtocol>) -> () {
        self.initialize().await;
        self.run_identity_manager(receiver).await;
    }
}

pub struct DeploymentWatcher {
    deployment_api: Api<Deployment>,
}

impl<'a> DeploymentWatcher {
    pub fn new(client: Client) -> Self {
        let deployment_api: Api<Deployment> = Api::all(client);
        DeploymentWatcher {
            deployment_api: deployment_api,
        }
    }

    pub async fn watch_deployments(self, queue: Sender<IdentityManagerProtocol>) -> () {
        info!("Starting Deployments watcher!");
        let tx = &queue;
        watcher::watcher(self.deployment_api, ListParams::default())
            .for_each_concurrent(5, |res| async move {
                match res {
                    Ok(Event::Restarted(deployments)) => {
                        for deployment in deployments {
                            info!("Found new service candidate: {}", deployment.service_id());
                            if let Err(err) = tx
                                .send(IdentityManagerProtocol::RequestIdentity {
                                    service_name: deployment.name(),
                                    service_ns: deployment.namespace().unwrap(),
                                })
                                .await
                            {
                                error!("Error requesting new ServiceIdentity")
                            }
                        }
                    }
                    Ok(Event::Applied(deployment)) => {
                        info!("Found new service candidate: {}", deployment.service_id());
                        if let Err(err) = tx
                            .send(IdentityManagerProtocol::RequestIdentity {
                                service_name: deployment.name(),
                                service_ns: deployment.namespace().unwrap(),
                            })
                            .await
                        {
                            error!("Error requesting new ServiceIdentity")
                        }
                    }
                    Ok(Event::Deleted(deployment)) => {
                        info!("Deleted service candidate {}", deployment.service_id());
                    }
                    Err(err) => {
                        error!("Some error: {}", err);
                    }
                }
            })
            .await
    }
}
