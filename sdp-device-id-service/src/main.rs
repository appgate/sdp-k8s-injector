use clap::{Parser, Subcommand};

#[derive(Debug, Subcommand)]
enum DeviceIdCommands {
    /// Runs the DeviceId Service
    Run,
}

#[derive(Debug, Parser)]
#[clap(name = "sdp-device-id-service")]
struct DeviceIdService {
    #[clap(subcommand)]
    command: DeviceIdCommands,
}

#[tokio::main]
async fn main() -> () {
    env_logger::init();
    let args = DeviceIdService::parse();
    match args.command {
        DeviceIdCommands::Run => {
            println!("Hello World!")
        }
    }
}
