use k8s_openapi::api::apps::v1::Deployment;
use k8s_openapi::api::core::v1::Namespace;
use kube::ResourceExt;
use sdp_common::annotations::SDP_INJECTOR_LABEL;
use sdp_common::crd::SDPService;
use sdp_common::service::ServiceCandidate;
use sdp_common::traits::MaybeService;
use sdp_common::watcher::SimpleWatchingProtocol;
use sdp_common::{crd::ServiceIdentity, traits::Candidate};
use sdp_macros::{
    logger, sdp_error, sdp_info, sdp_log, service_candidate_protocol, when_ok, with_dollar_sign,
};

use crate::identity_manager::IdentityManagerProtocol;

logger!("SDPServiceWatcher");

#[derive(Debug, Clone)]
pub enum ServiceCandidateWatcherProtocol {
    IdentityManagerReady,
}

/*
 * SimpleWatchingProtocol that is shared between all Service Candidates
 */
trait ServiceCandidateProtocol {
    fn service_candidate<'a>(&'a self) -> ServiceCandidate;

    fn initialized(
        &self,
        ns: Option<Namespace>,
    ) -> Option<IdentityManagerProtocol<ServiceCandidate, ServiceIdentity>> {
        when_ok!((service_id:IdentityManagerProtocol<ServiceCandidate, ServiceIdentity> = self.service_candidate().service_id()) {
            let ns_candidate = ns.as_ref().and_then(|ns| ns.labels().get(SDP_INJECTOR_LABEL)).map(|s| s.eq_ignore_ascii_case("enabled")).unwrap_or(false);
            if ns_candidate && self.service_candidate().is_candidate() {
                info!("[{}] Found service candidate: {}", service_id, service_id);
                Some(IdentityManagerProtocol::FoundServiceCandidate(self.service_candidate().clone()))
            } else {
                info!("[{}] Ignored service candidate: {}", service_id, service_id);
                None
            }
        })
    }

    fn applied(
        &self,
        ns: Option<Namespace>,
    ) -> Option<IdentityManagerProtocol<ServiceCandidate, ServiceIdentity>> {
        when_ok!((service_id:IdentityManagerProtocol<ServiceCandidate, ServiceIdentity> = self.service_candidate().service_id()) {
            let ns_candidate = ns.as_ref().and_then(|ns| ns.labels().get(SDP_INJECTOR_LABEL)).map(|s| s.eq_ignore_ascii_case("enabled")).unwrap_or(false);
            if ns_candidate && self.service_candidate().is_candidate() {
                info!("[{}] Applied service candidate {}", service_id, service_id);
                Some(IdentityManagerProtocol::RequestServiceIdentity {
                    service_candidate: self.service_candidate().clone(),
                })
            } else {
                info!("[{}] Ignoring applied service candidate, not a candidate {}", service_id, service_id);
                None
            }
        })
    }

    fn reapplied(
        &self,
        _ns: Option<Namespace>,
    ) -> Option<IdentityManagerProtocol<ServiceCandidate, ServiceIdentity>> {
        when_ok!((service_id:IdentityManagerProtocol<ServiceCandidate, ServiceIdentity> = self.service_candidate().service_id()) {
            if self.service_candidate().is_candidate() {
                info!("[{}] Ignoring reapplied service candidate {}", service_id, service_id);
            }
            None
        })
    }

    fn deleted(
        &self,
        ns: Option<Namespace>,
    ) -> Option<IdentityManagerProtocol<ServiceCandidate, ServiceIdentity>> {
        when_ok!((service_id:IdentityManagerProtocol<ServiceCandidate, ServiceIdentity> = self.service_candidate().service_id()) {
            let ns_candidate = ns.as_ref().and_then(|ns| ns.labels().get(SDP_INJECTOR_LABEL)).map(|s| s.eq_ignore_ascii_case("enabled")).unwrap_or(false);
            if ns_candidate && self.service_candidate().is_candidate() {
                info!("[{}] Deleted candidate Deployment {}", service_id, service_id);
                Some(IdentityManagerProtocol::DeletedServiceCandidate(self.service_candidate().clone()))
            } else {
                info!("[{}] Ignoring deleted service candidate, not a candidate {}", service_id, service_id);
                None
            }
        })
    }

    fn key(&self) -> Option<String> {
        self.service_candidate().service_id().ok()
    }
}

#[macro_export]
macro_rules! service_candidate_protocol {
    ($t:ident, $new_candidate:expr) => {
        impl ServiceCandidateProtocol for $t {
            fn service_candidate(&self) -> ServiceCandidate {
                $new_candidate(self.clone())
            }
        }

        impl SimpleWatchingProtocol<IdentityManagerProtocol<ServiceCandidate, ServiceIdentity>>
            for $t
        {
            fn initialized(
                &self,
                ns: Option<Namespace>,
            ) -> Option<IdentityManagerProtocol<ServiceCandidate, ServiceIdentity>> {
                ServiceCandidateProtocol::initialized(self, ns)
            }

            fn applied(
                &self,
                ns: Option<Namespace>,
            ) -> Option<IdentityManagerProtocol<ServiceCandidate, ServiceIdentity>> {
                ServiceCandidateProtocol::applied(self, ns)
            }

            fn reapplied(
                &self,
                ns: Option<Namespace>,
            ) -> Option<IdentityManagerProtocol<ServiceCandidate, ServiceIdentity>> {
                ServiceCandidateProtocol::reapplied(self, ns)
            }

            fn deleted(
                &self,
                ns: Option<Namespace>,
            ) -> Option<IdentityManagerProtocol<ServiceCandidate, ServiceIdentity>> {
                ServiceCandidateProtocol::deleted(self, ns)
            }

            fn key(&self) -> Option<String> {
                ServiceCandidateProtocol::key(self)
            }
        }
    };
}

service_candidate_protocol!(SDPService, ServiceCandidate::SDPService);
service_candidate_protocol!(Deployment, ServiceCandidate::Deployment);
