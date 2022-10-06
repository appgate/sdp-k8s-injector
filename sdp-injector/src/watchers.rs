use futures::{StreamExt, TryStreamExt};
use k8s_openapi::api::core::v1::Pod;
use kube::Resource;
use kube::{
    api::ListParams,
    runtime::watcher::{self, Event},
    Api,
};
use log::{error, info};
use sdp_common::constants::POD_DEVICE_ID_ANNOTATION;
use sdp_common::crd::{DeviceId, ServiceIdentity};
use sdp_common::traits::{Annotated, Candidate, Service};
use sdp_macros::when_ok;
use serde::de::DeserializeOwned;
use std::fmt::Debug;
use tokio::sync::mpsc::Sender;

use crate::deviceid::DeviceIdProviderRequestProtocol;

pub struct Watcher<E, P> {
    pub api: Api<E>,
    pub queue_tx: Sender<P>,
}

pub trait SimpleWatchingProtocol<P> {
    fn applied(&self) -> Option<P>;
    fn deleted(&self) -> Option<P>;
}

pub async fn watch<E, P>(simple_watcher: Watcher<E, P>) -> ()
where
    E: Clone + Debug + Send + DeserializeOwned + Resource + SimpleWatchingProtocol<P> + 'static,
{
    let xs = watcher::watcher(simple_watcher.api, ListParams::default());
    let mut xs = xs.boxed();
    loop {
        match xs.try_next().await {
            Ok(Some(Event::Applied(e))) => {
                if let Some(msg) = e.applied() {
                    if let Err(e) = simple_watcher.queue_tx.send(msg).await {
                        error!("Error sending Applied message: {}", e.to_string())
                    }
                }
            }
            Ok(Some(Event::Deleted(e))) => {
                if let Some(msg) = e.deleted() {
                    if let Err(e) = simple_watcher.queue_tx.send(msg).await {
                        error!("Error sending Deleted message: {}", e.to_string())
                    }
                }
            }
            Ok(Some(Event::Restarted(_))) => {}
            Ok(None) => {}
            Err(e) => {
                error!("Error reading ServiceIdentity events: {}", e.to_string());
            }
        }
    }
}

impl SimpleWatchingProtocol<DeviceIdProviderRequestProtocol<ServiceIdentity>> for ServiceIdentity {
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
    fn applied(&self) -> Option<DeviceIdProviderRequestProtocol<ServiceIdentity>> {
        when_ok!((service_id:DeviceIdProviderRequestProtocol<ServiceIdentity> = self.service_id()) {
            info!("Applied DeviceId {}", service_id);
            Some(DeviceIdProviderRequestProtocol::FoundDevideId(self.clone()))
        })
    }

    fn deleted(&self) -> Option<DeviceIdProviderRequestProtocol<ServiceIdentity>> {
        when_ok!((service_id:DeviceIdProviderRequestProtocol<ServiceIdentity> = self.service_id()) {
            info!("Deleted DeviceId {}", service_id);
            Some(DeviceIdProviderRequestProtocol::DeletedDevideId(
                self.clone(),
            ))
        })
    }
}

impl SimpleWatchingProtocol<DeviceIdProviderRequestProtocol<ServiceIdentity>> for Pod {
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
