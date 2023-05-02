use k8s_openapi::api::core::v1::Namespace;
use sdp_common::crd::AssignedDeviceId;
use sdp_common::watcher::SimpleWatchingProtocol;
use sdp_macros::{logger, sdp_info, sdp_log, with_dollar_sign};

use crate::identity_creator::IdentityCreatorProtocol;

logger!("AssignedDeviceIdWatcher");

impl SimpleWatchingProtocol<IdentityCreatorProtocol> for AssignedDeviceId {
    fn initialized(&self, _ns: Option<Namespace>) -> Option<IdentityCreatorProtocol> {
        None
    }

    fn applied(&self, _ns: Option<Namespace>) -> Option<IdentityCreatorProtocol> {
        None
    }

    fn reapplied(&self, _ns: Option<Namespace>) -> Option<IdentityCreatorProtocol> {
        None
    }

    fn deleted(&self, _ns: Option<Namespace>) -> Option<IdentityCreatorProtocol> {
        info!(
            "[{}] Deleted AssignedDeviceId {}",
            self.spec.device_id, self.spec.distinguished_name
        );
        Some(IdentityCreatorProtocol::ReleaseAssignedDeviceId(
            self.clone(),
        ))
    }

    fn key(&self) -> Option<String> {
        None
    }
}
