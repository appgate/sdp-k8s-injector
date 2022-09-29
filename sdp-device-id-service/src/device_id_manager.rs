use crate::service_identity_watcher::ServiceIdentityWatcherProtocol;
use k8s_openapi::api::apps::v1::ReplicaSet;
use k8s_openapi::apimachinery::pkg::apis::meta::v1::OwnerReference;
use kube::api::{DeleteParams, ListParams, PostParams};
use kube::{Api, Client, Error as KError, Resource};
use log::{error, info, warn};
use sdp_common::crd::{DeviceId, DeviceIdSpec, ServiceIdentity};
use sdp_common::kubernetes::SDP_K8S_NAMESPACE;
use sdp_common::service::{HasCredentials, ServiceCandidate};
use sdp_macros::{queue_debug, sdp_error, sdp_info, sdp_log};
use std::collections::HashMap;
use std::fmt::Debug;
use std::future::Future;
use std::pin::Pin;
use tokio::sync::mpsc::{Receiver, Sender};
use uuid::Uuid;

#[derive(Debug)]
pub enum DeviceIdManagerProtocol<From: ServiceCandidate + HasCredentials, To: ServiceCandidate> {
    DeviceIdManagerDebug(String),
    CreateDeviceId { service_identity_ref: From },
    DeleteDeviceId { device_id: To },
    DeviceIdManagerInitialized,
    DeviceIdManagerStarted,
    FoundServiceIdentity { service_identity_ref: From },
}

#[derive(Default)]
struct DeviceIdManagerPool {
    device_id_map: HashMap<String, DeviceId>,
}

trait DeviceIdProvider {
    type From: ServiceCandidate + HasCredentials + Send;
    type To: ServiceCandidate + Send;
    fn register(&mut self, to: Self::To) -> ();
    fn unregister(&mut self, to: &Self::To) -> Option<Self::To>;
    fn device_id(&self, from: &Self::From) -> Option<&Self::To>;
    fn device_ids(&self) -> Vec<&Self::To>;
    fn next_device_id(&self, from: &Self::From) -> Option<Self::To>;
}

impl DeviceIdProvider for DeviceIdManagerPool {
    type From = ServiceIdentity;
    type To = DeviceId;

    fn register(&mut self, to: Self::To) -> () {
        self.device_id_map.insert(to.service_id_key(), to);
    }

    fn unregister(&mut self, to: &Self::To) -> Option<Self::To> {
        self.device_id_map.remove(&to.service_id_key())
    }

    fn device_id(&self, from: &Self::From) -> Option<&Self::To> {
        self.device_id_map.get(&from.service_id_key())
    }

    fn device_ids(&self) -> Vec<&Self::To> {
        self.device_id_map.values().collect()
    }

    fn next_device_id(&self, from: &Self::From) -> Option<Self::To> {
        let id = from.service_id();
        Some(DeviceId::new(
            &id,
            DeviceIdSpec {
                uuids: vec![],
                service_name: ServiceCandidate::name(from),
                service_namespace: ServiceCandidate::namespace(from),
            },
        ))
    }
}

trait DeviceIdAPI {
    fn create<'a>(
        &'a self,
        device_id: &'a DeviceId,
    ) -> Pin<Box<dyn Future<Output = Result<DeviceId, KError>> + Send + '_>>;

    fn delete<'a>(
        &'a self,
        device_id_name: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<(), KError>> + Send + '_>>;

    fn list(&self) -> Pin<Box<dyn Future<Output = Result<Vec<DeviceId>, KError>> + Send + '_>>;
}

#[sdp_proc_macros::device_id_provider()]
#[derive(sdp_proc_macros::DeviceIdProvider)]
#[DeviceIdProvider(From = "ServiceIdentity", To = "DeviceId")]
pub struct KubeDeviceIdManager {
    client: Client,
}

