use std::collections::{HashMap, HashSet};
use std::pin::Pin;

use futures::Future;
use log::{error, info, warn};
use sdp_common::crd::DeviceId;
use sdp_common::service::ServiceIdentity;
use sdp_common::traits::{HasCredentials, Service};
use sdp_macros::when_ok;
use tokio::sync::mpsc::{Receiver, Sender};
use uuid::Uuid;

#[derive(Debug)]
pub enum DeviceIdProviderRequestProtocol<A: Service + HasCredentials> {
    FoundServiceIdentity(A),
    FoundDevideId(DeviceId),
    DeletedServiceIdentity(A),
    DeletedDevideId(DeviceId),
    RequestDeviceId(Sender<DeviceIdProviderResponseProtocol<A>>, String),
    ReleasedDeviceId(String, Uuid),
}

pub enum DeviceIdProviderResponseProtocol<A: Service + HasCredentials> {
    AssignedDeviceId(A, Uuid),
    NotFound,
}

#[derive(Debug)]
pub struct RegisteredDeviceId(usize, HashSet<Uuid>, usize, Vec<Uuid>);

pub trait IdentityStore<A: Service + HasCredentials>: Send + Sync {
    fn pop_device_id<'a>(
        &'a mut self,
        service_id: &'a str,
    ) -> Pin<Box<dyn Future<Output = Option<(A, Uuid)>> + Send + '_>>;

    fn push_device_id<'a>(
        &'a mut self,
        service_id: &'a str,
        uuid: Uuid,
    ) -> Pin<Box<dyn Future<Output = Option<Uuid>> + Send + '_>>;

    fn register_service(&mut self, service: A) -> Pin<Box<dyn Future<Output = ()> + Send + '_>>;

    fn register_device_ids(
        &mut self,
        device_id: DeviceId,
    ) -> Pin<Box<dyn Future<Output = ()> + Send + '_>>;

    fn unregister_service<'a>(
        &'a mut self,
        service_id: &'a str,
    ) -> Pin<Box<dyn Future<Output = Option<(A, RegisteredDeviceId)>> + Send + '_>>;

    fn unregister_device_ids<'a>(
        &'a mut self,
        device_id: &'a str,
    ) -> Pin<Box<dyn Future<Output = Option<(A, RegisteredDeviceId)>> + Send + '_>>;
}

pub struct DeviceIdProvider<A: Service + HasCredentials> {
    store: Box<dyn IdentityStore<A> + Send>,
}

#[derive(Default)]
pub struct InMemoryIdentityStore {
    identities: HashMap<String, ServiceIdentity>,
    device_ids: HashMap<String, RegisteredDeviceId>,
}

impl InMemoryIdentityStore {
    fn new() -> Self {
        InMemoryIdentityStore {
            identities: HashMap::new(),
            device_ids: HashMap::new(),
        }
    }
}

