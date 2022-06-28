use errors::IdentityServiceError;
use http::Uri;
use k8s_openapi::api::apps::v1::Deployment;
use kube::{Client, Config, CustomResourceExt};
use log::info;
use reqwest::Url;
use std::{convert::TryFrom, env::args};
use tokio::sync::mpsc::channel;

use crate::{
    deployment_watcher::{DeploymentWatcher, DeploymentWatcherProtocol},
    identity_creator::{IdentityCreator, IdentityCreatorProtocol},
    identity_manager::{IdentityManagerProtocol, IdentityManagerRunner, ServiceIdentity},
    sdp::{Credentials, SystemConfig},
};

pub mod deployment_watcher;
pub mod errors;
pub mod identity_creator;
pub mod identity_manager;
pub mod sdp;

const SDP_K8S_HOST_ENV: &str = "SDP_K8S_HOST";
const SDP_K8S_HOST_DEFAULT: &str = "kubernetes.default.svc";
const SDP_K8S_NO_VERIFY_ENV: &str = "SDP_K8S_NO_VERIFY";
const SDP_SYSTEM_HOSTS: &str = "SDP_SYSTEM_HOSTS";
const CREDENTIALS_POOL_SIZE: usize = 10;

async fn get_k8s_client() -> Client {
    let mut k8s_host = String::from("https://");
    k8s_host.push_str(&std::env::var(SDP_K8S_HOST_ENV).unwrap_or(SDP_K8S_HOST_DEFAULT.to_string()));
    let k8s_uri = k8s_host
        .parse::<Uri>()
        .expect("Unable to parse SDP_K8S_HOST value:");
    let mut k8s_config = Config::infer()
        .await
        .expect("Unable to infer K8S configuration");
    k8s_config.cluster_url = k8s_uri;
    k8s_config.accept_invalid_certs = std::env::var(SDP_K8S_NO_VERIFY_ENV)
        .map(|v| v == "1" || v.to_lowercase() == "true")
        .unwrap_or(false);
    Client::try_from(k8s_config).expect("Unable to create k8s client")
}

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

#[tokio::main]
async fn main() -> () {
    env_logger::init();
    let command = args().nth(1);
    let _arguments = args().skip(2).collect::<Vec<String>>();
    match command.as_deref() {
        Some("crd") => {
            show_crds();
        }
        Some("run") => {
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
                    username: "admin".to_string(),
                    password: "admin".to_string(),
                    provider_name: "local".to_string(),
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
                )
                .await;
        }
        Some(_) | None => {
            println!("Usage: sdp-identity-service [run | crd]");
        }
    }
}
