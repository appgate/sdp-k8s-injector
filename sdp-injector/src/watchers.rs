use k8s_openapi::api::core::v1::{Namespace, Pod};
use log::{error, info};
use sdp_common::annotations::SDP_ANNOTATION_CLIENT_DEVICE_ID;
use sdp_common::crd::{DeviceId, ServiceIdentity};
use sdp_common::traits::{Annotated, Candidate, MaybeService, Service};
use sdp_common::watcher::SimpleWatchingProtocol;
use sdp_macros::when_ok;

use crate::deviceid::DeviceIdProviderRequestProtocol;

impl SimpleWatchingProtocol<DeviceIdProviderRequestProtocol<ServiceIdentity>> for ServiceIdentity {
    fn initialized(
        &self,
        _ns: Option<Namespace>,
    ) -> Option<DeviceIdProviderRequestProtocol<ServiceIdentity>> {
        info!("Recovered ServiceIdentity {}", self.service_id());
        Some(DeviceIdProviderRequestProtocol::FoundServiceIdentity(
            self.clone(),
        ))
    }

    fn applied(
        &self,
        _ns: Option<Namespace>,
    ) -> Option<DeviceIdProviderRequestProtocol<ServiceIdentity>> {
        info!("Applied ServiceIdentity {}", self.service_id());
        Some(DeviceIdProviderRequestProtocol::FoundServiceIdentity(
            self.clone(),
        ))
    }

    fn deleted(
        &self,
        _ns: Option<Namespace>,
    ) -> Option<DeviceIdProviderRequestProtocol<ServiceIdentity>> {
        info!("Deleted ServiceIdentity {}", self.service_id());
        Some(DeviceIdProviderRequestProtocol::DeletedServiceIdentity(
            self.clone(),
        ))
    }

    fn reapplied(
        &self,
        _ns: Option<Namespace>,
    ) -> Option<DeviceIdProviderRequestProtocol<ServiceIdentity>> {
        None
    }

    fn key(&self) -> Option<String> {
        Some(self.service_id())
    }
}

impl SimpleWatchingProtocol<DeviceIdProviderRequestProtocol<ServiceIdentity>> for DeviceId {
    fn initialized(
        &self,
        _ns: Option<Namespace>,
    ) -> Option<DeviceIdProviderRequestProtocol<ServiceIdentity>> {
        info!("Recovered DeviceId {}", self.service_id());
        Some(DeviceIdProviderRequestProtocol::FoundDeviceId(self.clone()))
    }

    fn applied(
        &self,
        _ns: Option<Namespace>,
    ) -> Option<DeviceIdProviderRequestProtocol<ServiceIdentity>> {
        info!("Applied DeviceId {}", self.service_id());
        Some(DeviceIdProviderRequestProtocol::FoundDeviceId(self.clone()))
    }

    fn deleted(
        &self,
        _ns: Option<Namespace>,
    ) -> Option<DeviceIdProviderRequestProtocol<ServiceIdentity>> {
        info!("Deleted DeviceId {}", self.service_id());
        Some(DeviceIdProviderRequestProtocol::DeletedDeviceId(
            self.clone(),
        ))
    }

    fn reapplied(
        &self,
        _ns: Option<Namespace>,
    ) -> Option<DeviceIdProviderRequestProtocol<ServiceIdentity>> {
        None
    }

    fn key(&self) -> Option<String> {
        Some(self.service_id())
    }
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
