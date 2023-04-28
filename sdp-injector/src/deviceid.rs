use std::collections::{HashMap, HashSet};
use std::pin::Pin;

use futures::Future;
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

#[derive(Debug)]
pub struct RegisteredDeviceId {
    assigned: HashSet<Uuid>,
    avaiable: Vec<Uuid>,
}

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
        let fut = async move {
            let mut errors = Vec::new();
            let mut uuids: Vec<Uuid> = vec![];
            for deviceid in &service.spec.device_ids {
                match Uuid::try_parse(&deviceid) {
                    Ok(uuid) => {
                        uuids.push(uuid);
                    }
                    Err(_) => {
                        errors.push(deviceid.clone());
                    }
                }
            }
            if !errors.is_empty() {
                Err(SDPServiceError::from_string(format!(
                    "Unable to parse uuids: {}",
                    errors.join(",")
                )))
            } else {
                self.device_ids.insert(
                    service.service_id(),
                    RegisteredDeviceId {
                        assigned: HashSet::from_iter(uuids.clone()),
                        avaiable: uuids,
                    },
                );
                Ok(self
                    .identities
                    .insert(service.service_id(), service.clone())
                    .or_else(|| Some(service)))
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

    fn pop_device_id<'a>(
        &'a mut self,
        service_id: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<(ServiceIdentity, Uuid), SDPServiceError>> + Send + '_>>
    {
        let fut = async move {
            let mut sid = &mut self.identities.get_mut(&service_id.to_string());
            let mut ds: &mut Option<&mut RegisteredDeviceId> =
                &mut self.device_ids.get_mut(&service_id.to_string());
            match (&mut sid, &mut ds) {
                (Some(sid), Some(registered_device_ids)) => {
                    let uuid = if registered_device_ids.avaiable.len() == 0 {
                        format!(
                            "Requested device id for {} but no device ids are available, creating dynamically a new one!",
                            service_id
                        );
                        let uuid = Uuid::new_v4();
                        sid.spec.device_ids.push(uuid.to_string());
                        uuid
                    } else {
                        // Get first uuid
                        let uuid = registered_device_ids.avaiable.remove(0);
                        uuid
                    };
                    info!(
                        "[{}] DeviceID {} assigned to service {}",
                        service_id, uuid, service_id
                    );
                    info!(
                        "[{}] Service {} has {} DeviceIDs available {:?}",
                        service_id,
                        service_id,
                        registered_device_ids.avaiable.len(),
                        &registered_device_ids.avaiable
                    );
                    // Update current data
                    registered_device_ids.assigned.insert(uuid);
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
            // TODO: We need to call the release API here
            match (
                self.identities.get(&service_id),
                &mut self.device_ids.get_mut(&service_id),
            ) {
                (Some(_), Some(registered_device_id))
                    if registered_device_id.assigned.contains(&uuid) =>
                {
                    if !&registered_device_id.avaiable.contains(&uuid) {
                        info!("[{}] Pushing DeviceID {} to the list of available DeviceIDs for service {}", service_id, uuid, service_id);
                        registered_device_id.avaiable.push(uuid.clone());
                        Ok(Some(uuid.clone()))
                    } else {
                        warn!("[{}] DeviceID {} is already in the list of available device-ids for service {}", service_id, uuid, service_id);
                        Ok(None)
                    }
                }
                (Some(_), Some(_)) => Err(SDPServiceError::from_string(format!(
                    "Device id {} is not registered for service {}",
                    &uuid, service_id
                ))),
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
    use std::collections::HashMap;

    use sdp_common::crd::ServiceIdentitySpec;
    use sdp_common::errors::SDPServiceError;
    use sdp_common::service::{ServiceIdentity, ServiceUser};
    use sdp_macros::{service_identity, service_user};
    use uuid::Uuid;

    use super::{IdentityStore, InMemoryIdentityStore};

    #[tokio::test]
    async fn test_in_memory_identity_store_0() {
        let mut store = InMemoryIdentityStore::default();

        // Try to get a device_id. Fails because we don't have any service identity registered
        let maybe_device_id = store.pop_device_id("service1").await;
        assert!(maybe_device_id.is_err());
        assert_eq!(
            maybe_device_id.err(),
            Some(SDPServiceError::from_string(format!(
                "ServiceIdentity and/or DeviceId is missing for service service1"
            )))
        );

        // Register a new service identity
        let maybe_service_identity = store.register_service(service_identity!(1)).await;
        assert!(maybe_service_identity.is_ok());
        if let Ok(Some(service_identity)) = store.register_service(service_identity!(1)).await {
            assert_eq!(service_identity.spec, service_identity!(1).spec)
        } else {
            assert!(false, "Error registering service identity");
        }

        let mut used_uuids = vec![];
        // Try to get three, we should get a new one
        for i in 0..3 {
            let maybe_device_id = store.pop_device_id("ns1_srv1").await;
            if let Ok((s, uuid)) = maybe_device_id {
                used_uuids.push(uuid);
                let mut expected_s = service_identity!(1);
                expected_s.spec.device_ids = used_uuids
                    .iter()
                    .map(Uuid::to_string)
                    .collect::<Vec<String>>()
                    .clone();
                assert_eq!(s.spec, expected_s.spec);
                if i == 0 {
                    // These should be reused
                    assert_eq!(
                        uuid.to_string(),
                        format!("00000000-0000-0000-0000-000000000001")
                    );
                } else {
                    // This should be a new one
                    assert_ne!(
                        uuid.to_string(),
                        format!("00000000-0000-0000-0000-000000000001")
                    );
                }
            } else {
                println!("Error getting devide id: {:?}", maybe_device_id);
                assert!(false);
            }
        }

        // Release the device ids
        for i in 0..3 {
            let result = store.push_device_id("ns1_srv1", used_uuids[i]).await;
            if let Ok(Some(uuid)) = result {
                assert_eq!(uuid.to_string(), used_uuids[i].to_string());
            } else {
                println!("Error pushing devide id: {:?}", result);
                assert!(false);
            }
        }
        // Now we get them again and they should be reused
        let mut expected_s = service_identity!(1);
        expected_s.spec.device_ids = used_uuids
            .iter()
            .map(Uuid::to_string)
            .collect::<Vec<String>>()
            .clone();
        for i in 0..4 {
            let maybe_device_id = store.pop_device_id("ns1_srv1").await;
            if let Ok((s, uuid)) = maybe_device_id {
                if i < 3 {
                    assert_eq!(uuid.to_string(), used_uuids[i].to_string());
                } else {
                    // This should be a new device id
                    assert!(!used_uuids
                        .iter()
                        .map(Uuid::to_string)
                        .collect::<Vec<String>>()
                        .contains(&uuid.to_string()));
                    expected_s.spec.device_ids.push(uuid.to_string());
                }
                assert_eq!(s.spec, expected_s.spec);
            } else {
                println!("Error getting devide id: {:?}", maybe_device_id);
                assert!(false);
            }
        }
    }

    #[tokio::test]
    async fn test_in_memory_identity_store_1() {
        let mut store = InMemoryIdentityStore::default();

        let result = store.push_device_id("ns1_srv1", Uuid::new_v4()).await;
        if let Err(err) = result {
            assert_eq!(
                err,
                SDPServiceError::from_string(format!("Service id ns1_srv1 is not registered"))
            );
        } else {
            assert!(false, "We should not be able to push device ids when there is no service identity registered");
        }

        // Register a new service identity
        let result = store.register_service(service_identity!(1)).await;
        if let Ok(Some(service_identity)) = result {
            assert_eq!(service_identity.spec, service_identity!(1).spec)
        } else {
            println!("Error registering service identity: {:?}", result);
            assert!(false, "Error registering service identity");
        }

        // Check that we can push a new device even if this was not part of the ones in service identity
        let test_device_id = "d587f3e1-ee5b-4ea7-8023-32c88d5d0afb";
        if let Err(err) = store
            .push_device_id("ns1_srv1", Uuid::parse_str(test_device_id).unwrap())
            .await
        {
            assert_eq!(
                err.error,
                format!(
                    "Device id {} is not registered for service ns1_srv1",
                    test_device_id
                )
            );
        } else {
            assert!(
                false,
                "We should not be able to push a device id that was not regitered before"
            )
        }

        // Get the original device we had
        let (s, uuid) = store
            .pop_device_id("ns1_srv1")
            .await
            .expect("We should be able to get a device id");
        assert!(!s.spec.device_ids.contains(&test_device_id.to_string()));
        assert_eq!(
            uuid.to_string(),
            format!("00000000-0000-0000-0000-000000000001")
        );

        let (s, uuid) = store
            .pop_device_id("ns1_srv1")
            .await
            .expect("We should be able to get a device id");
        assert!(!s.spec.device_ids.contains(&test_device_id.to_string()));
        assert!(s.spec.device_ids.contains(&uuid.to_string()));
    }
}
