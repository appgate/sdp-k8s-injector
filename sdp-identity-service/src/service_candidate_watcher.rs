use k8s_openapi::api::apps::v1::Deployment;
use k8s_openapi::api::core::v1::Namespace;
use kube::ResourceExt;
use log::{error, info};
use sdp_common::annotations::SDP_INJECTOR_LABEL;
use sdp_common::crd::SDPService;
use sdp_common::traits::MaybeService;
use sdp_common::watcher::SimpleWatchingProtocol;
use sdp_common::{crd::ServiceIdentity, traits::Candidate};
use sdp_macros::{service_candidate_protocol, when_ok};

use crate::identity_manager::IdentityManagerProtocol;

#[derive(Debug)]
pub enum DeploymentWatcherProtocol {
    IdentityManagerReady,
}

/*
 * SimpleWatchingProtocol that is shared between all Service Candidates
 */
trait ServiceCandidateProtocol<A: MaybeService + Candidate + Clone> {
    fn service<'a>(&'a self) -> &'a A;

    fn initialized(
        &self,
        ns: Option<Namespace>,
    ) -> Option<IdentityManagerProtocol<A, ServiceIdentity>> {
        when_ok!((service_id:IdentityManagerProtocol<A, ServiceIdentity> = self.service().service_id()) {
            let ns_candidate = ns.as_ref().and_then(|ns| ns.labels().get(SDP_INJECTOR_LABEL)).map(|s| s.eq_ignore_ascii_case("enabled")).unwrap_or(false);
            if ns_candidate && self.service().is_candidate() {
                info!("[{}] Found service candidate: {}", service_id, service_id);
                Some(IdentityManagerProtocol::FoundServiceCandidate(self.service().clone()))
            } else {
                info!("[{}] Ignored service candidate: {}", service_id, service_id);
                None
            }
        })
    }

    fn applied(
        &self,
        ns: Option<Namespace>,
    ) -> Option<IdentityManagerProtocol<A, ServiceIdentity>> {
        when_ok!((service_id:IdentityManagerProtocol<A, ServiceIdentity> = self.service().service_id()) {
            let ns_candidate = ns.as_ref().and_then(|ns| ns.labels().get(SDP_INJECTOR_LABEL)).map(|s| s.eq_ignore_ascii_case("enabled")).unwrap_or(false);
            if ns_candidate && self.service().is_candidate() {
                info!("[{}] Applied service candidate {}", service_id, service_id);
                Some(IdentityManagerProtocol::RequestServiceIdentity {
                    service_candidate: self.service().clone(),
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
    ) -> Option<IdentityManagerProtocol<A, ServiceIdentity>> {
        when_ok!((service_id:IdentityManagerProtocol<A, ServiceIdentity> = self.service().service_id()) {
            if self.service().is_candidate() {
                info!("[{}] Ignoring reapplied service candidate {}", service_id, service_id);
            }
            None
        })
    }

    fn deleted(
        &self,
        ns: Option<Namespace>,
    ) -> Option<IdentityManagerProtocol<A, ServiceIdentity>> {
        when_ok!((service_id:IdentityManagerProtocol<A, ServiceIdentity> = self.service().service_id()) {
            let ns_candidate = ns.as_ref().and_then(|ns| ns.labels().get(SDP_INJECTOR_LABEL)).map(|s| s.eq_ignore_ascii_case("enabled")).unwrap_or(false);
            if ns_candidate && self.service().is_candidate() {
                info!("[{}] Deleted candidate Deployment {}", service_id, service_id);
                Some(IdentityManagerProtocol::DeletedServiceCandidate(self.service().clone()))
            } else {
                info!("[{}] Ignoring deleted service candidate, not a candidate {}", service_id, service_id);
                None
            }
        })
    }

    fn key(&self) -> Option<String> {
        self.service().service_id().ok()
    }
}

#[macro_export]
macro_rules! service_candidate_protocol {
    ($t:ident) => {
        impl ServiceCandidateProtocol<$t> for $t {
            fn service<'a>(&'a self) -> &'a $t {
                &self
            }
        }

        impl SimpleWatchingProtocol<IdentityManagerProtocol<$t, ServiceIdentity>> for $t {
            fn initialized(
                &self,
                ns: Option<Namespace>,
            ) -> Option<IdentityManagerProtocol<$t, ServiceIdentity>> {
                ServiceCandidateProtocol::initialized(self, ns)
            }

            fn applied(
                &self,
                ns: Option<Namespace>,
            ) -> Option<IdentityManagerProtocol<$t, ServiceIdentity>> {
                ServiceCandidateProtocol::applied(self, ns)
            }

            fn reapplied(
                &self,
                ns: Option<Namespace>,
            ) -> Option<IdentityManagerProtocol<$t, ServiceIdentity>> {
                ServiceCandidateProtocol::reapplied(self, ns)
            }

            fn deleted(
                &self,
                ns: Option<Namespace>,
            ) -> Option<IdentityManagerProtocol<$t, ServiceIdentity>> {
                ServiceCandidateProtocol::deleted(self, ns)
            }

            fn key(&self) -> Option<String> {
                ServiceCandidateProtocol::key(self)
            }
        }
    };
}

service_candidate_protocol!(SDPService);
service_candidate_protocol!(Deployment);
