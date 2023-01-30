use crate::job_watcher::JobWatcherProtocol;
use crate::{
    deployment_watcher::DeploymentWatcherProtocol,
    identity_creator::{IdentityCreator, IdentityCreatorProtocol},
    identity_manager::{IdentityManagerProtocol, IdentityManagerRunner},
};
use clap::{Parser, Subcommand};
use k8s_openapi::api::apps::v1::Deployment;
use k8s_openapi::api::batch::v1::Job;
use k8s_openapi::api::core::v1::Namespace;
use kube::{Api, CustomResourceExt};
use log::error;
use sdp_common::kubernetes::Target;
use sdp_common::sdp::system::get_sdp_system;
use sdp_common::watcher::{watch, WatcherWaitReady};
use sdp_common::{crd::ServiceIdentity, watcher::Watcher};
use sdp_common::{kubernetes::get_k8s_client, service::get_log_config_path};
use std::{panic, process::exit};
use tokio::sync::mpsc::channel;

pub mod deployment_watcher;
pub mod errors;
pub mod identity_creator;
pub mod identity_manager;
pub mod job_watcher;

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
        IdentityServiceCommands::Run => {
            let client = get_k8s_client().await;

            // Identity manager
            let im_client = client.clone();
            let im_runner = IdentityManagerRunner::kube_runner(im_client);
            let (im_proto_tx, im_proto_rx) =
                channel::<IdentityManagerProtocol<Target, ServiceIdentity>>(50);
            let im_proto_tx2 = im_proto_tx.clone();

            // Deployment Watcher
            let deployment_watcher_client = client.clone();
            let (deployment_watcher_proto_tx, deployment_watcher_proto_rx) =
                channel::<DeploymentWatcherProtocol>(50);
            let im_proto_tx_deployment = im_proto_tx.clone();
            tokio::spawn(async move {
                let deployment_api: Api<Deployment> = Api::all(deployment_watcher_client.clone());
                let ns_api: Api<Namespace> = Api::all(deployment_watcher_client);
                let watcher = Watcher {
                    api_ns: Some(ns_api),
                    api: deployment_api,
                    queue_tx: im_proto_tx_deployment.clone(),
                    notification_message: Some(IdentityManagerProtocol::DeploymentWatcherReady),
                };
                let watcher_ready = WatcherWaitReady(deployment_watcher_proto_rx, |_| true);
                if let Err(e) = watch(watcher, Some(watcher_ready)).await {
                    panic!("Error deploying Deployment Watcher: {}", e);
                }
            });

            // Job Watcher
            let job_watcher_client = client.clone();
            let (job_watcher_proto_tx, job_watcher_proto_rx) = channel::<JobWatcherProtocol>(50);
            let im_proto_tx_job = im_proto_tx.clone();
            tokio::spawn(async move {
                let job_api: Api<Job> = Api::all(job_watcher_client.clone());
                let ns_api: Api<Namespace> = Api::all(job_watcher_client);
                let watcher = Watcher {
                    api_ns: Some(ns_api),
                    api: job_api,
                    queue_tx: im_proto_tx_job.clone(),
                    notification_message: Some(IdentityManagerProtocol::JobWatcherReady),
                };
                let watcher_ready = WatcherWaitReady(job_watcher_proto_rx, |_| true);
                if let Err(e) = watch(watcher, Some(watcher_ready)).await {
                    panic!("Error deploying Job Watcher: {}", e);
                }
            });

            // Identity Creator
            let identity_creator_client = client;
            let (identity_creator_proto_tx, identity_creator_proto_rx) =
                channel::<IdentityCreatorProtocol>(50);
            tokio::spawn(async move {
                let system = get_sdp_system();
                let identity_creator =
                    IdentityCreator::new(system, identity_creator_client, CREDENTIALS_POOL_SIZE);
                let mut system2 = get_sdp_system();
                identity_creator
                    .run(&mut system2, identity_creator_proto_rx, im_proto_tx)
                    .await;
            });
            im_runner
                .run(
                    im_proto_rx,
                    im_proto_tx2,
                    identity_creator_proto_tx.clone(),
                    deployment_watcher_proto_tx,
                    job_watcher_proto_tx,
                    None,
                )
                .await;
        }
    }
}
