use clap::{Parser, Subcommand};
use k8s_openapi::api::apps::v1::Deployment;
use kube::{Api, CustomResourceExt};
use log::{error, info};
use sdp_common::constants::SDP_CLUSTER_ID_ENV;
use sdp_common::crd::ServiceIdentity;
use sdp_common::kubernetes::get_k8s_client;
use sdp_common::sdp::system::get_sdp_system;
use std::{panic, process::exit};
use tokio::sync::mpsc::channel;

use crate::deployment_watcher::Watcher;
use crate::{
    deployment_watcher::DeploymentWatcherProtocol,
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

    // Exit on panics from other threads
    panic::set_hook(Box::new(|info| {
        error!("Got panic. @info:{}", info);
        exit(1);
    }));

    let args = IdentityService::parse();
    match args.command {
        IdentityServiceCommands::Crd => {
            show_crds();
        }
        IdentityServiceCommands::Run => {
            info!("Starting sdp identity service ...");
            let client = get_k8s_client().await;
            let identity_manager_client = client.clone();
            let deployment_watcher_client = client.clone();
            let identity_creator_client = client;
            let cluster_id = std::env::var(SDP_CLUSTER_ID_ENV);
            if cluster_id.is_err() {
                panic!("Unable to get cluster id, make sure SDP_CLUSTER_ID environemnt variable is set.");
            }
            let identity_manager_runner =
                IdentityManagerRunner::kube_runner(identity_manager_client, cluster_id.unwrap());
            let (identity_manager_proto_tx, identity_manager_proto_rx) =
                channel::<IdentityManagerProtocol<Deployment, ServiceIdentity>>(50);
            let identity_manager_proto_tx_cp = identity_manager_proto_tx.clone();
            let identity_manager_proto_tx_cp2 = identity_manager_proto_tx.clone();
            let (identity_creator_proto_tx, identity_creator_proto_rx) =
                channel::<IdentityCreatorProtocol>(50);
            let (deployment_watched_proto_tx, deployment_watcher_proto_rx) =
                channel::<DeploymentWatcherProtocol>(50);
            tokio::spawn(async move {
                let deployment_api: Api<Deployment> = Api::all(deployment_watcher_client.clone());
                let mut watcher = Watcher {
                    api: deployment_api,
                    queue_tx: identity_manager_proto_tx.clone(),
                    queue_rx: deployment_watcher_proto_rx,
                };
                watcher.watch().await;
            });
            tokio::spawn(async move {
                let system = get_sdp_system();
                let identity_creator =
                    IdentityCreator::new(system, identity_creator_client, CREDENTIALS_POOL_SIZE);
                let mut system2 = get_sdp_system();
                identity_creator
                    .run(
                        &mut system2,
                        identity_creator_proto_rx,
                        identity_manager_proto_tx_cp,
                    )
                    .await;
            });
            identity_manager_runner
                .run(
                    identity_manager_proto_rx,
                    identity_manager_proto_tx_cp2,
                    identity_creator_proto_tx.clone(),
                    deployment_watched_proto_tx,
                    None,
                )
                .await;
        }
    }
}
