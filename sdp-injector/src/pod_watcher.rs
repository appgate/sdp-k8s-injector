use k8s_openapi::api::core::v1::{Namespace, Pod};
use sdp_common::crd::ServiceIdentity;
use sdp_common::traits::{Annotated, Candidate, MaybeService};
use sdp_common::watcher::SimpleWatchingProtocol;
use sdp_common::annotations::SDP_ANNOTATION_CLIENT_DEVICE_ID;
use sdp_macros::{logger, sdp_info, sdp_log, sdp_error, when_ok, with_dollar_sign};

use crate::deviceid::DeviceIdProviderRequestProtocol;

logger!("PodWatcher");

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

    fn deleted(
        &self,
        _ns: Option<Namespace>,
    ) -> Option<DeviceIdProviderRequestProtocol<ServiceIdentity>> {
        when_ok!((service_id:DeviceIdProviderRequestProtocol<ServiceIdentity> = self.service_id()) {
            let msg = self
                .is_candidate()
                .then_some(true)
                .and_then(|_| self.annotation(SDP_ANNOTATION_CLIENT_DEVICE_ID))
                .and_then(|uuid_str| {
                    let uuid = uuid::Uuid::parse_str(uuid_str);
                    match uuid {
                        Err(e) => {
                            error!(
                                "Error parsing device id from {}: {}",
                                uuid_str,
                                e.to_string()
                            );
                            None
                        }
                        Ok(uuid) => {
                            info!("Deleted POD with device id assigned {}", &service_id);
                            Some(DeviceIdProviderRequestProtocol::ReleasedDeviceId(
                                service_id.clone(),
                                uuid,
                            ))
                        }
                    }
                });
            if msg.is_none() {
                info!("Ignoring POD {}", service_id);
            }
            msg
        })
    }

    fn reapplied(
        &self,
        _ns: Option<Namespace>,
    ) -> Option<DeviceIdProviderRequestProtocol<ServiceIdentity>> {
        None
    }

    fn key(&self) -> Option<String> {
        self.service_id().ok()
    }
}
