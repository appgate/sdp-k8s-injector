use std::collections::{HashMap, HashSet};
use std::pin::Pin;

use futures::Future;
use sdp_common::crd::DeviceId;
use sdp_common::errors::SDPServiceError;
use sdp_common::service::ServiceIdentity;
use sdp_common::traits::{HasCredentials, Service};
use sdp_macros::{logger, sdp_error, sdp_info, sdp_log, sdp_warn, with_dollar_sign};
use tokio::sync::mpsc::{Receiver, Sender};
use uuid::Uuid;

logger!("DeviceIDProvider");

#[derive(Debug)]
pub enum DeviceIdProviderRequestProtocol<A: Service + HasCredentials> {
    FoundServiceIdentity(A),
    FoundDeviceId(DeviceId),
    DeletedServiceIdentity(A),
    DeletedDeviceId(DeviceId),
    RequestDeviceId(Sender<DeviceIdProviderResponseProtocol<A>>, String),
    ReleasedDeviceId(String, Uuid),
}

#[derive(PartialEq)]
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
    ) -> Pin<Box<dyn Future<Output = Result<(ServiceIdentity, Uuid), SDPServiceError>> + Send + '_>>;

    fn push_device_id<'a>(
        &'a mut self,
        service_id: &'a str,
        uuid: Uuid,
    ) -> Pin<Box<dyn Future<Output = Result<Option<Uuid>, SDPServiceError>> + Send + '_>>;

    fn register_service(
        &mut self,
        service: A,
    ) -> Pin<Box<dyn Future<Output = Result<Option<A>, SDPServiceError>> + Send + '_>>;

    fn register_device_ids(
        &mut self,
        device_id: DeviceId,
    ) -> Pin<
        Box<dyn Future<Output = Result<Option<RegisteredDeviceId>, SDPServiceError>> + Send + '_>,
    >;

    fn unregister_service<'a>(
        &'a mut self,
        service_id: &'a str,
    ) -> Pin<
        Box<
            dyn Future<Output = Result<Option<(A, RegisteredDeviceId)>, SDPServiceError>>
                + Send
                + '_,
        >,
    >;

    fn unregister_device_ids<'a>(
        &'a mut self,
        device_id: &'a str,
    ) -> Pin<
        Box<
            dyn Future<Output = Result<Option<(A, RegisteredDeviceId)>, SDPServiceError>>
                + Send
                + '_,
        >,
    >;
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
    ) -> Pin<Box<dyn Future<Output = Result<Option<ServiceIdentity>, SDPServiceError>> + Send + '_>>
    {
        let fut = async move { Ok(self.identities.insert(service.service_id(), service)) };
        Box::pin(fut)
    }

    fn register_device_ids(
        &mut self,
        device_id: DeviceId,
    ) -> Pin<
        Box<dyn Future<Output = Result<Option<RegisteredDeviceId>, SDPServiceError>> + Send + '_>,
    > {
        let fut = async move {
            let mut errors = Vec::new();
            let mut uuids = Vec::new();
            for uuid in &device_id.spec.uuids {
                match Uuid::try_parse(&uuid) {
                    Ok(uuid) => {
                        uuids.push(uuid);
                    }
                    Err(_) => {
                        errors.push(uuid.clone());
                    }
                }
            }
            if !errors.is_empty() {
                Err(SDPServiceError::from_string(format!(
                    "Unable to parse uuids: {}",
                    errors.join(",")
                )))
            } else {
                let n = uuids.len();
                Ok(self.device_ids.insert(
                    device_id.service_id(),
                    RegisteredDeviceId(n, HashSet::from_iter(uuids.clone()), n, uuids),
                ))
            }
        };
        Box::pin(fut)
    }

    fn unregister_service<'a>(
        &'a mut self,
        service_id: &'a str,
    ) -> Pin<
        Box<
            dyn Future<
                    Output = Result<Option<(ServiceIdentity, RegisteredDeviceId)>, SDPServiceError>,
                > + Send
                + '_,
        >,
    > {
        let fut = async move {
            let device_id = self.device_ids.remove(service_id);
            let service_id = self.identities.remove(service_id);
            match (service_id, device_id) {
                (Some(sid), Some(did)) => Ok(Some((sid, did))),
                _ => Ok(None),
            }
        };
        Box::pin(fut)
    }

    fn unregister_device_ids<'a>(
        &'a mut self,
        device_id: &'a str,
    ) -> Pin<
        Box<
            dyn Future<
                    Output = Result<Option<(ServiceIdentity, RegisteredDeviceId)>, SDPServiceError>,
                > + Send
                + '_,
        >,
    > {
        let fut = async move {
            let service_id = self.identities.remove(device_id);
            let device_id = self.device_ids.remove(device_id);
            match (service_id, device_id) {
                (Some(sid), Some(did)) => Ok(Some((sid, did))),
                _ => Ok(None),
            }
        };
        Box::pin(fut)
    }

    fn pop_device_id<'a>(
        &'a mut self,
        service_id: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<(ServiceIdentity, Uuid), SDPServiceError>> + Send + '_>>
    {
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
                    let (uuid, available_uuids) = if *n_available_uuids == 0 {
                        format!(
                            "Requested device id for {} but no device ids are available, creating dynamically a new one!",
                            service_id
                        );
                        (Uuid::new_v4(), available_uuids.clone())
                    } else {
                        // Get first uuid
                        let mut available_uuids = available_uuids.clone();
                        let uuid = available_uuids.remove(0);
                        (uuid, available_uuids)
                    };
                    info!(
                        "[{}] DeviceID {} assigned to service {}",
                        service_id, uuid, service_id
                    );
                    info!(
                        "[{}] Service {} has {} DeviceIDs available {:?}",
                        service_id,
                        service_id,
                        available_uuids.len(),
                        &available_uuids
                    );
                    // Update current data
                    self.device_ids.insert(
                        service_id.to_string(),
                        RegisteredDeviceId(
                            *n_all_uuids,
                            all_uuids.clone(),
                            available_uuids.len(),
                            available_uuids,
                        ),
                    );
                    Ok((sid.clone(), uuid))
                }
                _ => {
                    if sid.is_none() {
                        error!(
                            "[{}] ServiceIdentity does not exist for service {}",
                            service_id, service_id
                        );
                    }
                    if ds.is_none() {
                        error!(
                            "[{}] DeviceID does not exist for service {}",
                            service_id, service_id
                        );
                    }
                    Err(SDPServiceError::from_string(format!(
                        "ServiceIdentity and/or DeviceId is missing for service {}",
                        service_id
                    )))
                }
            }
        };
        Box::pin(fut)
    }

    fn push_device_id<'a>(
        &'a mut self,
        service_id: &'a str,
        uuid: Uuid,
    ) -> Pin<Box<dyn Future<Output = Result<Option<Uuid>, SDPServiceError>> + Send + '_>> {
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
                            info!("[{}] Pushing DeviceID {} to the list of available DeviceIDs for service {}", service_id, uuid, service_id);
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
                            Ok(Some(uuid.clone()))
                        } else {
                            warn!("[{}] DeviceID {} is already in the list of available device-ids for service {}", service_id, uuid, service_id);
                            Ok(None)
                        }
                    } else {
                        warn!("[{}] Unable to push DeviceID {} into the list of available devices for service {}. List of devices is complete.", service_id, uuid, service_id);
                        Ok(None)
                    }
                }
                (Some(_), Some(RegisteredDeviceId(_, _, _, _))) => {
                    Err(SDPServiceError::from_string(format!(
                        "Device id {} is not in the list of allowed device ids for service {}",
                        uuid, service_id
                    )))
                }
                (None, _) => Err(SDPServiceError::from_string(format!(
                    "Service id {} is not registered",
                    service_id
                ))),
                (_, None) => Err(SDPServiceError::from_string(format!(
                    "Service id {} has not device ids registered",
                    service_id
                ))),
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
        info!("Starting DeviceID Provider");
        loop {
            tokio::select! {
                val = watcher_rx.recv() => {
                    match val {
                        Some(DeviceIdProviderRequestProtocol::FoundServiceIdentity(s)) => {
                            let service_id = s.service_id();
                            info!("[{}] Registering new service {}", service_id, &service_id);
                            if let Err(e) = self.store.register_service(s).await {
                                error!("[{}] Unable to register service identity {}: {}", service_id, service_id, e);
                            }
                        },
                        Some(DeviceIdProviderRequestProtocol::DeletedServiceIdentity(s)) => {
                            let service_id = s.service_id();
                            info!("[{}] Unregistering service {}", service_id, service_id);
                            if let Err(err) = self.store.unregister_service(&service_id).await {
                                error!("[{}] Unable to unregister service {}: {}", service_id, service_id, err);
                            };
                        },
                        Some(DeviceIdProviderRequestProtocol::FoundDeviceId(id)) => {
                            let service_id = id.service_id();
                            info!("[{}] Registering new DeviceID for service {}", service_id, service_id);
                            if let Err(err) = self.store.register_device_ids(id).await {
                                error!("[{}] Unable to register DeviceID {}: {}", service_id, service_id, err);
                            }
                        },
                        Some(DeviceIdProviderRequestProtocol::DeletedDeviceId(id)) => {
                            let service_id = id.service_id();
                            info!("[{}] Unregistering DeviceID for service {}", service_id, service_id);
                            if let Err(err) = self.store.unregister_device_ids(&service_id).await {
                                error!("[{}] Unable to unregister DeviceID for service {}: {}", service_id, service_id, err);
                            };
                        },
                        Some(DeviceIdProviderRequestProtocol::ReleasedDeviceId(service_id, uuid)) => {
                            info!("[{}] Released DeviceID {} for service {}", service_id, uuid.to_string(), service_id);
                            if let Err(err) = self.store.push_device_id(&service_id, uuid).await {
                                error!("[{}] Unable to release DeviceID {} for service {}: {}", service_id, uuid, service_id, err);
                            }
                        },
                        Some(_) => {}
                        None => {},
                    }
                },
                val = provider_rx.recv() => {
                    match val {
                        Some(DeviceIdProviderRequestProtocol::RequestDeviceId(q_tx, service_id)) => {
                            let msg = match self.store.pop_device_id(&service_id).await {
                                Ok((service_identity, uuid)) =>  {
                                    info!("[{}] Requested DeviceID for service {}", service_id, service_id);
                                    Some(DeviceIdProviderResponseProtocol::AssignedDeviceId(service_identity.clone(), uuid))
                                },
                                Err(e) => {
                                    error!("[{}] Error assigning DeviceID: {}", service_id, e.to_string());
                                    Some(DeviceIdProviderResponseProtocol::NotFound)
                                }
                            };
                            if let Some(msg) = msg {
                                if let Err(e) = q_tx.send(msg).await {
                                    // TODO: Probably we want to unregister it at this point or retry
                                    error!("[{}] Error assigning DeviceID: {}", service_id, e.to_string());
                                }
                            }
                        },
                        Some(_) => {},
                        None => {}
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