impl DeviceIdAPI for KubeDeviceIdManager {
    fn create<'a>(
        &'a self,
        device_id: &'a DeviceId,
    ) -> Pin<Box<dyn Future<Output = Result<DeviceId, KError>> + Send + '_>> {
        let fut = async move {
            let device_id_api: Api<DeviceId> =
                Api::namespaced(self.client.clone(), SDP_K8S_NAMESPACE);
            match device_id_api.get_opt(&device_id.service_id()).await {
                Ok(None) => {
                    info!(
                        "DeviceIds {} does not exist, creating it.",
                        device_id.service_id()
                    );
                    let service_name = &device_id.spec.service_name;
                    let service_namespace = &device_id.spec.service_namespace;

                    let mut uuids: Vec<String> = vec![];
                    let mut owner_ref = OwnerReference::default();
                    let device_id_name = &format!("{}-{}", service_namespace, service_name);

                    let replicaset_api: Api<ReplicaSet> =
                        Api::namespaced(self.client.clone(), service_namespace);
                    let replicasets = replicaset_api
                        .list(&ListParams::default())
                        .await
                        .expect("Unable to list replicaset");
                    for replicaset in replicasets {
                        if let Some(replicaset_owners) =
                            &replicaset.meta().owner_references.as_ref()
                        {
                            let owner = &replicaset_owners[0];
                            if owner.name == *service_name {
                                let service_identity_api: Api<ServiceIdentity> =
                                    Api::namespaced(self.client.clone(), SDP_K8S_NAMESPACE);
                                let service_identity = service_identity_api
                                    .get(device_id_name)
                                    .await
                                    .expect("Unable to get service identity");
                                owner_ref.controller = Default::default();
                                owner_ref.block_owner_deletion = Some(true);
                                owner_ref.name = service_identity.metadata.name.unwrap();
                                owner_ref.api_version = "injector.sdp.com/v1".to_string();
                                owner_ref.kind = "ServiceIdentity".to_string();
                                owner_ref.uid =
                                    service_identity.metadata.uid.clone().unwrap_or_default();

                                if let Some(num_replicas) = replicaset.spec.unwrap().replicas {
                                    for _ in 0..(2 * num_replicas) {
                                        let uuid = Uuid::new_v4().to_string();
                                        info!(
                                            "Assigning uuid {} to DeviceID {}",
                                            uuid, device_id_name
                                        );
                                        uuids.push(uuid);
                                    }
                                }
                            }
                        }
                    }
                    let mut device_id = DeviceId::new(
                        device_id_name,
                        DeviceIdSpec {
                            uuids,
                            service_name: service_name.to_string(),
                            service_namespace: service_namespace.to_string(),
                        },
                    );
                    device_id.metadata.owner_references = Some(vec![owner_ref]);
                    let device_id_api: Api<DeviceId> =
                        Api::namespaced(self.client.clone(), SDP_K8S_NAMESPACE);
                    device_id_api
                        .create(&PostParams::default(), &device_id)
                        .await
                }
                Ok(_) => {
                    info!("DeviceIds {} already exists.", device_id.service_id());
                    Ok(device_id.clone())
                }
                Err(e) => {
                    error!(
                        "Error checking if device ids {} exists.",
                        device_id.service_id()
                    );
                    Err(e)
                }
            }
        };
        Box::pin(fut)
    }

    fn delete<'a>(
        &'a self,
        device_id_name: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<(), KError>> + Send + '_>> {
        let fut = async move {
            let device_id_api: Api<DeviceId> =
                Api::namespaced(self.client.clone(), "purple-devops");
            device_id_api
                .delete(device_id_name, &DeleteParams::default())
                .await
                .map(|_| ())
        };
        Box::pin(fut)
    }

    fn list(&self) -> Pin<Box<dyn Future<Output = Result<Vec<DeviceId>, KError>> + Send + '_>> {
        let fut = async move {
            let device_id_api: Api<DeviceId> = Api::namespaced(self.client.clone(), SDP_K8S_NAMESPACE);
            device_id_api
                .list(&ListParams::default())
                .await
                .map(|d| d.items)
        };
        Box::pin(fut)
    }
}

trait DeviceIdManager<
    From: ServiceCandidate + HasCredentials + Send + Sync,
    To: ServiceCandidate + Send + Sync,
