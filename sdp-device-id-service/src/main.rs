use crate::device_id_creator::{DeviceIdCreator, DeviceIdCreatorProtocol};
use crate::device_id_manager::{DeviceIdManagerProtocol, DeviceIdManagerRunner};
use crate::service_identity_watcher::ServiceIdentityWatcherProtocol;
use clap::{Parser, Subcommand};
use kube::CustomResourceExt;
use sdp_common::crd::{DeviceId, ServiceIdentity};
use sdp_common::kubernetes;
use sdp_common::sdp::system::get_sdp_system;
use service_identity_watcher::ServiceIdentityWatcher;
use tokio::sync::mpsc::channel;

mod device_id_creator;
mod device_id_errors;
mod device_id_manager;
mod service_identity_watcher;

const DEVICE_ID_POOL_SIZE: usize = 10;

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
    let (manager_proto_tx, manager_proto_rx) =
        channel::<DeviceIdManagerProtocol<ServiceIdentity, DeviceId>>(50);
    let manager_proto_tx_1 = manager_proto_tx.clone();
    let manager_proto_tx_2 = manager_proto_tx.clone();
    let device_id_manager = DeviceIdManagerRunner::kube_runner(manager_client);

    let watcher_client = kubernetes::get_k8s_client().await;
    let (watcher_proto_tx, watcher_proto_rx) = channel::<ServiceIdentityWatcherProtocol>(50);
    tokio::spawn(async {
        let watcher = ServiceIdentityWatcher::new(watcher_client);
        watcher.run(watcher_proto_rx, manager_proto_tx).await;
    });

    let creator_client = kubernetes::get_k8s_client().await;
    let (creator_proto_tx, creator_proto_rx) = channel::<DeviceIdCreatorProtocol>(50);
    let device_id_creator = DeviceIdCreator::new(creator_client, DEVICE_ID_POOL_SIZE).await;
    tokio::spawn(async move {
        let mut system = get_sdp_system();
        device_id_creator
            .run(&mut system, creator_proto_rx, manager_proto_tx_1)
            .await;
    });

    device_id_manager
        .run(
            manager_proto_rx,
            manager_proto_tx_2,
            creator_proto_tx,
            watcher_proto_tx,
            None,
        )
        .await;
}

#[tokio::main]
async fn main() -> () {
    env_logger::init();
    let args = DeviceIdService::parse();
    match args.command {
        DeviceIdCommands::Run => run().await,
        DeviceIdCommands::Crd => crd(),
    }
}
