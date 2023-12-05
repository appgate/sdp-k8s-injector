use std::collections::{HashMap, VecDeque};

use async_trait::async_trait;
use sdp_common::crd::DeviceId;
use sdp_common::errors::SDPServiceError;
use sdp_common::service::ServiceIdentity;
use sdp_common::traits::{HasCredentials, Service};
use sdp_macros::{logger, sdp_error, sdp_info, sdp_log, with_dollar_sign};
use tokio::sync::mpsc::{Receiver, Sender};
use uuid::Uuid;

logger!("DeviceIDProvider");

#[derive(Debug, Clone)]
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

#[derive(Debug, PartialEq)]
pub enum ReleasedDeviceId {
    FromPool(Uuid),
    Fresh(Uuid),
}

#[derive(Clone, Debug, PartialEq)]
//pub struct RegisteredDeviceId(usize, HashSet<Uuid>, usize, Vec<Uuid>);
pub struct RegisteredDeviceId(VecDeque<Uuid>);

impl RegisteredDeviceId {
    fn push(&mut self, uuid: Uuid) -> () {
        self.0.push_back(uuid);
    }

    fn pop(&mut self) -> ReleasedDeviceId {
        if let Some(uuid) = self.0.pop_front() {
            ReleasedDeviceId::FromPool(uuid.clone())
        } else {
            ReleasedDeviceId::Fresh(Uuid::new_v4())
        }
    }
}

#[async_trait]
pub trait IdentityStore<A: Service + HasCredentials>: Send + Sync {
    async fn pop_device_id<'a>(
        &mut self,
        service_id: &'a str,
    ) -> Result<(ServiceIdentity, Uuid), SDPServiceError>;

    async fn push_device_id<'a>(
        &mut self,
        service_id: &'a str,
        uuid: Uuid,
    ) -> Result<Option<Uuid>, SDPServiceError>;

    async fn register_service(&mut self, service: A) -> Result<A, SDPServiceError>;

    async fn register_device_ids(
        &mut self,
        device_id: DeviceId,
    ) -> Result<RegisteredDeviceId, SDPServiceError>;

    async fn unregister_service<'a>(
        &mut self,
        service_id: &'a str,
    ) -> Result<Option<(A, RegisteredDeviceId)>, SDPServiceError>;

    async fn unregister_device_ids<'a>(
        &mut self,
        device_id: &'a str,
    ) -> Result<Option<(A, RegisteredDeviceId)>, SDPServiceError>;
}

pub struct DeviceIdProvider<A: Service + HasCredentials> {
    store: Box<dyn IdentityStore<A> + Send>,
}

#[derive(Default)]
pub struct InMemoryIdentityStore {
    identities: HashMap<String, ServiceIdentity>,
    device_ids: HashMap<String, RegisteredDeviceId>,
}

impl<'a> InMemoryIdentityStore {
    pub fn new() -> Self {
        InMemoryIdentityStore {
            identities: HashMap::new(),
            device_ids: HashMap::new(),
        }
    }
}