>: DeviceIdAPI + DeviceIdProvider<From = From, To = To>
{
}

pub struct DeviceIdManagerRunner<
    From: ServiceCandidate + HasCredentials + Send,
    To: ServiceCandidate + Send,
> {
    dm: Box<dyn DeviceIdManager<From, To> + Send + Sync>,
}

impl DeviceIdManagerRunner<ServiceIdentity, DeviceId> {
    pub fn kube_runner(client: Client) -> DeviceIdManagerRunner<ServiceIdentity, DeviceId> {
        DeviceIdManagerRunner {
            dm: Box::new(KubeDeviceIdManager {
                pool: DeviceIdManagerPool {
                    device_id_map: HashMap::new(),
                },
                client,
            }),
        }
    }

    async fn initialize<F: ServiceCandidate + HasCredentials + Send>(
        dm: &mut Box<dyn DeviceIdManager<F, DeviceId> + Send + Sync>,
    ) -> () {
        match dm.list().await {
            Ok(device_ids) => {
                device_ids.iter().for_each(|d| {
                    info!("Restoring Device ID");
                    dm.register(d.clone());
                });
                info!("Restored {} Device IDs", device_ids.len())
            }
            Err(err) => {
                panic!("Error fetching the list of DeviceIds: {}", err)
            }
        }
    }

    async fn run_device_id_manager<F: ServiceCandidate + HasCredentials + Send + Debug>(
        dm: &mut Box<dyn DeviceIdManager<F, DeviceId> + Send + Sync>,
        mut manager_proto_rx: Receiver<DeviceIdManagerProtocol<F, DeviceId>>,
        manager_proto_tx: Sender<DeviceIdManagerProtocol<F, DeviceId>>,
        _watcher_proto_tx: Sender<ServiceIdentityWatcherProtocol>,
        queue_tx: Option<&Sender<DeviceIdManagerProtocol<F, DeviceId>>>,
    ) {
        info!("Entering Device ID Manager main loop");
        queue_debug!(DeviceIdManagerProtocol::<F, DeviceId>::DeviceIdManagerStarted => queue_tx);

        while let Some(message) = manager_proto_rx.recv().await {
            match message {
                DeviceIdManagerProtocol::DeviceIdManagerStarted => {}

                DeviceIdManagerProtocol::CreateDeviceId {
                    service_identity_ref,
                } => {
                    sdp_info!(DeviceIdManagerProtocol::<F, DeviceId>::DeviceIdManagerDebug | (
                        "Received request for new DeviceId for ServiceIdentity {}", service_identity_ref.service_id()
                    ) => queue_tx);

                    match dm.next_device_id(&service_identity_ref) {
                        Some(d) => match dm.create(&d).await {
                            Ok(device_id) => {
                                sdp_info!(DeviceIdManagerProtocol::<F, DeviceId>::DeviceIdManagerDebug | (
                                    "Created DeviceID {} for ServiceIdentity {}", device_id.service_id(), service_identity_ref.service_id()
                                ) => queue_tx);
                            }
                            Err(error) => {
                                sdp_error!(DeviceIdManagerProtocol::<F, DeviceId>::DeviceIdManagerDebug | (
                                    "Error creating DeviceId for ServiceIdentity {}: {}", service_identity_ref.service_id(), error
                                ) => queue_tx);
                            }
                        },
                        _ => {}
                    }
                }

                DeviceIdManagerProtocol::DeleteDeviceId { device_id: _ } => {}

                DeviceIdManagerProtocol::FoundServiceIdentity {
                    service_identity_ref,
                } => {
                    sdp_info!(DeviceIdManagerProtocol::<F, DeviceId>::DeviceIdManagerDebug | (
                        "Found ServiceIdentity {}",
                        service_identity_ref.service_id()
                    ) => queue_tx);

                    manager_proto_tx
                        .send(DeviceIdManagerProtocol::CreateDeviceId {
                            service_identity_ref,
                        })
                        .await
                        .expect("Unable to send CreateDeviceId message");
                }
                _ => {
                    warn!("Ignored message");
                }
            }
        }
    }

