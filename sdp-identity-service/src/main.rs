use crate::{
    identity_creator::{IdentityCreator, IdentityCreatorProtocol},
    identity_manager::{IdentityManager, IdentityManagerProtocol},
    service_candidate_watcher::ServiceCandidateWatcherProtocol,
};
use clap::{Parser, Subcommand};
use identity_manager::{IdentityManagerRunner, IdentityManagerServiceIdentityAPI};
use k8s_openapi::api::{
    apps::v1::Deployment,
    core::v1::{Namespace, Pod},
};
use kube::{Api, CustomResourceExt};
use log::error;
use sdp_common::{crd::SDPService, sdp::system::get_sdp_system, service::ServiceCandidate};
use sdp_common::{crd::ServiceIdentity, watcher::Watcher};
use sdp_common::{kubernetes::get_k8s_client, service::get_log_config_path};
use sdp_common::{
    kubernetes::SDP_K8S_NAMESPACE,
    watcher::{watch, WatcherWaitReady},
};
use std::{collections::HashSet, panic, process::exit, time::Duration};
use tokio::sync::broadcast::channel as broadcast_channel;
use tokio::sync::mpsc::channel;

pub mod errors;
pub mod identity_creator;
pub mod identity_manager;
pub mod pod_watcher;
pub mod service_candidate_watcher;

const CREDENTIALS_POOL_SIZE: usize = 10;

fn show_crds() {
    let crd = ServiceIdentity::crd();
    let p = serde_json::to_string(&crd);
    println!("{}", p.unwrap());
}

async fn release_device_ids(user_name: String, since: Duration, dry_run: bool, exact_match: bool) {
    let mut sdp_client = get_sdp_system().await;
    let mut hs: Option<HashSet<String>> = None;
    if exact_match {
        hs = Some(HashSet::from_iter(vec![user_name.to_string()]));
    }
    if let Err(e) = sdp_client
        .unregister_device_ids_for_username(&user_name, None, hs.as_ref(), Some(since), dry_run)
        .await
    {
        error!(
            "Error releasing device ids for user {} older than {} seconds: {}",
            user_name,
            since.as_secs(),
            e.to_string()
        );
    }
}

#[derive(Subcommand)]
enum IdentityServiceCommands {
    /// Cli tool to manage device ids
    DeviceIds(DeviceIdsArgs),
    /// Prints the ServiceIdentity CustomResourceDefinition YAML
    Crd,
    /// Runs the Identity Service
    Run,
}

#[derive(clap::Parser, Debug)]
struct DeviceIdsArgs {
    #[arg()]
    user_name: String,
    #[arg(value_parser = parse_seconds)]
    since: Duration,
    #[arg(long = "dry-run")]
    dry_run: bool,
    #[arg(long = "exact-match")]
    exact_match: bool,
}

fn parse_seconds(arg: &str) -> Result<std::time::Duration, std::num::ParseIntError> {
    let seconds = arg.parse()?;
    Ok(std::time::Duration::from_secs(seconds))
}

#[derive(Parser)]
#[clap(name = "sdp-identity-service")]
struct IdentityService {
    #[clap(subcommand)]
    command: IdentityServiceCommands,
}

