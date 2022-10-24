use k8s_openapi::api::core::v1::Pod;
use log::{error, info};
use sdp_common::constants::POD_DEVICE_ID_ANNOTATION;
use sdp_common::crd::{DeviceId, ServiceIdentity};
use sdp_common::traits::{Annotated, Candidate, Service};
use sdp_common::watcher::SimpleWatchingProtocol;
use sdp_macros::when_ok;

use crate::deviceid::DeviceIdProviderRequestProtocol;

impl SimpleWatchingProtocol<DeviceIdProviderRequestProtocol<ServiceIdentity>> for ServiceIdentity {
    fn initialized(&self) -> Option<DeviceIdProviderRequestProtocol<ServiceIdentity>> {
        when_ok!((service_id:DeviceIdProviderRequestProtocol<ServiceIdentity> = self.service_id()) {
            info!("Recovered ServiceIdentity {}", service_id);
            Some(DeviceIdProviderRequestProtocol::FoundServiceIdentity(
                self.clone(),
            ))
        })
    }

    fn applied(&self) -> Option<DeviceIdProviderRequestProtocol<ServiceIdentity>> {
        when_ok!((service_id:DeviceIdProviderRequestProtocol<ServiceIdentity> = self.service_id()) {
            info!("Applied ServiceIdentity {}", service_id);
            Some(DeviceIdProviderRequestProtocol::FoundServiceIdentity(
                self.clone(),
            ))
        })
    }

    fn deleted(&self) -> Option<DeviceIdProviderRequestProtocol<ServiceIdentity>> {
        when_ok!((service_id:DeviceIdProviderRequestProtocol<ServiceIdentity> = self.service_id()) {
            info!("Deleted ServiceIdentity {}", service_id);
            Some(DeviceIdProviderRequestProtocol::DeletedServiceIdentity(
                self.clone(),
            ))
        })
    }
}

impl SimpleWatchingProtocol<DeviceIdProviderRequestProtocol<ServiceIdentity>> for DeviceId {
    fn initialized(&self) -> Option<DeviceIdProviderRequestProtocol<ServiceIdentity>> {
        when_ok!((service_id:DeviceIdProviderRequestProtocol<ServiceIdentity> = self.service_id()) {
            info!("Recovered DeviceId {}", service_id);
            Some(DeviceIdProviderRequestProtocol::FoundDeviceId(self.clone()))
        })
    }

    fn applied(&self) -> Option<DeviceIdProviderRequestProtocol<ServiceIdentity>> {
        when_ok!((service_id:DeviceIdProviderRequestProtocol<ServiceIdentity> = self.service_id()) {
            info!("Applied DeviceId {}", service_id);
            Some(DeviceIdProviderRequestProtocol::FoundDeviceId(self.clone()))
        })
    }

    fn deleted(&self) -> Option<DeviceIdProviderRequestProtocol<ServiceIdentity>> {
        when_ok!((service_id:DeviceIdProviderRequestProtocol<ServiceIdentity> = self.service_id()) {
            info!("Deleted DeviceId {}", service_id);
            Some(DeviceIdProviderRequestProtocol::DeletedDeviceId(
                self.clone(),
            ))
        })
    }
}

impl SimpleWatchingProtocol<DeviceIdProviderRequestProtocol<ServiceIdentity>> for Pod {
    fn initialized(&self) -> Option<DeviceIdProviderRequestProtocol<ServiceIdentity>> {
        None
    }

    fn applied(&self) -> Option<DeviceIdProviderRequestProtocol<ServiceIdentity>> {
        None
    }

    fn deleted(&self) -> Option<DeviceIdProviderRequestProtocol<ServiceIdentity>> {
        when_ok!((service_id:DeviceIdProviderRequestProtocol<ServiceIdentity> = self.service_id()) {
            let msg = self
                .is_candidate()
                .then_some(true)
                .and_then(|_| self.annotation(POD_DEVICE_ID_ANNOTATION))
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
}
