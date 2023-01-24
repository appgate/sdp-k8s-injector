use k8s_openapi::api::core::v1::{Namespace, Pod};
use sdp_common::annotations::SDP_ANNOTATION_CLIENT_DEVICE_ID;
use sdp_common::crd::ServiceIdentity;
use sdp_common::traits::{Annotated, Candidate, MaybeService};
use sdp_common::watcher::SimpleWatchingProtocol;
use sdp_macros::{logger, sdp_error, sdp_info, sdp_log, when_ok, with_dollar_sign};
use uuid::Uuid;

use crate::deviceid::DeviceIdProviderRequestProtocol;

logger!("PodWatcher");

fn get_device_id(
    pod: &Pod,
    service_id: &String,
) -> Option<DeviceIdProviderRequestProtocol<ServiceIdentity>> {
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
                        service_id, &service_id
                    );
                    Some(DeviceIdProviderRequestProtocol::ReleasedDeviceId(
                        service_id.clone(),
                        uuid,
                    ))
                }
            }
        })
}

impl SimpleWatchingProtocol<DeviceIdProviderRequestProtocol<ServiceIdentity>> for Pod {
    fn initialized(
        &self,
        _ns: Option<Namespace>,
    ) -> Option<DeviceIdProviderRequestProtocol<ServiceIdentity>> {
        None
    }

    fn applied(
        &self,
        _ns: Option<Namespace>,
    ) -> Option<DeviceIdProviderRequestProtocol<ServiceIdentity>> {
        None
    }

    fn reapplied(
        &self,
        _ns: Option<Namespace>,
    ) -> Option<DeviceIdProviderRequestProtocol<ServiceIdentity>> {
        when_ok!((service_id:DeviceIdProviderRequestProtocol<ServiceIdentity> = self.service_id()) {
            self.status.as_ref().and_then(|status| {
                if let Some("Evicted") = status.reason.as_ref().map(String::as_str) {
                    info!("[{}] Evicted POD: {} ", service_id,
                        status.message.as_ref().map(String::as_str).unwrap_or("No message"));
                    get_device_id(self, &service_id)
                } else {
                    None
                }
            })
        })
    }

    fn deleted(
        &self,
        _ns: Option<Namespace>,
    ) -> Option<DeviceIdProviderRequestProtocol<ServiceIdentity>> {
        when_ok!((service_id:DeviceIdProviderRequestProtocol<ServiceIdentity> = self.service_id()) {
            let msg = get_device_id(self, &service_id);
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