#[tokio::main]
async fn main() -> () {
    log4rs::init_file(get_log_config_path(), Default::default()).unwrap();

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
        IdentityServiceCommands::DeviceIds(args) => {
            release_device_ids(args.user_name, args.since, args.dry_run, args.exact_match).await;
        }
        IdentityServiceCommands::Run => {
            let client = get_k8s_client().await;
            let identity_manager_client = client.clone();
            let deployment_watcher_client = client.clone();
            let sdp_service_watcher = client.clone();
            let pod_watcher = client.clone();
            let identity_creator_client = client;

            // Create channel for IdentityManagerProtocol
            // It has several Senders (IdentityCreator, IdentityManager and ServiceCandidate watchers)
            //   and it has 1 Receiver: IdentityManager (mpsc channel is used)
            let (identity_manager_proto_tx, identity_manager_proto_rx) =
                channel::<IdentityManagerProtocol<ServiceCandidate, ServiceIdentity>>(50);
            // Create copies for senders
            let identity_manager_proto_tx_1 = identity_manager_proto_tx.clone();
            let identity_manager_proto_tx_2 = identity_manager_proto_tx.clone();
            let identity_manager_proto_tx_3 = identity_manager_proto_tx.clone();
            let identity_manager_proto_tx_4 = identity_manager_proto_tx.clone();

            // Create channel for IdentityCreatorProtocol
            // IdentityService is the Sender and IdentityCreator is the Receiver (mpsc channel is used)
            let (identity_creator_proto_tx, identity_creator_proto_rx) =
                channel::<IdentityCreatorProtocol>(50);

            // Create the channel for ServiceCandidateWatcherProtocol
            // IdentityManager is the Sender and each of the ServiceCandidate watchers are the Receivers (broadcast channel is used)
            // TODO: Implement using watch channel
            let (service_candidate_watcher_proto_tx, service_candidate_watcher_proto_rx) =
                broadcast_channel::<ServiceCandidateWatcherProtocol>(50);
            // We have 2 ServiceCandidate watchers, subscribe to the channel
            let service_candidate_watcher_proto_rx_1 =
                service_candidate_watcher_proto_tx.subscribe();

            /* Create a thread that starts a Watcher listening for Deployments, it will propagate Deployment
             *   as ServiceCandidate::Deployment
             * queue_tx is used to notify IdentityManager about initial ServiceCandidates and to
             *   notify that it's ready.
             */
            tokio::spawn(async move {
                let deployment_api: Api<Deployment> = Api::all(deployment_watcher_client.clone());
                let ns_api: Api<Namespace> = Api::all(deployment_watcher_client);
                let watcher = Watcher {
                    api_ns: Some(ns_api),
                    api: deployment_api,
                    queue_tx: identity_manager_proto_tx_1,
                    notification_message: Some(IdentityManagerProtocol::DeploymentWatcherReady),
                };
                let watcher_ready =
                    WatcherWaitReady(service_candidate_watcher_proto_rx_1, |_| true);
                if let Err(e) = watch(watcher, Some(watcher_ready)).await {
                    panic!("Error deploying Deployment Watcher: {}", e);
                }
            });

            /* Create a thread that starts a Watcher listening for SDPServices, it will propagate SDPService
             *   as ServiceCandidate::SDPService
             * queue_tx is used to notify IdentityManager about initial ServiceCandidates and to
             *   notify that it's ready.
             */
            tokio::spawn(async move {
                let sdp_service_api: Api<SDPService> = Api::all(sdp_service_watcher.clone());
                let ns_api: Api<Namespace> = Api::all(sdp_service_watcher);
                let watcher = Watcher {
                    api_ns: Some(ns_api),
                    api: sdp_service_api,
                    queue_tx: identity_manager_proto_tx_2,
                    notification_message: Some(IdentityManagerProtocol::DeploymentWatcherReady),
                };
                let watcher_ready = WatcherWaitReady(service_candidate_watcher_proto_rx, |_| true);
                if let Err(e) = watch(watcher, Some(watcher_ready)).await {
                    panic!("Error deploying SDPService Watcher: {}", e);
                }
            });

            // Thread to watch Pod entities
            // When a Pod that is a candidate has been deleted, we just return back the device id
            // it was using to the device ids provider.
            let pods_api: Api<Pod> = Api::all(pod_watcher);
            tokio::spawn(async move {
                let watcher = Watcher {
                    api_ns: None,
                    api: pods_api,
                    queue_tx: identity_manager_proto_tx_3,
                    notification_message: None,
                };
                let w = watch::<
                    Pod,
                    IdentityManagerProtocol<ServiceCandidate, ServiceIdentity>,
                    IdentityManagerProtocol<ServiceCandidate, ServiceIdentity>,
                >(watcher, None);
                if let Err(e) = w.await {
                    panic!("Unable to start Pod Watcher: {}", e);
                }
            });

            tokio::spawn(async move {
                let system = get_sdp_system().await;
                let identity_creator =
                    IdentityCreator::new(system, identity_creator_client, CREDENTIALS_POOL_SIZE);
                let mut system2 = get_sdp_system().await;
                identity_creator
                    .run(
                        &mut system2,
                        identity_creator_proto_rx,
                        identity_manager_proto_tx_4,
                    )
                    .await;
            });
            let mut identity_manager = IdentityManager::new(
                IdentityManagerServiceIdentityAPI::new(
                    Api::namespaced(identity_manager_client, SDP_K8S_NAMESPACE),
                    identity_creator_proto_tx.clone(),
                ),
                identity_manager_proto_rx,
                identity_manager_proto_tx,
                service_candidate_watcher_proto_tx,
                identity_creator_proto_tx.clone(),
                None,
            );
            identity_manager.run().await;
        }
    }
}
