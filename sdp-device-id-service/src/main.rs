use crate::device_id_manager::{DeviceIdManagerProtocol, DeviceIdManagerRunner};
use crate::service_identity_watcher::ServiceIdentityWatcherProtocol;
use clap::{Parser, Subcommand};
use kube::{Api, CustomResourceExt};
use sdp_common::crd::{DeviceId, ServiceIdentity};
use sdp_common::kubernetes::{self, SDP_K8S_NAMESPACE};
use sdp_common::watcher::{watch, Watcher, WatcherWaitReady};
use tokio::sync::mpsc::channel;

mod device_id_errors;
mod device_id_manager;
mod service_identity_watcher;

#[derive(Debug, Subcommand)]
enum DeviceIdCommands {
    /// Prints the DeviceId CRD in YAML
    Crd,
    /// Runs the DeviceId Service
    Run,
}

#[derive(Debug, Parser)]
#[clap(name = "sdp-device-id-service")]
struct DeviceIdService {
    #[clap(subcommand)]
    command: DeviceIdCommands,
}

fn crd() {
    let crd = DeviceId::crd();
    let result = serde_json::to_string(&crd);
    println!("{}", result.unwrap());
}

async fn run() {
    let manager_client = kubernetes::get_k8s_client().await;
    let (manager_proto_tx_1, manager_proto_rx) =
        channel::<DeviceIdManagerProtocol<ServiceIdentity>>(50);
    let manager_proto_tx_2 = manager_proto_tx_1.clone();
    let device_id_manager = DeviceIdManagerRunner::kube_runner(manager_client);

    let client = kubernetes::get_k8s_client().await;
    let (watcher_proto_tx, watcher_proto_rx) = channel::<ServiceIdentityWatcherProtocol>(50);
    let api: Api<ServiceIdentity> = Api::namespaced(client, SDP_K8S_NAMESPACE);
    tokio::spawn(async move {
        let watcher = Watcher {
            api_ns: None,
            api: api,
            queue_tx: manager_proto_tx_1.clone(),
            notification_message: None,
        };
        let watcher_ready = WatcherWaitReady(watcher_proto_rx, |_| true);
        if let Err(e) = watch(watcher, Some(watcher_ready)).await {
            panic!("Error deploying Deployment Watcher: {}", e);
        }
    });
    device_id_manager
        .run(manager_proto_rx, manager_proto_tx_2, watcher_proto_tx, None)
        .await;
}

#[tokio::main]
async fn main() -> () {
    log4rs::init_file("/opt/sdp-device-id-service/log4rs.yaml", Default::default()).unwrap();

    let args = DeviceIdService::parse();
    match args.command {
        DeviceIdCommands::Run => run().await,
        DeviceIdCommands::Crd => crd(),
    }
}