    pub async fn run(
        mut self,
        manager_proto_rx: Receiver<DeviceIdManagerProtocol<ServiceIdentity, DeviceId>>,
        manager_proto_tx: Sender<DeviceIdManagerProtocol<ServiceIdentity, DeviceId>>,
        watcher_proto_tx: Sender<ServiceIdentityWatcherProtocol>,
        queue_tx: Option<Sender<DeviceIdManagerProtocol<ServiceIdentity, DeviceId>>>,
    ) -> () {
        info!("Starting Device ID Manager");
        DeviceIdManagerRunner::initialize(&mut self.dm).await;
        queue_debug!(DeviceIdManagerProtocol::<ServiceIdentity, DeviceId>::DeviceIdManagerInitialized => queue_tx);

        watcher_proto_tx
            .send(ServiceIdentityWatcherProtocol::DeviceIdManagerReady)
            .await
            .expect("Unable to send DeviceIdManagerReady message");

        DeviceIdManagerRunner::run_device_id_manager(
            &mut self.dm,
            manager_proto_rx,
            manager_proto_tx,
            watcher_proto_tx,
            queue_tx.as_ref(),
        )
        .await;
    }
}

#[cfg(test)]
mod tests {
    use super::{
        DeviceIdAPI, DeviceIdManager, DeviceIdManagerPool, DeviceIdManagerProtocol,
        DeviceIdProvider,
    };
    use crate::DeviceIdManagerRunner;
    use crate::ServiceIdentityWatcherProtocol;
    use futures::future;
    use kube::error::Error;
    use sdp_common::crd::{DeviceId, DeviceIdSpec, ServiceIdentity, ServiceIdentitySpec};
    use sdp_common::service::ServiceUser;
    use sdp_macros::{device_id, service_identity, service_user};
    use sdp_test_macros::assert_message;
    use std::collections::HashMap;
    use std::future::Future;
    use std::pin::Pin;
    use std::sync::{Arc, Mutex};
    use tokio::sync::mpsc::channel;
    use tokio::time::{timeout, Duration};

    #[derive(Default)]
    struct APICounters {
        delete_calls: usize,
        create_calls: usize,
        list_calls: usize,
    }

    #[sdp_proc_macros::device_id_provider()]
    #[derive(sdp_proc_macros::DeviceIdProvider, Default)]
    #[DeviceIdProvider(From = "ServiceIdentity", To = "DeviceId")]
    struct TestDeviceIdManager {
        api_counters: Arc<Mutex<APICounters>>,
    }

    impl TestDeviceIdManager {}