impl IdentityStore<ServiceIdentity> for InMemoryIdentityStore {
    fn register_service(
        &mut self,
        service: ServiceIdentity,
    ) -> Pin<Box<dyn Future<Output = ()> + Send + '_>> {
        let fut = async move {
            when_ok!((service_id = service.service_id()) {
                self.identities.insert(service_id, service)
            });
        };
        Box::pin(fut)
    }

    fn register_device_ids(
        &mut self,
        device_id: DeviceId,
    ) -> Pin<Box<dyn Future<Output = ()> + Send + '_>> {
        let fut = async move {
            let uuids = device_id
                .spec
                .uuids
                .iter()
                .map(|s| {
                    Uuid::try_parse(s)
                        .map_err(|e| {
                            error!("Unable to parse Uuid {}: {}", s, e.to_string());
                        })
                        .map(Some)
                        .unwrap_or(None)
                })
                .filter(|u| u.is_some())
                .map(Option::unwrap);
            let uuids: Vec<Uuid> = uuids.collect();
            let n = uuids.len();
            when_ok!((service_id = device_id.service_id()) {
                self.device_ids.insert(
                    service_id,
                    RegisteredDeviceId(n, HashSet::from_iter(uuids.clone()), n, uuids),
                )
            });
        };
        Box::pin(fut)
    }

    fn unregister_service<'a>(
        &'a mut self,
        service_id: &'a str,
    ) -> Pin<Box<dyn Future<Output = Option<(ServiceIdentity, RegisteredDeviceId)>> + Send + '_>>
    {
        let fut = async move {
            let device_id = self.device_ids.remove(service_id);
            let service_id = self.identities.remove(service_id);
            match (service_id, device_id) {
                (Some(sid), Some(did)) => Some((sid, did)),
                _ => None,
            }
        };
        Box::pin(fut)
    }

    fn unregister_device_ids<'a>(
        &'a mut self,
        device_id: &'a str,
    ) -> Pin<Box<dyn Future<Output = Option<(ServiceIdentity, RegisteredDeviceId)>> + Send + '_>>
    {
        let fut = async move {
            let service_id = self.identities.remove(device_id);
            let device_id = self.device_ids.remove(device_id);
            match (service_id, device_id) {
                (Some(sid), Some(did)) => Some((sid, did)),
                _ => None,
            }
        };
        Box::pin(fut)
    }

    fn pop_device_id<'a>(
        &'a mut self,
        service_id: &'a str,
    ) -> Pin<Box<dyn Future<Output = Option<(ServiceIdentity, Uuid)>> + Send + '_>> {
        let fut = async move {
            let sid = self.identities.get(&service_id.to_string());
            let ds = self.device_ids.get(&service_id.to_string());
            match (sid, ds) {
                (
                    Some(sid),
                    Some(RegisteredDeviceId(
                        n_all_uuids,
                        all_uuids,
                        n_available_uuids,
                        available_uuids,
                    )),
                ) => {
                    if *n_available_uuids == 0 {
                        error!(
                            "Requested device id for {} but no device ids are available",
                            service_id
                        );
                        None
                    } else {
                        // Get first uuid and update current data
                        let mut available_uuids = available_uuids.clone();
                        let uuid = available_uuids.remove(0);
                        let available_uuids = available_uuids.clone();
                        info!("Device id {} assigned to service {}", uuid, service_id);
                        info!(
                            "Service {} has {} device ids available {:?}",
                            service_id,
                            available_uuids.len(),
                            &available_uuids
                        );
                        self.device_ids.insert(
                            service_id.to_string(),
                            RegisteredDeviceId(
                                *n_all_uuids,
                                all_uuids.clone(),
                                available_uuids.len(),
                                available_uuids,
                            ),
                        );
                        Some((sid.clone(), uuid))
                    }
                }
                _ => {
                    warn!("Service {} is not registered ({}) or device ids for service are not registered ({})", service_id, sid.is_none(), ds.is_none());
                    None
                }
            }
        };
        Box::pin(fut)
    }

    fn push_device_id<'a>(
        &'a mut self,
        service_id: &'a str,
        uuid: Uuid,
    ) -> Pin<Box<dyn Future<Output = Option<Uuid>> + Send + '_>> {
        let fut = async move {
            let service_id = service_id.to_string();
            match (
                self.identities.get(&service_id),
                self.device_ids.get(&service_id),
            ) {
                (Some(_), Some(RegisteredDeviceId(n_all, all, n_available, available)))
                    if all.contains(&uuid) =>
                {
                    if n_available < n_all {
                        if !available.contains(&uuid) {
                            info!("Pushing device id {} to the list of available device ids for service {}", uuid, service_id);
                            let mut available = available.clone();
                            available.push(uuid.clone());
                            self.device_ids.insert(
                                service_id.to_string(),
                                RegisteredDeviceId(
                                    *n_all,
                                    all.clone(),
                                    *n_available + 1,
                                    available.to_vec(),
                                ),
                            );
                            Some(uuid.clone())
                        } else {
                            warn!("Device id {} is already in the list of available device-ids for service {}", uuid, service_id);
                            None
                        }
                    } else {
                        warn!("Unable to push device id {} into the list of available devices for service {}. List of devices is complete.", uuid, service_id);
                        None
                    }
                }
                (Some(_), Some(RegisteredDeviceId(_, _, _, _))) => {
                    error!(
                        "Device id {} is not in the list of allowed device ids for service {}",
                        uuid, service_id
                    );
                    None
                }
                (None, _) => {
                    error!("Service id {} is not registered", service_id);
                    None
                }
                (_, None) => {
                    error!("Service id {} has not device ids registered", service_id);
                    None
                }
            }
        };
        Box::pin(fut)
    }
}

