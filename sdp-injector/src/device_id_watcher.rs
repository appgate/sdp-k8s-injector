use k8s_openapi::api::core::v1::Namespace;
use sdp_common::crd::{DeviceId, ServiceIdentity};
use sdp_common::traits::Service;
use sdp_common::watcher::SimpleWatchingProtocol;
use sdp_macros::{logger, sdp_info, sdp_log, with_dollar_sign};

use crate::deviceid::DeviceIdProviderRequestProtocol;

logger!("DeviceIDWatcher");

/*
 * DeviceId implement SimpleWatchingProtocol for DeviceIdProviderRequestProtocol
 * This watching protocol is used to register new DeviceId created so they can later
 * be assigned to new pods
 */
impl SimpleWatchingProtocol<DeviceIdProviderRequestProtocol<ServiceIdentity>> for DeviceId {
    fn initialized(
        &self,
        _ns: Option<Namespace>,
    ) -> Option<DeviceIdProviderRequestProtocol<ServiceIdentity>> {
        info!(
            "[{}] Recovered DeviceId {}, ignoring",
            self.service_id(),
            self.service_id()
        );
        None
    }

    fn applied(
        &self,
        _ns: Option<Namespace>,
    ) -> Option<DeviceIdProviderRequestProtocol<ServiceIdentity>> {
        info!(
            "[{}] Applied DeviceId {}, ignoring",
            self.service_id(),
            self.service_id()
        );
        None
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
            "[{}] Deleted DeviceId {}, ignoring",
            self.service_id(),
            self.service_id()
        );
        None
    }

    fn key(&self) -> Option<String> {
        Some(self.service_id())
    }
}
