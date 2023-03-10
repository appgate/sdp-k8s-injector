use crate::device_id_manager::DeviceIdManagerProtocol;
use k8s_openapi::api::core::v1::Namespace;
use sdp_common::crd::ServiceIdentity;
use sdp_common::traits::Service;
use sdp_common::watcher::SimpleWatchingProtocol;
use sdp_macros::{logger, with_dollar_sign};

logger!("ServiceIdentityWatcher");

#[derive(Debug, Clone)]
pub enum ServiceIdentityWatcherProtocol {
    DeviceIdManagerReady,
}

/*
 * ServiceIdentity implement SimpleWatchingProtocol for DeviceIdManagerProtocol
 * This watching protocol is used to notify the device-id manager about existing ServiceIdentity
 */
impl SimpleWatchingProtocol<DeviceIdManagerProtocol<ServiceIdentity>> for ServiceIdentity {
    fn initialized(
        &self,
        _ns: Option<Namespace>,
    ) -> Option<DeviceIdManagerProtocol<ServiceIdentity>> {
        Some(DeviceIdManagerProtocol::FoundServiceIdentity(self.clone()))
    }

    fn applied(&self, _ns: Option<Namespace>) -> Option<DeviceIdManagerProtocol<ServiceIdentity>> {
        Some(DeviceIdManagerProtocol::FoundServiceIdentity(self.clone()))
    }

    fn reapplied(
        &self,
        _ns: Option<Namespace>,
    ) -> Option<DeviceIdManagerProtocol<ServiceIdentity>> {
        Some(DeviceIdManagerProtocol::FoundServiceIdentity(self.clone()))
    }

    fn deleted(&self, _ns: Option<Namespace>) -> Option<DeviceIdManagerProtocol<ServiceIdentity>> {
        None
    }

    fn key(&self) -> Option<String> {
        Some(self.service_id())
    }
}