impl DeviceIdProvider<ServiceIdentity> {
    pub async fn run(
        &mut self,
        mut provider_rx: Receiver<DeviceIdProviderRequestProtocol<ServiceIdentity>>,
        mut watcher_rx: Receiver<DeviceIdProviderRequestProtocol<ServiceIdentity>>,
    ) {
        info!("Starting device id provider");
        loop {
            tokio::select! {
                val = watcher_rx.recv() => {
                    match val {
                        Some(DeviceIdProviderRequestProtocol::FoundServiceIdentity(s)) => {
                            when_ok!((service_id = s.service_id()) {
                                info!("Registering new service {}", &service_id);
                                self.store.register_service(s).await;
                            });
                        },
                        Some(DeviceIdProviderRequestProtocol::DeletedServiceIdentity(s)) => {
                            when_ok!((service_id = s.service_id()) {
                                info!("Unregistering service {}", service_id);
                                if self.store.unregister_service(&service_id).await.is_none() {
                                    error!("Unable to unregister service {}", service_id);
                                };
                            });
                        },
                        Some(DeviceIdProviderRequestProtocol::FoundDevideId(id)) => {
                            when_ok!((service_id = id.service_id()) {
                                info!("Registering new device ids for service {}", service_id);
                                self.store.register_device_ids(id).await;
                            });
                        },
                        Some(DeviceIdProviderRequestProtocol::DeletedDevideId(id)) => {
                            when_ok!((service_id = id.service_id()) {
                                info!("Unregistering device ids for service {}", service_id);
                                if self.store.unregister_device_ids(&service_id).await.is_none() {
                                    error!("Unable to unregister device ids for service {}", service_id);
                                };
                            });
                        },
                        Some(DeviceIdProviderRequestProtocol::ReleasedDeviceId(service_id, uuid)) => {
                            info!("Released device id {} for service {}", uuid.to_string(), service_id);
                            if self.store.push_device_id(&service_id, uuid).await.is_none() {
                                error!("Unable to release device id {} for service {}", uuid, service_id);
                            }
                        },
                        Some(ev) => {
                            warn!("Ignored event {:?}", ev);
                        }
                        None => {
                            warn!("Ignored None event");
                        },
                    }
                },
                val = provider_rx.recv() => {
                    match val {
                        Some(DeviceIdProviderRequestProtocol::RequestDeviceId(q_tx, service_id)) => {
                            let msg = if let Some((service_identity, uuid)) = self.store.pop_device_id(&service_id).await {
                                info!("Requested device id for service {}", service_id);
                                DeviceIdProviderResponseProtocol::AssignedDeviceId(service_identity.clone(), uuid)
                            } else {
                                warn!("Requested device id for service {} not found", service_id);
                                DeviceIdProviderResponseProtocol::NotFound
                            };
                            if let Err(e) = q_tx.send(msg).await {
                                error!("Error assigning devide id: {}", e.to_string());
                            }
                        },
                        Some(ev) => {
                            info!("Ignored event {:?}", ev);
                        },
                        None => {
                            warn!("Event not found");
                        }
                    }
                }
            }
        }
    }

    pub fn new(store: Option<Box<dyn IdentityStore<ServiceIdentity> + Send>>) -> Self {
        DeviceIdProvider {
            store: store.unwrap_or(Box::new(InMemoryIdentityStore::new())),
        }
    }
}
