use std::collections::HashMap;
use std::pin::Pin;

use futures::Future;
use log::{error, info, warn};
use sdp_common::crd::DeviceId;
use sdp_common::service::{HasCredentials, ServiceCandidate, ServiceIdentity};
use tokio::sync::mpsc::{Receiver, Sender};
use uuid::Uuid;

#[derive(Debug)]
pub enum InjectorPoolProtocol<A: ServiceCandidate + HasCredentials> {
    FoundServiceIdentity(A),
    FoundDevideId(DeviceId),
    DeletedServiceIdentity(A),
    DeletedDevideId(DeviceId),
    RequestDeviceId(Sender<InjectorPoolProtocolResponse<A>>, String),
    AssignedDeviceId(Uuid),
    ReleasedDevideId(String, Uuid),
}

pub enum InjectorPoolProtocolResponse<A: ServiceCandidate + HasCredentials> {
    AssignedDeviceId(A, Uuid),
    NotFound,
}

pub trait IdentityStore<A: ServiceCandidate + HasCredentials>: Send + Sync {
    fn identity<'a>(
        &'a self,
        service_id: &'a str,
    ) -> Pin<Box<dyn Future<Output = Option<(A, Uuid)>> + Send + '_>>;

    fn register_service(&mut self, service: A) -> Pin<Box<dyn Future<Output = ()> + Send + '_>>;

    fn register_device_ids(
        &mut self,
        device_id: DeviceId,
    ) -> Pin<Box<dyn Future<Output = ()> + Send + '_>>;

    fn unregister_service<'a>(
        &'a mut self,
        service: &'a A,
    ) -> Pin<Box<dyn Future<Output = Option<(A, DeviceId)>> + Send + '_>>;

    fn unregister_device_ids<'a>(
        &'a mut self,
        device_id: &'a DeviceId,
    ) -> Pin<Box<dyn Future<Output = Option<(A, DeviceId)>> + Send + '_>>;
}

pub struct InjectorPool<A: ServiceCandidate + HasCredentials> {
    store: Box<dyn IdentityStore<A> + Send>,
}

#[derive(Default)]
pub struct InMemoryIdentityStore {
    identities: HashMap<String, ServiceIdentity>,
    device_ids: HashMap<String, DeviceId>,
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
            self.identities.insert(service.service_id_key(), service);
        };
        Box::pin(fut)
    }

    fn register_device_ids(
        &mut self,
        device_id: DeviceId,
    ) -> Pin<Box<dyn Future<Output = ()> + Send + '_>> {
        let fut = async move {
            self.device_ids
                .insert(device_id.service_id_key(), device_id);
        };
        Box::pin(fut)
    }

    fn unregister_service<'a>(
        &'a mut self,
        service: &'a ServiceIdentity,
    ) -> Pin<Box<dyn Future<Output = Option<(ServiceIdentity, DeviceId)>> + Send + '_>> {
        let fut = async move {
            let device_id = self.device_ids.remove(&service.service_id_key());
            let service_id = self.identities.remove(&service.service_id_key());
            match (service_id, device_id) {
                (Some(sid), Some(did)) => Some((sid, did)),
                _ => None,
            }
        };
        Box::pin(fut)
    }

    fn unregister_device_ids<'a>(
        &'a mut self,
        device_id: &'a DeviceId,
    ) -> Pin<Box<dyn Future<Output = Option<(ServiceIdentity, DeviceId)>> + Send + '_>> {
        let fut = async move {
            let service_id = self.identities.remove(&device_id.service_id_key());
            let device_id = self.device_ids.remove(&device_id.service_id_key());
            match (service_id, device_id) {
                (Some(sid), Some(did)) => Some((sid, did)),
                _ => None,
            }
        };
        Box::pin(fut)
    }

    fn identity<'a>(
        &'a self,
        service_id: &'a str,
    ) -> Pin<Box<dyn Future<Output = Option<(ServiceIdentity, Uuid)>> + Send + '_>> {
        let fut = async move {
            let sid = self.identities.get(&service_id.to_string());
            let ds = self.device_ids.get(&service_id.to_string());
            match (sid, ds) {
                (Some(sid), Some(ds)) => match Uuid::parse_str(&ds.spec.uuids[0]) {
                    Ok(uuid) => Some((sid.clone(), uuid)),
                    Err(e) => {
                        error!("Error parsing uuid: {}", e.to_string());
                        None
                    }
                },
                _ => None,
            }
        };
        Box::pin(fut)
    }
}

