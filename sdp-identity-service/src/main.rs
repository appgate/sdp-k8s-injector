use clap::{Parser, Subcommand};
use errors::IdentityServiceError;
use k8s_openapi::api::apps::v1::Deployment;
use kube::CustomResourceExt;
use log::info;
use reqwest::Url;
use sdp_common::crd::ServiceIdentity;
use sdp_common::kubernetes::{
    get_k8s_client, SDP_K8S_PASSWORD_DEFAULT, SDP_K8S_PASSWORD_ENV, SDP_K8S_PROVIDER_DEFAULT,
    SDP_K8S_PROVIDER_ENV, SDP_K8S_USERNAME_DEFAULT, SDP_K8S_USERNAME_ENV,
};
use sdp_common::sdp::auth::Credentials;
use sdp_common::sdp::system::SystemConfig;
use tokio::sync::mpsc::channel;

use crate::{
    deployment_watcher::{DeploymentWatcher, DeploymentWatcherProtocol},
    identity_creator::{IdentityCreator, IdentityCreatorProtocol},
    identity_manager::{IdentityManagerProtocol, IdentityManagerRunner},
};

pub mod deployment_watcher;
pub mod errors;
pub mod identity_creator;
pub mod identity_manager;

const SDP_SYSTEM_HOSTS: &str = "SDP_SYSTEM_HOSTS";
const CREDENTIALS_POOL_SIZE: usize = 10;

fn get_system_hosts() -> Result<Vec<Url>, IdentityServiceError> {
    std::env::var(SDP_SYSTEM_HOSTS)
        .map_err(|_| IdentityServiceError::new("Unable to find SDP system hosts".to_string(), None))
        .map(|vs| {
            vs.split(",")
                .map(|v| Url::parse(v).expect("Error parsing host"))
                .collect::<Vec<Url>>()
        })
}

fn show_crds() {
    let crd = ServiceIdentity::crd();
    let p = serde_json::to_string(&crd);
    println!("{}", p.unwrap());
}

#[derive(Debug, Subcommand)]
enum IdentityServiceCommands {
    /// Prints the ServiceIdentity CustomResourceDefinition YAML
    Crd,
    /// Runs the Identity Service
    Run,
}

#[derive(Debug, Parser)]
#[clap(name = "sdp-identity-service")]
struct IdentityService {
    #[clap(subcommand)]
    command: IdentityServiceCommands,
}

#[tokio::main]
async fn main() -> () {
    env_logger::init();
    let args = IdentityService::parse();
    match args.command {
        IdentityServiceCommands::Crd => {
            show_crds();
        }
        IdentityServiceCommands::Run => {
            info!("Starting sdp identity service ...");
            let identity_manager_client = get_k8s_client().await;
            let deployment_watcher_client = get_k8s_client().await;
            let identity_creator_client = get_k8s_client().await;
            let identity_manager_runner =
                IdentityManagerRunner::kube_runner(identity_manager_client);
            let identity_creator =
                IdentityCreator::new(identity_creator_client, CREDENTIALS_POOL_SIZE);
            let (identity_manager_proto_tx, identity_manager_proto_rx) =
                channel::<IdentityManagerProtocol<Deployment, ServiceIdentity>>(50);
            let identity_manager_proto_tx_cp = identity_manager_proto_tx.clone();
            let identity_manager_proto_tx_cp2 = identity_manager_proto_tx.clone();
            let (identity_creator_proto_tx, identity_creator_proto_rx) =
                channel::<IdentityCreatorProtocol>(50);
            let (deployment_watched_proto_tx, deployment_watcher_proto_rx) =
                channel::<DeploymentWatcherProtocol>(50);
            let hosts = get_system_hosts().expect("Unable to get SDP system hosts");
            tokio::spawn(async {
                let deployment_watcher = DeploymentWatcher::new(deployment_watcher_client);
                deployment_watcher
                    .run(deployment_watcher_proto_rx, identity_manager_proto_tx)
                    .await;
            });
            tokio::spawn(async move {
                let credentials = Credentials {
                    username: std::env::var(SDP_K8S_USERNAME_ENV)
                        .unwrap_or(SDP_K8S_USERNAME_DEFAULT.to_string()),
                    password: std::env::var(SDP_K8S_PASSWORD_ENV)
                        .unwrap_or(SDP_K8S_PASSWORD_DEFAULT.to_string()),
                    provider_name: std::env::var(SDP_K8S_PROVIDER_ENV)
                        .unwrap_or(SDP_K8S_PROVIDER_DEFAULT.to_string()),
                    device_id: uuid::Uuid::new_v4().to_string(),
                };
                let mut system = SystemConfig::new(hosts)
                    .with_api_version("17")
                    .build(credentials)
                    .expect("Unable to create SDP client");
                identity_creator
                    .run(
                        &mut system,
                        identity_creator_proto_rx,
                        identity_manager_proto_tx_cp,
                    )
                    .await;
            });
            identity_manager_runner
                .run(
                    identity_manager_proto_rx,
                    identity_manager_proto_tx_cp2,
                    identity_creator_proto_tx,
                    deployment_watched_proto_tx,
                    None,
                )
                .await;
        }
    }
}
