use const_format::formatcp;

const DOMAIN_ANNOTATION: &str = "k8s.appgate.com";
const COMPONENT_ANNOTATION: &str = "sdp-injector";

pub const SDP_INJECTOR_ANNOTATION_STRATEGY: &str = formatcp!("{DOMAIN_ANNOTATION}~1{COMPONENT_ANNOTATION}.strategy");
pub const SDP_INJECTOR_ANNOTATION_ENABLED: &str = formatcp!("{DOMAIN_ANNOTATION}~1{COMPONENT_ANNOTATION}.enabled");
pub const SDP_INJECTOR_ANNOTATION_CLIENT_VERSION: &str = formatcp!("{DOMAIN_ANNOTATION}~1{COMPONENT_ANNOTATION}.client-version");
pub const SDP_INJECTOR_ANNOTATION_DISABLE_INIT_CONTAINERS: &str  = formatcp!("{DOMAIN_ANNOTATION}~1{COMPONENT_ANNOTATION}.disable-init-containers");
pub const SDP_ANNOTATION_CLIENT_CONFIG: &str = formatcp!("{DOMAIN_ANNOTATION}~1{COMPONENT_ANNOTATION}.client-config");
pub const SDP_ANNOTATION_CLIENT_SECRETS: &str = formatcp!("{DOMAIN_ANNOTATION}~1{COMPONENT_ANNOTATION}.client-secrets");
pub const SDP_ANNOTATION_CLIENT_DEVICE_ID: &str = formatcp!("{DOMAIN_ANNOTATION}~1{COMPONENT_ANNOTATION}.device-id");
pub const SDP_ANNOTATION_DNS_SEARCHES: &str = formatcp!("{DOMAIN_ANNOTATION}~1{COMPONENT_ANNOTATION}.dns-searches");
