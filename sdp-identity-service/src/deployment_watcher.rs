use k8s_openapi::api::apps::v1::Deployment;
use k8s_openapi::api::core::v1::Namespace;
use kube::ResourceExt;
use log::{error, info};
use sdp_common::watcher::SimpleWatchingProtocol;
use sdp_common::{crd::ServiceIdentity, traits::Candidate, traits::MaybeService};
use sdp_macros::{sdp_info, sdp_log, when_ok};
use std::fmt::Debug;

use crate::identity_manager::IdentityManagerProtocol;

pub const SDP_INJECTOR_LABEL: &str = "sdp-injection";

macro_rules! watcher_info {
    ($target:expr $(, $arg:expr)*) => {
        sdp_info!("DeploymentWatcher" | ($target $(, $arg)*))
    };
}

#[derive(Debug)]
pub enum DeploymentWatcherProtocol {
    IdentityManagerReady,
}

impl SimpleWatchingProtocol<IdentityManagerProtocol<Deployment, ServiceIdentity>> for Deployment {
    fn initialized(
        &self,
        ns: Option<Namespace>,
    ) -> Option<IdentityManagerProtocol<Deployment, ServiceIdentity>> {
        when_ok!((service_id:IdentityManagerProtocol<Deployment, ServiceIdentity> = self.service_id()) {
            let ns_candidate = ns.as_ref().and_then(|ns| ns.labels().get(SDP_INJECTOR_LABEL)).map(|s| s.eq_ignore_ascii_case("enabled")).unwrap_or(false);
            if ns_candidate && self.is_candidate() {
                watcher_info!("Found service candidate: {}", service_id);
                Some(IdentityManagerProtocol::FoundServiceCandidate(self.clone()))
            } else {
                watcher_info!("Ignored service candidate: {}", service_id);
                None
            }
        })
    }

    fn applied(
        &self,
        ns: Option<Namespace>,
    ) -> Option<IdentityManagerProtocol<Deployment, ServiceIdentity>> {
        when_ok!((service_id:IdentityManagerProtocol<Deployment, ServiceIdentity> = self.service_id()) {
            let ns_candidate = ns.as_ref().and_then(|ns| ns.labels().get(SDP_INJECTOR_LABEL)).map(|s| s.eq_ignore_ascii_case("enabled")).unwrap_or(false);
            if ns_candidate && self.is_candidate() {
                watcher_info!("Applied candidate Deployment {}", service_id);
                Some(IdentityManagerProtocol::RequestServiceIdentity {
                    service_candidate: self.clone(),
                })
            } else {
                watcher_info!("Ignoring applied Deployment, not a candidate {}", service_id);
                None
            }
        })
    }

    fn deleted(
        &self,
        ns: Option<Namespace>,
    ) -> Option<IdentityManagerProtocol<Deployment, ServiceIdentity>> {
        when_ok!((service_id:IdentityManagerProtocol<Deployment, ServiceIdentity> = self.service_id()) {
            let ns_candidate = ns.as_ref().and_then(|ns| ns.labels().get(SDP_INJECTOR_LABEL)).map(|s| s.eq_ignore_ascii_case("enabled")).unwrap_or(false);
            if ns_candidate && self.is_candidate() {
                watcher_info!("Deleted candidate Deployment {}", service_id);
                Some(IdentityManagerProtocol::DeletedServiceCandidate(self.clone()))
            } else {
                watcher_info!("Ignoring deleted Deployment, not a candidate {}", service_id);
                None
            }
        })
    }

    fn reapplied(
        &self,
        _ns: Option<Namespace>,
    ) -> Option<IdentityManagerProtocol<Deployment, ServiceIdentity>> {
        when_ok!((service_id:IdentityManagerProtocol<Deployment, ServiceIdentity> = self.service_id()) {
            if self.is_candidate() {
                watcher_info!("Ignoring reapplied Deployment {}", service_id);
            }
            None
        })
    }

    fn key(&self) -> Option<String> {
        self.service_id().ok()
    }
}
