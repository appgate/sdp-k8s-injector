use clap::{Parser, Subcommand};
use kube::CustomResourceExt;
use sdp_common::crd::DeviceId;

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

fn run() {
    println!("Hello World!")
}

#[tokio::main]
async fn main() -> () {
    env_logger::init();
    let args = DeviceIdService::parse();
    match args.command {
        DeviceIdCommands::Run => run(),
        DeviceIdCommands::Crd => crd(),
    }
}
