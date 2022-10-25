use crate::device_id_manager::DeviceIdManagerProtocol;
use k8s_openapi::api::core::v1::Namespace;
use sdp_common::crd::ServiceIdentity;
use sdp_common::traits::Service;
use sdp_common::watcher::SimpleWatchingProtocol;

#[derive(Debug)]
pub enum ServiceIdentityWatcherProtocol {
    DeviceIdManagerReady,
}

impl SimpleWatchingProtocol<DeviceIdManagerProtocol<ServiceIdentity>> for ServiceIdentity {
    fn initialized(
        &self,
        _ns: Option<Namespace>,
    ) -> Option<DeviceIdManagerProtocol<ServiceIdentity>> {
        Some(DeviceIdManagerProtocol::FoundServiceIdentity(self.clone()))
    }

    fn applied(&self) -> Option<DeviceIdManagerProtocol<ServiceIdentity>> {
        Some(DeviceIdManagerProtocol::FoundServiceIdentity(self.clone()))
    }

    fn deleted(&self) -> Option<DeviceIdManagerProtocol<ServiceIdentity>> {
        None
    }

    fn reapplied(&self) -> Option<DeviceIdManagerProtocol<ServiceIdentity>> {
        None
    }

    fn key(&self) -> Option<String> {
        Some(self.service_id())
    }
}
