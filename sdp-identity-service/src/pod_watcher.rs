use k8s_openapi::api::core::v1::{Namespace, Pod};
use sdp_common::annotations::SDP_ANNOTATION_CLIENT_DEVICE_ID;
use sdp_common::crd::ServiceIdentity;
use sdp_common::service::{ServiceCandidate, ServiceLookup};
use sdp_common::traits::{
    Annotated, Candidate, HasCredentials, MaybeNamespaced, MaybeService, Named, Service,
};
use sdp_common::watcher::SimpleWatchingProtocol;
use sdp_macros::{logger, sdp_error, sdp_info, sdp_log, when_ok, with_dollar_sign};

use crate::identity_manager::IdentityManagerProtocol;

logger!("PodWatcher");

fn device_id_release_message<A: Send + Sync, B: Send + Sync>(
    pod: &Pod,
    namespace: &String,
    name: &String,
    service_id: &String,
) -> Option<IdentityManagerProtocol<A, B>>
where
    A: MaybeService,
    B: Service + HasCredentials,
{
    pod.is_candidate()
        .then_some(true)
        .and_then(|_| pod.annotation(SDP_ANNOTATION_CLIENT_DEVICE_ID))
        .and_then(|uuid_str| {
            let uuid = uuid::Uuid::parse_str(uuid_str);
            match uuid {
                Err(e) => {
                    error!(
                        "[{}] Error parsing DeviceID from {}: {}",
                        service_id,
                        uuid_str,
                        e.to_string()
                    );
                    None
                }
                Ok(uuid) => {
                    info!(
                        "[{}] Deleted POD with DeviceID assigned {}",
                        service_id,
                        &uuid.to_string()
                    );
                    Some(IdentityManagerProtocol::ReleaseDeviceId(
                        ServiceLookup::new(&namespace, name, None),
                        uuid,
                    ))
                }
            }
        })
}

/*
 * Pods implement SimpleWatchingProtocol for DeviceIdProviderRequestProtocol
 * This watching protocol is used to release device ids when pods are deleted
 */
impl SimpleWatchingProtocol<IdentityManagerProtocol<ServiceCandidate, ServiceIdentity>> for Pod {
    fn initialized(
        &self,
        _ns: Option<Namespace>,
    ) -> Option<IdentityManagerProtocol<ServiceCandidate, ServiceIdentity>> {
        None
    }

    fn applied(
        &self,
        ns: Option<Namespace>,
    ) -> Option<IdentityManagerProtocol<ServiceCandidate, ServiceIdentity>> {
        self.reapplied(ns)
    }

    fn reapplied(
        &self,
        _ns: Option<Namespace>,
    ) -> Option<IdentityManagerProtocol<ServiceCandidate, ServiceIdentity>> {
        when_ok!((service_id:IdentityManagerProtocol<ServiceCandidate, ServiceIdentity> = self.service_id()) {
            self.status.as_ref().and_then(|status| {
                if let Some("Evicted") = status.reason.as_ref().map(String::as_str) {
                    info!("[{}] Evicted POD: {} ", service_id,
                        status.message.as_ref().map(String::as_str).unwrap_or("No message"));
                    device_id_release_message(self, &self.namespace().unwrap(), &self.name(), &service_id)
                } else {
                    None
                }
            })
        })
    }

    fn deleted(
        &self,
        _ns: Option<Namespace>,
    ) -> Option<IdentityManagerProtocol<ServiceCandidate, ServiceIdentity>> {
        when_ok!((service_id:IdentityManagerProtocol<ServiceCandidate, ServiceIdentity> = self.service_id()) {
            let msg = device_id_release_message(self, &self.namespace().unwrap(), &self.name(), &service_id);
            if msg.is_none() {
                info!("[{}] Ignoring Pod {}", service_id, service_id);
            }
            msg
        })
    }

    fn key(&self) -> Option<String> {
        self.service_id().ok()
    }
}
