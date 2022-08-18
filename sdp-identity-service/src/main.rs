use clap::{Parser, Subcommand};
use k8s_openapi::api::apps::v1::Deployment;
use kube::CustomResourceExt;
use log::info;
use sdp_common::crd::ServiceIdentity;
use sdp_common::kubernetes::get_k8s_client;
use sdp_common::sdp::system::get_sdp_system;
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

const CREDENTIALS_POOL_SIZE: usize = 10;

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
            tokio::spawn(async {
                let deployment_watcher = DeploymentWatcher::new(deployment_watcher_client);
                deployment_watcher
                    .run(deployment_watcher_proto_rx, identity_manager_proto_tx)
                    .await;
            });
            tokio::spawn(async move {
                let mut system = get_sdp_system();
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
