use k8s_openapi::api::apps::v1::Deployment;
use log::{error, info};
use sdp_common::watcher::SimpleWatchingProtocol;
use sdp_common::{crd::ServiceIdentity, traits::Candidate, traits::MaybeService};
use sdp_macros::when_ok;
use std::fmt::Debug;

use crate::identity_manager::IdentityManagerProtocol;

#[derive(Debug)]
pub enum DeploymentWatcherProtocol {
    IdentityManagerReady,
}

impl SimpleWatchingProtocol<IdentityManagerProtocol<Deployment, ServiceIdentity>> for Deployment {
    fn initialized(&self) -> Option<IdentityManagerProtocol<Deployment, ServiceIdentity>> {
        when_ok!((service_id:IdentityManagerProtocol<Deployment, ServiceIdentity> = self.service_id()) {
            if self.is_candidate() {
                info!("Found service candidate: {}", service_id);
                Some(IdentityManagerProtocol::FoundServiceCandidate(self.clone()))
            } else {
                None
            }
        })
    }

    fn applied(&self) -> Option<IdentityManagerProtocol<Deployment, ServiceIdentity>> {
        when_ok!((service_id:IdentityManagerProtocol<Deployment, ServiceIdentity> = self.service_id()) {
            if self.is_candidate() {
                info!("Applied candidate Deployment {}", service_id);
                Some(IdentityManagerProtocol::RequestServiceIdentity {
                    service_candidate: self.clone(),
                })
            } else {
                info!("Ignoring applied Deployment, not a candidate {}", service_id);
                None
            }
        })
    }

    fn deleted(&self) -> Option<IdentityManagerProtocol<Deployment, ServiceIdentity>> {
        when_ok!((service_id:IdentityManagerProtocol<Deployment, ServiceIdentity> = self.service_id()) {
            if self.is_candidate() {
                info!("Deleted candidate Deployment {}", service_id);
                Some(IdentityManagerProtocol::DeletedServiceCandidate(self.clone()))
            } else {
                info!("Ignoring deleted Deployment, not a candidate {}", service_id);
                None
            }
        })
    }

    fn reapplied(&self) -> Option<IdentityManagerProtocol<Deployment, ServiceIdentity>> {
        when_ok!((service_id:IdentityManagerProtocol<Deployment, ServiceIdentity> = self.service_id()) {
            if self.is_candidate() {
                info!("Ignoring reapplied Deployment {}", service_id);
            }
            None
        })
    }

    fn key(&self) -> Option<String> {
        self.service_id().ok()
    }
}
