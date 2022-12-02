use k8s_openapi::api::apps::v1::Deployment;
use k8s_openapi::api::core::v1::Namespace;
use kube::ResourceExt;
use sdp_common::annotations::SDP_INJECTOR_LABEL;
use sdp_common::watcher::SimpleWatchingProtocol;
use sdp_common::{crd::ServiceIdentity, traits::Candidate, traits::MaybeService};
use sdp_macros::{logger, sdp_error, sdp_info, sdp_log, when_ok, with_dollar_sign};
use std::fmt::Debug;

use crate::identity_manager::IdentityManagerProtocol;

logger!("DeploymentWatcher");

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
                info!("[{}] Found service candidate: {}", service_id, service_id);
                Some(IdentityManagerProtocol::FoundServiceCandidate(self.clone()))
            } else {
                info!("[{}] Ignored service candidate: {}", service_id, service_id);
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
                info!("[{}] Applied candidate Deployment {}", service_id, service_id);
                Some(IdentityManagerProtocol::RequestServiceIdentity {
                    service_candidate: self.clone(),
                })
            } else {
                info!("[{}] Ignoring applied Deployment, not a candidate {}", service_id, service_id);
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
                info!("[{}] Ignoring reapplied Deployment {}", service_id, service_id);
            }
            None
        })
    }

    fn deleted(
        &self,
        ns: Option<Namespace>,
    ) -> Option<IdentityManagerProtocol<Deployment, ServiceIdentity>> {
        when_ok!((service_id:IdentityManagerProtocol<Deployment, ServiceIdentity> = self.service_id()) {
            let ns_candidate = ns.as_ref().and_then(|ns| ns.labels().get(SDP_INJECTOR_LABEL)).map(|s| s.eq_ignore_ascii_case("enabled")).unwrap_or(false);
            if ns_candidate && self.is_candidate() {
                info!("[{}] Deleted candidate Deployment {}", service_id, service_id);
                Some(IdentityManagerProtocol::DeletedServiceCandidate(self.clone()))
            } else {
                info!("[{}] Ignoring deleted Deployment, not a candidate {}", service_id, service_id);
                None
            }
        })
    }

    fn key(&self) -> Option<String> {
        self.service_id().ok()
    }
}
