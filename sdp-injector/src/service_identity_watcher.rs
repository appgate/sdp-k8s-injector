use k8s_openapi::api::core::v1::Namespace;
use sdp_common::crd::ServiceIdentity;
use sdp_common::traits::Service;
use sdp_common::watcher::SimpleWatchingProtocol;
use sdp_macros::{logger, sdp_info, sdp_log, with_dollar_sign};

use crate::deviceid::DeviceIdProviderRequestProtocol;

logger!("ServiceIdentityWatcher");

/*
 * ServiceIdentity implement SimpleWatchingProtocol for DeviceIdProviderRequestProtocol
 * This watching protocol is used to register new ServiceIdentity created so they can later
 * be associated with new pods
 * TODO: This is so close to the implementation for DeviceId, a macro to redue code would be nice
 */
impl SimpleWatchingProtocol<DeviceIdProviderRequestProtocol<ServiceIdentity>> for ServiceIdentity {
    fn initialized(
        &self,
        _ns: Option<Namespace>,
    ) -> Option<DeviceIdProviderRequestProtocol<ServiceIdentity>> {
        info!(
            "[{}] Recovered ServiceIdentity {}",
            self.service_id(),
            self.service_id()
        );
        Some(DeviceIdProviderRequestProtocol::FoundServiceIdentity(
            self.clone(),
        ))
    }

    fn applied(
        &self,
        _ns: Option<Namespace>,
    ) -> Option<DeviceIdProviderRequestProtocol<ServiceIdentity>> {
        info!(
            "[{}] Applied ServiceIdentity {}",
            self.service_id(),
            self.service_id()
        );
        Some(DeviceIdProviderRequestProtocol::FoundServiceIdentity(
            self.clone(),
        ))
    }

    fn reapplied(
        &self,
        _ns: Option<Namespace>,
    ) -> Option<DeviceIdProviderRequestProtocol<ServiceIdentity>> {
        None
    }

    fn deleted(
        &self,
        _ns: Option<Namespace>,
    ) -> Option<DeviceIdProviderRequestProtocol<ServiceIdentity>> {
        info!(
            "[{}] Deleted ServiceIdentity {}",
            self.service_id(),
            self.service_id()
        );
        Some(DeviceIdProviderRequestProtocol::DeletedServiceIdentity(
            self.clone(),
        ))
    }

    fn key(&self) -> Option<String> {
        Some(self.service_id())
    }
}
