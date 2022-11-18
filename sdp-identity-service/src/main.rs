use crate::{
    deployment_watcher::DeploymentWatcherProtocol,
    identity_creator::{IdentityCreator, IdentityCreatorProtocol},
    identity_manager::{IdentityManagerProtocol, IdentityManagerRunner},
};
use clap::{Parser, Subcommand};
use k8s_openapi::api::{apps::v1::Deployment, core::v1::Namespace};
use kube::{Api, CustomResourceExt};
use log::error;
use sdp_common::kubernetes::get_k8s_client;
use sdp_common::sdp::system::get_sdp_system;
use sdp_common::watcher::{watch, WatcherWaitReady};
use sdp_common::{crd::ServiceIdentity, watcher::Watcher};
use std::{panic, process::exit};
use tokio::sync::mpsc::channel;

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
    log4rs::init_file("/opt/sdp-identity-service/log4rs.yaml", Default::default()).unwrap();

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
            let client = get_k8s_client().await;
            let identity_manager_client = client.clone();
            let deployment_watcher_client = client.clone();
            let identity_creator_client = client;
            let identity_manager_runner =
                IdentityManagerRunner::kube_runner(identity_manager_client);
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
                let ns_api: Api<Namespace> = Api::all(deployment_watcher_client);
                let watcher = Watcher {
                    api_ns: Some(ns_api),
                    api: deployment_api,
                    queue_tx: identity_manager_proto_tx.clone(),
                    notification_message: Some(IdentityManagerProtocol::DeploymentWatcherReady),
                };
                let watcher_ready = WatcherWaitReady(deployment_watcher_proto_rx, |_| true);
                if let Err(e) = watch(watcher, Some(watcher_ready)).await {
                    panic!("Error deploying Deployment Watcher: {}", e);
                }
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
