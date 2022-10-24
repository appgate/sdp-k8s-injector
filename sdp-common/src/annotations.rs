pub const SDP_INJECTOR_ANNOTATION_STRATEGY: &str =
    concat!("k8s.appgate.com", "~1", "sdp-injector", ".", "strategy");
pub const SDP_INJECTOR_ANNOTATION_ENABLED: &str =
    concat!("k8s.appgate.com", "~1", "sdp-injector", ".", "enabled");
pub const SDP_INJECTOR_ANNOTATION_CLIENT_VERSION: &str = concat!(
    "k8s.appgate.com",
    "~1",
    "sdp-injector",
    ".",
    "client-version"
);
pub const SDP_INJECTOR_ANNOTATION_DISABLE_INIT_CONTAINERS: &str = concat!(
    "k8s.appgate.com",
    "~1",
    "sdp-injector",
    ".",
    "disable-init-containers"
);
pub const SDP_ANNOTATION_CLIENT_CONFIG: &str = "sdp-injector-client-config";
pub const SDP_ANNOTATION_CLIENT_SECRETS: &str = "sdp-injector-client-secrets";
pub const SDP_ANNOTATION_CLIENT_DEVICE_ID: &str = "sdp-injector-client-device-id";
pub const SDP_ANNOTATION_DNS_SEARCHES: &str = "sdp-injector-dns-searches";
