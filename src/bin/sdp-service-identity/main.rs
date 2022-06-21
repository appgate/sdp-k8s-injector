use http::Uri;
use identity::{DeploymentWatcher, IdentityManager};
use kube::{Client, Config, CustomResourceExt};
use log::info;
use std::{convert::TryFrom, env::args};
use tokio::sync::mpsc::channel;

use crate::identity::{IdentityManagerProtocol, ServiceIdentity};

pub mod identity;
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
            let mut identity_manager = IdentityManager::new(identity_manager_client);
            let (sender, mut receiver) = channel::<IdentityManagerProtocol>(50);
            tokio::spawn(async move {
                let mut deployment_watcher = DeploymentWatcher::new(deployment_watcher_client);
                deployment_watcher.watch_deployments(sender).await;
            });
            identity_manager.run(receiver).await;
        }
        Some(_) | None => {
            println!("Usage: sdp-identity-service [run | crd]");
        }
    }
}
