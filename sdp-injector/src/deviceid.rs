use std::collections::{HashMap, HashSet};
use std::pin::Pin;

use futures::Future;
use kube::api::DeleteParams;
use kube::Api;
use sdp_common::crd::AssignedDeviceId;
use sdp_common::errors::SDPServiceError;
use sdp_common::service::ServiceIdentity;
use sdp_common::traits::{HasCredentials, Service, WithDeviceIds};
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
    available: Vec<Uuid>,
}

pub trait IdentityStore<A: Service + HasCredentials>: Send + Sync {
    fn get_service<'a>(
        &'a mut self,
        service_id: &'a str,
    ) -> Pin<
        Box<
            dyn Future<Output = Result<Option<&'a mut ServiceIdentity>, SDPServiceError>>
                + Send
                + '_,
        >,
    >;
    fn get_device_ids<'a>(
        &'a mut self,
        service_id: &'a str,
    ) -> Pin<
        Box<
            dyn Future<Output = Result<Option<&'a mut RegisteredDeviceId>, SDPServiceError>>
                + Send
                + '_,
        >,
    >;
    fn pop_device_id<'a>(
        &'a mut self,
        service_id: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<(ServiceIdentity, Uuid), SDPServiceError>> + Send + '_>>;

    fn push_device_id<'a>(
        &'a mut self,
        service_id: &'a str,
        uuid: Uuid,
        force: bool,
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
    fn get_service<'a>(
        &'a mut self,
        service_id: &'a str,
    ) -> Pin<
        Box<
            dyn Future<Output = Result<Option<&'a mut ServiceIdentity>, SDPServiceError>>
                + Send
                + '_,
        >,
    > {
        let fut = async move { Ok(self.identities.get_mut(&service_id.to_string())) };
        Box::pin(fut)
    }

    fn get_device_ids<'a>(
        &'a mut self,
        service_id: &'a str,
    ) -> Pin<
        Box<
            dyn Future<Output = Result<Option<&'a mut RegisteredDeviceId>, SDPServiceError>>
                + Send
                + '_,
        >,
    > {
        let fut = async move { Ok(self.device_ids.get_mut(&service_id.to_string())) };
        Box::pin(fut)
    }

    fn register_service(
        &mut self,
        mut service: ServiceIdentity,
    ) -> Pin<Box<dyn Future<Output = Result<Option<ServiceIdentity>, SDPServiceError>> + Send + '_>>
    {
        let fut = async move {
            let mut errors = Vec::new();
            let mut available_device_ids: Vec<Uuid> = vec![];
            let mut assigned_device_ids: Vec<Uuid> = vec![];

            // Get all the available device ids (if any is present) into the list of available device ids in store
            for device_id in service.available_device_ids() {
                match Uuid::try_parse(&device_id) {
                    Ok(uuid) => {
                        available_device_ids.push(uuid);
                    }
                    Err(_) => {
                        errors.push(device_id.clone());
                    }
                }
            }

            // Get all the assigned device ids (if any is present) into the list of available device ids in store
            for device_id in service.assigned_device_ids() {
                match Uuid::try_parse(&device_id) {
                    Ok(uuid) => {
                        assigned_device_ids.push(uuid);
                    }
                    Err(_) => {
                        errors.push(device_id.clone());
                    }
                }
            }

            // Register them
            if !errors.is_empty() {
                Err(SDPServiceError::from_string(format!(
                    "Unable to parse uuids: {}",
                    errors.join(",")
                )))
            } else {
                self.device_ids.insert(
                    service.service_id(),
                    RegisteredDeviceId {
                        assigned: HashSet::from_iter(assigned_device_ids),
                        available: available_device_ids,
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
                    let uuid = registered_device_ids.available.pop().unwrap_or_else(|| {
                        // We dont have more available device ids, so create a new one
                        format!(
                            "Requested device id for {} but no device ids are available, creating dynamically a new one!",
                            service_id
                        );
                        Uuid::new_v4()
                    });
                    info!(
                        "[{}] DeviceID {} assigned to service {}",
                        service_id, uuid, service_id
                    );
                    info!(
                        "[{}] Service {} has {} DeviceIDs available {:?}",
                        service_id,
                        service_id,
                        registered_device_ids.available.len(),
                        &registered_device_ids.available
                    );

                    // Add new device id into the assigned device ids in the service identity
                    sid.add_assigned_device_id(uuid.to_string());
                    sid.remove_available_device_id(uuid.to_string());

                    // Add new device id into the assigned device ids in the store
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
        force: bool,
    ) -> Pin<Box<dyn Future<Output = Result<Option<Uuid>, SDPServiceError>> + Send + '_>> {
        let fut = async move {
            let service_id = service_id.to_string();
            // TODO: We need to call the release API here
            match (
                &mut self.identities.get_mut(&service_id),
                &mut self.device_ids.get_mut(&service_id),
            ) {
                (Some(s), Some(registered_device_id))
                    if force || registered_device_id.assigned.contains(&uuid) =>
                {
                    if !registered_device_id.available.contains(&uuid) {
                        info!("[{}] Pushing DeviceID {} to the list of available DeviceIDs for service {}", service_id, uuid, service_id);

                        // Upate store
                        registered_device_id.assigned.remove(&uuid);
                        registered_device_id.available.push(uuid);

                        // Update ServiceIdentity
                        s.remove_assigned_device_id(uuid.to_string());
                        s.add_available_device_id(uuid.to_string());
                        Ok(Some(uuid.clone()))
                    } else {
                        warn!("[{}] DeviceID {} is already in the list of available device-ids for service {}", service_id, uuid, service_id);
                        Ok(None)
                    }
                }
                (Some(_), Some(_)) => Err(SDPServiceError::from_string(format!(
                    "Device id {} is not assigned for service {}",
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
        assigned_device_id_api: Api<AssignedDeviceId>,
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
                            if let Err(err) = self.store.push_device_id(&service_id, uuid, false).await {
                                error!("[{}] Unable to release DeviceID {} for service {}: {}", service_id, uuid, service_id, err);
                            };
                            if let Err(err) = assigned_device_id_api.delete(&uuid.to_string(), &DeleteParams::default()).await {
                                error!("[{}] Unable to delete AssignedDEviceId {} for service {}: {}", service_id, uuid, service_id, err);
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
    use std::collections::{HashMap, HashSet};

    use sdp_common::crd::ServiceIdentitySpec;
    use sdp_common::errors::SDPServiceError;
    use sdp_common::service::{ServiceIdentity, ServiceUser};
    use sdp_common::traits::WithDeviceIds;
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
        store
            .register_service(service_identity!(1))
            .await
            .expect("We should be able to register services");
        let ref mut service_identity = store
            .get_service("ns1_srv1")
            .await
            .expect("We should have service ns1_srv1 registered")
            .expect("We should have service ns1_srv1 registered");
        assert_eq!(
            service_identity.assigned_device_ids().clone(),
            HashSet::new()
        );
        assert_eq!(
            service_identity.available_device_ids().clone(),
            HashSet::from_iter(vec![format!("00000000-0000-0000-0000-000000000001")]),
        );

        let registered_device_ids = store
            .get_device_ids("ns1_srv1")
            .await
            .expect("We should have service ns1_srv1 registered")
            .expect("We should have service ns1_srv1 registered");
        assert_eq!(registered_device_ids.assigned, HashSet::new());
        assert_eq!(
            registered_device_ids.available,
            vec![Uuid::parse_str(&format!("00000000-0000-0000-0000-000000000001")).unwrap()]
        );

        let uuids = &mut vec![];
        // Try to get three, we should get new ones
        for _ in 0..4 {
            let (_, device_id) = store
                .pop_device_id("ns1_srv1")
                .await
                .expect("We should be able to pop device ids");
            uuids.push(device_id);
        }

        // Check device ids and services in storage
        let device_ids: &Vec<String> = &mut uuids.iter().map(Uuid::to_string).collect();
        let ref mut service_identity = store
            .get_service("ns1_srv1")
            .await
            .expect("We should have service ns1_srv1 registered")
            .expect("We should have service ns1_srv1 registered");
        // All three device ids are in assigned
        assert_eq!(
            service_identity.assigned_device_ids().clone(),
            HashSet::from_iter(device_ids.iter().map(Clone::clone))
        );
        // No device ids in available
        assert_eq!(
            service_identity.available_device_ids().clone(),
            HashSet::new()
        );

        let registered_device_ids = store
            .get_device_ids("ns1_srv1")
            .await
            .expect("We should have service ns1_srv1 registered")
            .expect("We should have service ns1_srv1 registered");
        assert_eq!(
            registered_device_ids.assigned,
            HashSet::from_iter(uuids.iter().map(Clone::clone))
        );
        assert_eq!(registered_device_ids.available, vec![]);

        // Add back one device
        store
            .push_device_id("ns1_srv1", uuids[0], false)
            .await
            .expect("We should be able to push device ids");

        // Check service identity in store for service
        let ref mut service_identity = store
            .get_service("ns1_srv1")
            .await
            .expect("We should have service ns1_srv1 registered")
            .expect("We should have service ns1_srv1 registered");
        // All except the one we pushed now are still assigned
        assert_eq!(
            service_identity.assigned_device_ids().clone(),
            HashSet::from_iter(
                device_ids[1..]
                    .iter()
                    .map(Clone::clone)
                    .collect::<Vec<String>>()
            )
        );
        // The one we pushed is available
        assert_eq!(
            service_identity.available_device_ids().clone(),
            HashSet::from_iter(
                device_ids[..1]
                    .iter()
                    .map(Clone::clone)
                    .collect::<Vec<String>>()
            )
        );

        // Check device ids in store for service
        let registered_device_ids = store
            .get_device_ids("ns1_srv1")
            .await
            .expect("We should have service ns1_srv1 registered")
            .expect("We should have service ns1_srv1 registered");
        assert_eq!(
            registered_device_ids.assigned,
            HashSet::from_iter(uuids[1..].iter().map(Clone::clone).collect::<Vec<Uuid>>())
        );
        assert_eq!(
            registered_device_ids.available,
            uuids[..1].iter().map(Clone::clone).collect::<Vec<Uuid>>()
        );
    }

    #[tokio::test]
    async fn test_in_memory_identity_store_1() {
        let mut store = InMemoryIdentityStore::default();

        let result = store
            .push_device_id("ns1_srv1", Uuid::new_v4(), false)
            .await;
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

        // Check that we can't push a new device even if this was not part of the ones in service identity
        let test_device_id = "d587f3e1-ee5b-4ea7-8023-32c88d5d0afb";
        if let Err(err) = store
            .push_device_id("ns1_srv1", Uuid::parse_str(test_device_id).unwrap(), false)
            .await
        {
            assert_eq!(
                err.error,
                format!(
                    "Device id {} is not assigned for service ns1_srv1",
                    test_device_id
                )
            );
        } else {
            assert!(
                false,
                "We should not be able to push a device id that was not regitered before"
            )
        }

        // We can always force to push the device id
        if let Ok(Some(uuid)) = store
            .push_device_id("ns1_srv1", Uuid::parse_str(test_device_id).unwrap(), true)
            .await
        {
            assert_eq!(uuid.to_string(), test_device_id);
        } else {
            assert!(
                false,
                "We should be able to push a device id even if it was not regitered before when force is true"
            )
        }

        // At this point we have 2 device ids as available

        // Get the device id we have pushed now
        let (mut s, uuid) = store
            .pop_device_id("ns1_srv1")
            .await
            .expect("We should be able to get a device id");
        assert!(!s
            .available_device_ids()
            .contains(&test_device_id.to_string()));
        assert_eq!(uuid.to_string(), test_device_id);

        // Get the original device we had (000000...1)
        let (mut s, uuid) = store
            .pop_device_id("ns1_srv1")
            .await
            .expect("We should be able to get a device id");
        assert_eq!(
            uuid.to_string(),
            format!("00000000-0000-0000-0000-000000000001")
        );
        assert!(!s
            .available_device_ids()
            .contains(&test_device_id.to_string()));
        assert!(s.available_device_ids().is_empty());
    }
}
