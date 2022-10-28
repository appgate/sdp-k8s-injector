use k8s_openapi::api::core::v1::Namespace;
use sdp_common::crd::{DeviceId, ServiceIdentity};
use sdp_common::traits::Service;
use sdp_common::watcher::SimpleWatchingProtocol;
use sdp_macros::{logger, sdp_info, sdp_log, with_dollar_sign};

use crate::deviceid::DeviceIdProviderRequestProtocol;

logger!("DeviceIDWatcher");

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
