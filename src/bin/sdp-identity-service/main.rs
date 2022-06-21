use http::Uri;
use kube::{Client, Config, CustomResourceExt};
use log::info;
use reqwest::Url;
use std::{convert::TryFrom, env::args};
use tokio::sync::mpsc::channel;

use crate::{
    deployment_watcher::DeploymentWatcher,
    identity_creator::{IdentityCreator, IdentityCreatorMessage},
    identity_manager::{IdentityManager, IdentityManagerProtocol, ServiceIdentity},
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

fn show_crds() {
    let crd = ServiceIdentity::crd();
    let p = serde_json::to_string(&crd);
    println!("{}", p.unwrap());
}

#[tokio::main]
async fn main() -> () {
    env_logger::init();
    let command = args().nth(1);
    let arguments = args().skip(2).collect::<Vec<String>>();
    match command.as_deref() {
        Some("crd") => {
            show_crds();
        }
        Some("run") => {
            info!("Starting sdp identity service ...");
            let identity_manager_client = get_k8s_client().await;
            let deployment_watcher_client = get_k8s_client().await;
            let identity_creator_client = get_k8s_client().await;
            let mut identity_manager = IdentityManager::new(identity_manager_client);
            let identity_creator = IdentityCreator::new(identity_creator_client);
            let (sender, mut receiver) = channel::<IdentityManagerProtocol>(50);
            let (identity_creator_tx, mut identity_creator_rx) =
                channel::<IdentityCreatorMessage>(50);
            let sender2 = sender.clone();
            tokio::spawn(async move {
                let mut deployment_watcher = DeploymentWatcher::new(deployment_watcher_client);
                deployment_watcher.watch_deployments(sender).await;
            });
            tokio::spawn(async move {
                let credentials = Credentials {
                    username: "admin".to_string(),
                    password: "admin".to_string(),
                    provider_name: "local".to_string(),
                    device_id: Some("1234567880".to_string()),
                };
                let hosts =
                    vec![Url::parse("https://mycontroller.com")
                        .expect("Error parsing Url")];
                let mut system = SystemConfig::new(hosts)
                    .with_api_version("17")
                    .build(credentials)
                    .expect("Unable to create SDP client");
                identity_creator
                    .run(&mut system, identity_creator_rx, sender2)
                    .await;
            });
            identity_manager.run(receiver, identity_creator_tx).await;
        }
        Some(_) | None => {
            println!("Usage: sdp-identity-service [run | crd]");
        }
    }
}