#[async_trait]
impl<'a> IdentityStore<ServiceIdentity> for InMemoryIdentityStore {
    async fn register_service(
        &mut self,
        service: ServiceIdentity,
    ) -> Result<ServiceIdentity, SDPServiceError> {
        let updated_service = self
            .identities
            .insert(service.service_id(), service.clone());
        Ok(updated_service.unwrap_or(service))
    }

    async fn register_device_ids(
        &mut self,
        device_id: DeviceId,
    ) -> Result<RegisteredDeviceId, SDPServiceError> {
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
            let registered_device_id =
                RegisteredDeviceId(VecDeque::from_iter(uuids.iter().map(Clone::clone)));
            let updated_device_ids = self
                .device_ids
                .insert(device_id.service_id(), registered_device_id.clone());
            Ok(updated_device_ids.unwrap_or(registered_device_id))
        }
    }

    async fn unregister_service<'b>(
        &mut self,
        service_id: &'b str,
    ) -> Result<Option<(ServiceIdentity, RegisteredDeviceId)>, SDPServiceError> {
        let device_id = self.device_ids.remove(service_id);
        let service_id = self.identities.remove(service_id);
        match (service_id, device_id) {
            (Some(sid), Some(did)) => Ok(Some((sid, did))),
            _ => Ok(None),
        }
    }

    async fn unregister_device_ids<'b>(
        &mut self,
        device_id: &'b str,
    ) -> Result<Option<(ServiceIdentity, RegisteredDeviceId)>, SDPServiceError> {
        let service_id = self.identities.remove(device_id);
        let device_id = self.device_ids.remove(device_id);
        match (service_id, device_id) {
            (Some(sid), Some(did)) => Ok(Some((sid, did))),
            _ => Ok(None),
        }
    }

    async fn pop_device_id<'b>(
        &mut self,
        service_id: &'b str,
    ) -> Result<(ServiceIdentity, Uuid), SDPServiceError> {
        let sid = self.identities.get(&service_id.to_string());
        let ds = self.device_ids.get_mut(&service_id.to_string());
        match (sid, ds) {
            (Some(sid), Some(registered_device_id)) => match registered_device_id.pop() {
                ReleasedDeviceId::Fresh(uuid) => {
                    info!(
                        "[{}] Got device id {} as a fresh device id",
                        service_id, uuid
                    );
                    Ok((sid.clone(), uuid))
                }
                ReleasedDeviceId::FromPool(uuid) => {
                    info!(
                        "[{}] Got device id {} from the service identity pool of device ids",
                        service_id, uuid
                    );
                    Ok((sid.clone(), uuid))
                }
            },
            (sid, ds) => {
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
    }

    async fn push_device_id<'b>(
        &mut self,
        service_id: &'b str,
        uuid: Uuid,
    ) -> Result<Option<Uuid>, SDPServiceError> {
        let service_id = service_id.to_string();
        match (
            self.identities.get(&service_id),
            self.device_ids.get_mut(&service_id),
        ) {
            (Some(_), Some(registered_device_id)) => {
                registered_device_id.push(uuid);
                Ok(Some(uuid.clone()))
            }
            (sid, ds) => {
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

#[cfg(test)]
mod tests {
    use std::collections::{HashMap, VecDeque};

    use crate::deviceid::{RegisteredDeviceId, ReleasedDeviceId};
    use sdp_common::errors::SDPServiceError;
    use sdp_common::traits::{Named, Namespaced, Service};
    use sdp_macros::{device_id, service_identity, service_user};
    use uuid::Uuid;

    use super::{IdentityStore, InMemoryIdentityStore};
    use sdp_common::crd::{DeviceId, DeviceIdSpec, ServiceIdentitySpec};
    use sdp_common::service::{ServiceIdentity, ServiceUser};

    #[tokio::test]
    // Pop device id when no ServiceIdentity or DevideId are registered
    async fn test_in_memory_identity_store_0() {
        let mut m: InMemoryIdentityStore = InMemoryIdentityStore::new();
        let s: ServiceIdentity = service_identity!(0);
        assert_eq!(
            m.pop_device_id(&s.service_id()).await.unwrap_err(),
            SDPServiceError::from_string(format!(
                "ServiceIdentity and/or DeviceId is missing for service ns0_srv0"
            ))
        );
    }

    #[tokio::test]
    // Pop device id when  DevideId is registered
    async fn test_in_memory_identity_store_1() {
        let mut m: InMemoryIdentityStore = InMemoryIdentityStore::new();
        let s: ServiceIdentity = service_identity!(0);
        let service_id = s.service_id();
        m.register_service(s).await.unwrap();
        assert_eq!(
            m.pop_device_id(&service_id).await.unwrap_err(),
            SDPServiceError::from_string(format!(
                "ServiceIdentity and/or DeviceId is missing for service ns0_srv0"
            ))
        );
    }

    #[tokio::test]
    // Pop device id when ServiceIdentity is registered
    async fn test_in_memory_identity_store_2() {
        let mut m: InMemoryIdentityStore = InMemoryIdentityStore::new();
        let s: ServiceIdentity = service_identity!(0);
        let service_id = s.service_id();
        m.register_device_ids(device_id!(0)).await.unwrap();
        assert_eq!(
            m.pop_device_id(&service_id).await.unwrap_err(),
            SDPServiceError::from_string(format!(
                "ServiceIdentity and/or DeviceId is missing for service ns0_srv0"
            ))
        );
    }

    #[tokio::test]
    // Pop device id when ServiceIdentity and DeviceId are registered
    async fn test_in_memory_identity_store_3() {
        let mut m = InMemoryIdentityStore::new();
        m.register_device_ids(device_id!(0)).await.unwrap();
        let s: ServiceIdentity = service_identity!(0);
        let service_id = &s.service_id();
        m.register_service(s.clone()).await.unwrap();
        m.register_service(service_identity!(0)).await.unwrap();
        let (got_s, got_uuid) = m.pop_device_id(&service_id).await.unwrap();
        assert!(Uuid::try_parse(&got_uuid.to_string()).is_ok());
        assert_eq!(got_s.spec, s.spec);
        assert_eq!(got_s.service_name(), s.service_name());
        assert_eq!(got_s.service_id(), s.service_id());
        assert_eq!(got_s.name(), s.name());
        assert_eq!(got_s.namespace(), s.namespace());
    }

    #[tokio::test]
    // Pop device id when ServiceIdentity and DeviceId are registered
    async fn test_in_memory_identity_store_register_device_id() {
        let mut m = InMemoryIdentityStore::new();
        let uuids: Vec<Uuid> = (0..3).map(|_i| Uuid::new_v4()).collect();
        let device_id = device_id!(
            0,
            uuids
                .clone()
                .into_iter()
                .map(|uuid| uuid.to_string())
                .collect()
        );
        let registered_device_id = m.register_device_ids(device_id).await.unwrap();
        assert_eq!(
            registered_device_id,
            RegisteredDeviceId(VecDeque::from_iter(uuids.iter().map(Clone::clone)))
        );
    }

    #[tokio::test]
    // Pop device id when ServiceIdentity and DeviceId are registered
    async fn test_in_memory_identity_store_pop_device_id() {
        let mut m = InMemoryIdentityStore::new();
        let uuids: Vec<String> = (0..3).map(|_i| Uuid::new_v4().to_string()).collect();
        let device_id = device_id!(0, uuids.clone());
        m.register_device_ids(device_id).await.unwrap();
        let s: ServiceIdentity = service_identity!(0);
        let service_id = &s.service_id();
        m.register_service(s.clone()).await.unwrap();
        m.register_service(service_identity!(0)).await.unwrap();
        let (_, got_uuid) = m.pop_device_id(&service_id).await.unwrap();
        assert_eq!(got_uuid.to_string(), uuids[0]);
        let (_, got_uuid) = m.pop_device_id(&service_id).await.unwrap();
        assert_eq!(got_uuid.to_string(), uuids[1]);
        let (_, got_uuid) = m.pop_device_id(&service_id).await.unwrap();
        assert_eq!(got_uuid.to_string(), uuids[2]);
    }

    #[test]
    fn test_regitered_device_id_get_device_id_pop() {
        let uuids: Vec<Uuid> = (0..3).map(|_i| Uuid::new_v4()).collect();
        let mut registered_device_id =
            RegisteredDeviceId(VecDeque::from_iter(uuids.iter().map(Clone::clone)));
        assert_eq!(registered_device_id.0.len(), 3);
        let got_uuid = registered_device_id.pop();
        assert_eq!(got_uuid, ReleasedDeviceId::FromPool(uuids[0]));
        let got_uuid = registered_device_id.pop();
        assert_eq!(got_uuid, ReleasedDeviceId::FromPool(uuids[1]));
        let got_uuid = registered_device_id.pop();
        assert_eq!(got_uuid, ReleasedDeviceId::FromPool(uuids[2]));
        if let ReleasedDeviceId::FromPool(_uuid) = registered_device_id.pop() {
            assert!(false, "Expected a fresh uuid since the pool was empty");
        }
    }

    #[test]
    fn test_regitered_device_id_get_device_id_push() {
        let uuids: Vec<Uuid> = (0..1).map(|_i| Uuid::new_v4()).collect();
        let mut registered_device_id =
            RegisteredDeviceId(VecDeque::from_iter(uuids.iter().map(Clone::clone)));
        assert_eq!(registered_device_id.0.len(), 1);
        // pop 1 device id - we can get it from the pool
        let got_uuid = registered_device_id.pop();
        assert_eq!(got_uuid, ReleasedDeviceId::FromPool(uuids[0]));

        // release the device id
        registered_device_id.push(uuids[0].clone());

        // Now we get it again since it's in the pool
        let got_uuid = registered_device_id.pop();
        assert_eq!(got_uuid, ReleasedDeviceId::FromPool(uuids[0]));

        // pop another device id, the pool is empty so we get a fresh new one
        let mut fresh_uuid = None;
        if let ReleasedDeviceId::Fresh(uuid) = registered_device_id.pop() {
            // Release it
            fresh_uuid = Some(uuid.clone());
            registered_device_id.push(uuid)
        } else {
            assert!(false, "Expected a fresh uuid since the pool was empty");
        }
        // Release the first device id we got
        registered_device_id.push(uuids[0].clone());

        // Release a completely new device id
        let extra_uuid = Uuid::new_v4();
        registered_device_id.push(extra_uuid.clone());

        // Now we have:
        // fresh_uuid
        // extra_uuid
        // uuids[0]
        let got_uuid = registered_device_id.pop();
        assert_eq!(got_uuid, ReleasedDeviceId::FromPool(fresh_uuid.unwrap()));
        let got_uuid = registered_device_id.pop();
        assert_eq!(got_uuid, ReleasedDeviceId::FromPool(uuids[0]));
        let got_uuid = registered_device_id.pop();
        assert_eq!(got_uuid, ReleasedDeviceId::FromPool(extra_uuid));
    }
}
