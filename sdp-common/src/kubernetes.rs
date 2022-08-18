use http::Uri;
use kube::{Client, Config};

pub const SDP_K8S_HOST_ENV: &str = "SDP_K8S_HOST";
pub const SDP_K8S_HOST_DEFAULT: &str = "kubernetes.default.svc";
pub const SDP_K8S_USERNAME_ENV: &str = "SDP_K8S_USERNAME";
pub const SDP_K8S_USERNAME_DEFAULT: &str = "admin";
pub const SDP_K8S_PASSWORD_ENV: &str = "SDP_K8S_PASSWORD";
pub const SDP_K8S_PASSWORD_DEFAULT: &str = "admin";
pub const SDP_K8S_PROVIDER_ENV: &str = "SDP_K8S_PROVIDER";
pub const SDP_K8S_PROVIDER_DEFAULT: &str = "local";
pub const SDP_K8S_NO_VERIFY_ENV: &str = "SDP_K8S_NO_VERIFY";

pub const SDP_NAMESPACE: &str = "sdp-system";

pub async fn get_k8s_client() -> Client {
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