    impl DeviceIdAPI for TestDeviceIdManager {
        fn create<'a>(
            &'a self,
            device_id: &'a DeviceId,
        ) -> Pin<Box<dyn Future<Output = Result<DeviceId, Error>> + Send + '_>> {
            self.api_counters.lock().unwrap().create_calls += 1;
            Box::pin(future::ready(Ok(device_id.clone())))
        }

        fn delete<'a>(
            &'a self,
            _: &'a str,
        ) -> Pin<Box<dyn Future<Output = Result<(), Error>> + Send + '_>> {
            self.api_counters.lock().unwrap().delete_calls += 1;
            Box::pin(future::ready(Ok(())))
        }

        fn list(&self) -> Pin<Box<dyn Future<Output = Result<Vec<DeviceId>, Error>> + Send + '_>> {
            self.api_counters.lock().unwrap().list_calls += 1;
            Box::pin(future::ready(Ok(vec![])))
        }
    }

    fn new_test_device_id_manager() -> Box<TestDeviceIdManager> {
        Box::new(TestDeviceIdManager::default())
    }

    fn new_test_device_id_runner(
        dm: Box<TestDeviceIdManager>,
    ) -> DeviceIdManagerRunner<ServiceIdentity, DeviceId> {
        DeviceIdManagerRunner {
            dm: dm as Box<dyn DeviceIdManager<ServiceIdentity, DeviceId> + Send + Sync>,
        }
    }

    macro_rules! test_device_id_manager {
        (($dm:ident($vs:expr), $queue_rx:ident, $manager_tx:ident, $watcher_rx:ident, $counters:ident) => $e:expr) => {
            let ($manager_tx, manager_rx) =
                channel::<DeviceIdManagerProtocol<ServiceIdentity, DeviceId>>(10);
            let (watcher_tx, $watcher_rx) = channel::<ServiceIdentityWatcherProtocol>(10);
            let (queue_tx, mut $queue_rx) =
                channel::<DeviceIdManagerProtocol<ServiceIdentity, DeviceId>>(10);

            let mut $dm = new_test_device_id_manager();
            for device_id in $vs.clone() {
                $dm.register(device_id);
            }

            let manager_tx_2 = $manager_tx.clone();
            let $counters = $dm.api_counters.clone();
            tokio::spawn(async move {
                let runner = new_test_device_id_runner($dm);
                runner
                    .run(manager_rx, manager_tx_2, watcher_tx, Some(queue_tx))
                    .await
            });
            $e
        };

        (($queue_rx:ident, $manager_tx:ident, $watcher_rx:ident, $counters:ident) => $e:expr) => {
            let device_ids = vec![device_id!(1), device_id!(2), device_id!(3)];
            test_device_id_manager! {
                (dm(device_ids), $queue_rx, $manager_tx, $watcher_rx, $counters) => {
                    $e
                }
            }
        };
    }

    #[tokio::test]
    async fn test_device_id_manager_init() {
        test_device_id_manager! {
            (queue_rx, manager_tx, _watcher_rx, counters) => {
                assert_message! {
                    (msg :: DeviceIdManagerProtocol::DeviceIdManagerInitialized in queue_rx) => {
                        assert_eq!(counters.lock().unwrap().list_calls, 1);
                        assert_message!(msg :: DeviceIdManagerProtocol::DeviceIdManagerStarted in queue_rx);
                    }
                }
            }
        }
    }

    #[tokio::test]
    async fn test_device_id_manager_request_device_id() {
        test_device_id_manager! {
            (queue_rx, manager_tx, _watcher_rx, counters) => {
                assert_message!(m :: DeviceIdManagerProtocol::DeviceIdManagerInitialized in queue_rx);
                assert_message!(m :: DeviceIdManagerProtocol::DeviceIdManagerStarted in queue_rx);

                let tx = manager_tx.clone();

                // Watcher notifies the manager about a new inactive ServiceIdentity
                tx.send(DeviceIdManagerProtocol::FoundServiceIdentity {
                    service_identity_ref: service_identity!(1),
                }).await.expect("Unable to send FoundServiceIdentity message");

                assert_message! {
                    (m :: DeviceIdManagerProtocol::DeviceIdManagerDebug(_) in queue_rx) => {
                        if let DeviceIdManagerProtocol::DeviceIdManagerDebug(msg) = m {
                            assert!(msg.eq("Found ServiceIdentity ns1-srv1"), "Wrong message, got {}", msg);
                        }
                    }
                }

                assert_message! {
                    (m :: DeviceIdManagerProtocol::DeviceIdManagerDebug(_) in queue_rx) => {
                        if let DeviceIdManagerProtocol::DeviceIdManagerDebug(msg) = m {
                            assert!(msg.eq("Received request for new DeviceId for ServiceIdentity ns1-srv1"), "Wrong message, got {}", msg)
                        }
                    }
                }

                assert_eq!(counters.lock().unwrap().create_calls, 1);

                assert_message! {
                    (m :: DeviceIdManagerProtocol::DeviceIdManagerDebug(_) in queue_rx) => {
                        if let DeviceIdManagerProtocol::DeviceIdManagerDebug(msg) = m {
                            assert!(msg.eq("Created DeviceID ns1-srv1 for ServiceIdentity ns1-srv1"), "Wrong message, got {}", msg)
                        }
                    }
                }
            }
        }
    }
}
