use std::collections::{HashMap, VecDeque};

use async_trait::async_trait;
use sdp_common::errors::SDPServiceError;
use sdp_common::service::ServiceIdentity;
use sdp_common::traits::{HasCredentials, Service};
use sdp_macros::{logger, sdp_error, sdp_info, sdp_log, sdp_warn, with_dollar_sign};
use tokio::sync::mpsc::{Receiver, Sender};
use uuid::Uuid;

logger!("DeviceIDProvider");

#[derive(Debug, Clone)]
pub enum DeviceIdProviderRequestProtocol<A: Service + HasCredentials> {
    FoundServiceIdentity(A),
    DeletedServiceIdentity(A),
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

#[derive(Clone, Debug, PartialEq, Default)]
//pub struct RegisteredDeviceId(usize, HashSet<Uuid>, usize, Vec<Uuid>);
pub struct RegisteredDeviceId(VecDeque<Uuid>);

impl RegisteredDeviceId {
    fn push(&mut self, uuid: Uuid) -> () {
        info!("[{}] Releasing device id", uuid);
        if !self.0.contains(&uuid) {
            self.0.push_front(uuid)
        } else {
            warn!("[{}] Device id is already released", uuid);
        }
    }

    fn pop(&mut self) -> ReleasedDeviceId {
        if let Some(uuid) = self.0.pop_front() {
            ReleasedDeviceId::FromPool(uuid.clone())
        } else {
            ReleasedDeviceId::Fresh(Uuid::new_v4())
        }
    }
}

/*
 Trait used to request a new device id
*/
#[async_trait]
pub trait DeviceIdRequester {
    async fn request(&self, service_id: &str) -> Result<(ServiceIdentity, Uuid), SDPServiceError>;
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

    async fn unregister_service<'a>(
        &mut self,
        service_id: &'a str,
    ) -> Result<Option<(A, RegisteredDeviceId)>, SDPServiceError>;
}

pub struct DeviceIdProvider<A: Service + HasCredentials> {
    store: Box<dyn IdentityStore<A> + Send>,
}

#[derive(Default)]
pub struct InMemoryIdentityStore {
    identities: HashMap<String, ServiceIdentity>,
    registered_device_ids: HashMap<String, RegisteredDeviceId>,
}

impl<'a> InMemoryIdentityStore {
    pub fn new() -> Self {
        InMemoryIdentityStore {
            identities: HashMap::new(),
            registered_device_ids: HashMap::new(),
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
        self.registered_device_ids
            .insert(service.service_id(), RegisteredDeviceId::default());
        Ok(updated_service.unwrap_or(service))
    }

    async fn unregister_service<'b>(
        &mut self,
        service_id: &'b str,
    ) -> Result<Option<(ServiceIdentity, RegisteredDeviceId)>, SDPServiceError> {
        let device_id = self.registered_device_ids.remove(service_id);
        let service_id = self.identities.remove(service_id);
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
        let ds = self.registered_device_ids.get_mut(&service_id.to_string());
        match (sid, ds) {
            (Some(sid), Some(registered_device_id)) => match registered_device_id.pop() {
                ReleasedDeviceId::Fresh(uuid) => {
                    info!(
                        "[{}] Got device id {} as a fresh device id",
                        service_id, uuid
                    );
                    println!("FRESH!");
                    Ok((sid.clone(), uuid))
                }
                ReleasedDeviceId::FromPool(uuid) => {
                    info!(
                        "[{}] Got device id {} from the service identity pool of device ids",
                        service_id, uuid
                    );
                    println!("CACHED!");
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
                    "ServiceIdentity is missing for service {}",
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
            self.registered_device_ids.get_mut(&service_id),
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
                    "ServiceIdentity is missing for service {}",
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
    use sdp_macros::{service_identity, service_user};
    use uuid::Uuid;

    use super::{IdentityStore, InMemoryIdentityStore};
    use sdp_common::crd::ServiceIdentitySpec;
    use sdp_common::service::{ServiceIdentity, ServiceUser};

    #[tokio::test]
    // Pop device id when no ServiceIdentity registered
    async fn test_in_memory_identity_store_0() {
        let mut m: InMemoryIdentityStore = InMemoryIdentityStore::new();
        let s: ServiceIdentity = service_identity!(0);
        assert_eq!(
            m.pop_device_id(&s.service_id()).await.unwrap_err(),
            SDPServiceError::from_string(format!(
                "ServiceIdentity is missing for service ns0_srv0"
            ))
        );
    }

    #[tokio::test]
    async fn test_in_memory_identity_store_1() {
        let mut m: InMemoryIdentityStore = InMemoryIdentityStore::new();
        let s: ServiceIdentity = service_identity!(0);
        let service_id = s.service_id();
        assert_eq!(
            m.pop_device_id(&service_id).await.unwrap_err(),
            SDPServiceError::from_string(format!(
                "ServiceIdentity is missing for service ns0_srv0"
            ))
        );
    }

    #[tokio::test]
    async fn test_in_memory_identity_store_2() {
        let mut m = InMemoryIdentityStore::new();
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
    async fn test_in_memory_identity_store_pop_device_id() {
        let mut m = InMemoryIdentityStore::new();
        let uuids: Vec<Uuid> = (0..3).map(|_i| Uuid::new_v4()).collect();
        let s: ServiceIdentity = service_identity!(0);
        let service_id = &s.service_id();
        m.register_service(s.clone()).await.unwrap();
        m.register_service(service_identity!(0)).await.unwrap();
        for uuid in &uuids {
            let _ = m.push_device_id(service_id, uuid.clone()).await;
        }
        let (_, got_uuid) = m.pop_device_id(&service_id).await.unwrap();
        assert_eq!(got_uuid, uuids[2]);
        let (_, got_uuid) = m.pop_device_id(&service_id).await.unwrap();
        assert_eq!(got_uuid, uuids[1]);
        let (_, got_uuid) = m.pop_device_id(&service_id).await.unwrap();
        assert_eq!(got_uuid, uuids[0]);
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
        assert_eq!(got_uuid, ReleasedDeviceId::FromPool(extra_uuid));
        let got_uuid = registered_device_id.pop();
        assert_eq!(got_uuid, ReleasedDeviceId::FromPool(uuids[0]));
        let got_uuid = registered_device_id.pop();
        assert_eq!(got_uuid, ReleasedDeviceId::FromPool(fresh_uuid.unwrap()));
    }
}
