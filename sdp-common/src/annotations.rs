use const_format::formatcp;

const DOMAIN_ANNOTATION: &str = "k8s.appgate.com";
const COMPONENT_ANNOTATION: &str = "sdp-injector";

macro_rules! appgate_annotate {
    ($annotation:literal) => {{
        formatcp!(
            "{}~1{}.{}",
            DOMAIN_ANNOTATION,
            COMPONENT_ANNOTATION,
            $annotation
        )
    }};
}

pub const SDP_INJECTOR_ANNOTATION_STRATEGY: &str = appgate_annotate!("strategy");
pub const SDP_INJECTOR_ANNOTATION_ENABLED: &str = appgate_annotate!("enabled");
pub const SDP_INJECTOR_ANNOTATION_CLIENT_VERSION: &str = appgate_annotate!("client-version");
pub const SDP_INJECTOR_ANNOTATION_DISABLE_INIT_CONTAINERS: &str =
    appgate_annotate!("disable-init-containers");
pub const SDP_ANNOTATION_CLIENT_CONFIG: &str = appgate_annotate!("client-config");
pub const SDP_ANNOTATION_CLIENT_SECRETS: &str = appgate_annotate!("client-secrets");
pub const SDP_ANNOTATION_CLIENT_DEVICE_ID: &str = appgate_annotate!("device-id");
pub const SDP_ANNOTATION_DNS_SEARCHES: &str = appgate_annotate!("dns-searches");