impl InjectorPool<ServiceIdentity> {
    pub async fn run(
        &mut self,
        mut pool_rx: Receiver<InjectorPoolProtocol<ServiceIdentity>>,
        mut watcher_rx: Receiver<InjectorPoolProtocol<ServiceIdentity>>,
    ) {
        tokio::select! {
            val = watcher_rx.recv() => {
                match val {
                    Some(InjectorPoolProtocol::FoundServiceIdentity(s)) => {
                        self.store.register_service(s);
                    },
                    Some(InjectorPoolProtocol::DeletedServiceIdentity(s)) => {
                        self.store.unregister_service(&s);
                    },
                    Some(InjectorPoolProtocol::FoundDevideId(id)) => {
                        self.store.register_device_ids(id);
                    },
                    Some(InjectorPoolProtocol::DeletedDevideId(id)) => {
                        self.store.unregister_device_ids(&id);
                    },
                    Some(ev) => {
                        warn!("Ignored event {:?}", ev);
                    }
                    None => {
                        warn!("Ignored None event");
                    },
                }
            },
            val = pool_rx.recv() => {
                match val {
                    Some(InjectorPoolProtocol::RequestDeviceId(q_tx, service_id)) => {
                        if let Some((a, b)) = self.store.identity(&service_id).await {
                            if let Err(e) = q_tx.send(InjectorPoolProtocolResponse::NotFound).await {
                                error!("Error assigning devide id: {}", e.to_string());
                            }
                        }
                    },
                    Some(InjectorPoolProtocol::ReleasedDevideId(_service_id, _device_id)) => {
                        // Remove device id from the list of reserved ids
                    },
                    Some(ev) => {
                        info!("Ignored event {:?}", ev);
                    },
                    None => todo!(),
                }
            }
        }
    }

    /*
    while let Ok(message) = pool_rx.recv().await {
        match message {
            InjectorProtocol::RequestDeviceId {
                message_id,
                service_id,
            } => {
                match self.device_id_api.get_opt(&service_id).await {
                    Ok(None) => {
                        info!(
                            "Device ID for {} is not available yet. Retrying.",
                            service_id
                        );
                    }
                    Ok(device_id) => {
                        let device_id = device_id.unwrap();

                        let mut available_uuids: Vec<String> = device_id.spec.uuids;

                        // Figure out which UUIDs are still available by iterating
                        // through the pods belonging to the replicaset/deployment
                        // and checking the environment variable CLIENT_DEVICE_ID.
                        // If the env is set, we remove the UUID from availability
                        let replicasets = self
                            .replicaset_api
                            .list(&ListParams::default())
                            .await
                            .expect("Unable to list replicaset");
                        for replicaset in replicasets {
                            if let Some(replicaset_owners) =
                                &replicaset.metadata.owner_references.as_ref()
                            {
                                let replicaset_owner = &replicaset_owners[0];
                                let replicaset_name = format!(
                                    "{}-{}",
                                    replicaset.metadata.namespace.unwrap(),
                                    replicaset_owner.name
                                );
                                let deployment_name =
                                    device_id.metadata.name.as_ref().unwrap().clone();

                                // Found a replicaset owned by the deployment
                                if replicaset_name == deployment_name {
                                    let pods = self
                                        .pod_api
                                        .list(&ListParams::default())
                                        .await
                                        .expect("Unable to list pods");
                                    for pod in pods {
                                        if let Some(pod_owners) =
                                            &pod.metadata.owner_references.as_ref()
                                        {
                                            let replicaset_name = replicaset
                                                .metadata
                                                .name
                                                .as_ref()
                                                .unwrap()
                                                .clone();
                                            let pod_owner = &pod_owners[0];

                                            // Found a pod owned by the replicaset
                                            if pod_owner.name == replicaset_name {
                                                // Iterate through environment variable, looking for CLIENT_DEVICE_ID
                                                for container in
                                                    &pod.spec.as_ref().unwrap().containers
                                                {
                                                    if container.name == "sdp-service" {
                                                        for env in
                                                            container.env.as_ref().unwrap()
                                                        {
                                                            // If found, remove the uuid from availability
                                                            if env.name == "CLIENT_DEVICE_ID" {
                                                                let unavailable_uuid =
                                                                    env.value.as_ref().unwrap();
                                                                let index = available_uuids
                                                                    .iter()
                                                                    .position(|x| {
                                                                        x == unavailable_uuid
                                                                    })
                                                                    .unwrap();
                                                                info!("Removing uuid {} from availability", unavailable_uuid);
                                                                available_uuids.remove(index);
                                                            }
                                                        }
                                                    }
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }

                        info!("Available uuids: {:?}", available_uuids);
                        if available_uuids.len() > 0 {
                            let available_uuid = available_uuids.get(0).unwrap();
                            pool_tx
                                .send(InjectorProtocol::FoundDeviceId {
                                    message_id: message_id,
                                    device_id: Uuid::parse_str(&available_uuid).unwrap(),
                                })
                                .expect("Error when sending FoundDeviceId message");
                        } else {
                            info!("No available uuids in Device ID {}", service_id.to_string());
                        }
                    }
                    Err(err) => {
                        error!("Error when getting the device id: {:?}", err);
                    }
                }
            }

            _ => {}
        }
    }*/

    pub fn new(store: Option<Box<dyn IdentityStore<ServiceIdentity> + Send>>) -> Self {
        InjectorPool {
            store: store.unwrap_or(Box::new(InMemoryIdentityStore::new())),
        }
    }
}
