use crate::device_id_creator::DeviceIdCreatorProtocol;
use crate::service_identity_watcher::ServiceIdentityWatcherProtocol;
use kube::api::{DeleteParams, ListParams, PostParams};
use kube::{Api, Client, Error as KError};
use log::{error, info, warn};
use sdp_common::crd::{DeviceId, ServiceIdentity};
use sdp_common::device_id::DeviceIdCandidate;
use sdp_common::kubernetes::SDP_K8S_NAMESPACE;
use sdp_common::service::{HasCredentials, ServiceCandidate};
use sdp_macros::queue_debug;
use std::collections::{HashMap, VecDeque};
use std::fmt::Debug;
use std::future::Future;
use std::pin::Pin;
use tokio::sync::mpsc::{Receiver, Sender};

#[derive(Debug)]
pub enum DeviceIdManagerProtocol<From: ServiceCandidate + HasCredentials, To: DeviceIdCandidate> {
    RequestDeviceId { device_id_candidate: From },
    DeleteDeviceId { device_id: To },
    DeviceIdManagerInitialized,
    DeviceIdManagerStarted,
}

#[derive(Default)]
struct DeviceIdManagerPool {
    pool: VecDeque<String>,
    device_id_map: HashMap<String, DeviceId>,
}

trait DeviceIdPool {
    fn pop(&mut self) -> Option<String>;
    fn push(&mut self, device_id: String) -> ();
    fn needs_new_device_id(&self) -> bool;
}

impl DeviceIdPool for DeviceIdManagerPool {
    fn pop(&mut self) -> Option<String> {
        self.pool.pop_front()
    }

    fn push(&mut self, device_id: String) -> () {
        self.pool.push_back(device_id)
    }

    fn needs_new_device_id(&self) -> bool {
        self.pool.len() < 10
    }
}

trait DeviceIdProvider {
    type From: ServiceCandidate + HasCredentials + Send;
    type To: DeviceIdCandidate + Send;
    fn register(&mut self, to: Self::To) -> ();
    fn unregister(&mut self, to: &Self::To) -> Option<Self::To>;
    fn device_id(&self, from: &Self::From) -> Option<&Self::To>;
    fn device_ids(&self) -> Vec<&Self::To>;
}

impl DeviceIdProvider for DeviceIdManagerPool {
    type From = ServiceIdentity;
    type To = DeviceId;

    fn register(&mut self, to: Self::To) -> () {
        self.device_id_map.insert(to.service_identity_id(), to);
    }

    fn unregister(&mut self, to: &Self::To) -> Option<Self::To> {
        self.device_id_map.remove(&to.service_identity_id())
    }

    fn device_id(&self, from: &Self::From) -> Option<&Self::To> {
        self.device_id_map.get(&from.service_identity_id())
    }

    fn device_ids(&self) -> Vec<&Self::To> {
        self.device_id_map.values().collect()
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
    device_id_api: Api<DeviceId>,
}

impl DeviceIdAPI for KubeDeviceIdManager {
    fn create<'a>(
        &'a self,
        device_id: &'a DeviceId,
    ) -> Pin<Box<dyn Future<Output = Result<DeviceId, KError>> + Send + '_>> {
        let fut = async move {
            self.device_id_api
                .create(&PostParams::default(), device_id)
                .await
        };
        Box::pin(fut)
    }

    fn delete<'a>(
        &'a self,
        device_id_name: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<(), KError>> + Send + '_>> {
        let fut = async move {
            self.device_id_api
                .delete(device_id_name, &DeleteParams::default())
                .await
                .map(|_| ())
        };
        Box::pin(fut)
    }

    fn list(&self) -> Pin<Box<dyn Future<Output = Result<Vec<DeviceId>, KError>> + Send + '_>> {
        let fut = async move {
            self.device_id_api
                .list(&ListParams::default())
                .await
                .map(|d| d.items)
        };
        Box::pin(fut)
    }
}

trait DeviceIdManager<
    From: ServiceCandidate + HasCredentials + Send + Sync,
    To: DeviceIdCandidate + Send + Sync,
>: DeviceIdAPI + DeviceIdProvider<From = From, To = To>
{
}

pub struct DeviceIdManagerRunner<
    From: ServiceCandidate + HasCredentials + Send,
    To: DeviceIdCandidate + Send,
> {
    dm: Box<dyn DeviceIdManager<From, To> + Send + Sync>,
}

impl DeviceIdManagerRunner<ServiceIdentity, DeviceId> {
    pub fn kube_runner(client: Client) -> DeviceIdManagerRunner<ServiceIdentity, DeviceId> {
        let device_id_api: Api<DeviceId> = Api::namespaced(client, SDP_K8S_NAMESPACE);
        DeviceIdManagerRunner {
            dm: Box::new(KubeDeviceIdManager {
                pool: DeviceIdManagerPool {
                    pool: VecDeque::with_capacity(30),
                    device_id_map: HashMap::new(),
                },
                device_id_api,
            }),
        }
    }

    async fn initialize<F: ServiceCandidate + HasCredentials + Send>(
        dm: &mut Box<dyn DeviceIdManager<F, DeviceId> + Send + Sync>,
    ) -> () {
        match dm.list().await {
            Ok(device_ids) => {
                info!("Restoring existing DeviceId");
                device_ids.iter().for_each(|d| {
                    info!("Restoring Device ID: {}", d.name());
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
        creator_proto_tx: Sender<DeviceIdCreatorProtocol>,
        watcher_proto_tx: Sender<ServiceIdentityWatcherProtocol>,
        queue_tx: Option<&Sender<DeviceIdManagerProtocol<F, DeviceId>>>,
    ) {
        info!("Entering Device ID Manager main loop");

        queue_debug!(DeviceIdManagerProtocol::<F, DeviceId>::DeviceIdManagerStarted => queue_tx);

        while let Some(message) = manager_proto_rx.recv().await {
            match message {
                DeviceIdManagerProtocol::DeviceIdManagerStarted => {}
                DeviceIdManagerProtocol::RequestDeviceId {
                    device_id_candidate: _,
                } => {}
                DeviceIdManagerProtocol::DeleteDeviceId { device_id: _ } => {}
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
        creator_proto_tx: Sender<DeviceIdCreatorProtocol>,
        watcher_proto_tx: Sender<ServiceIdentityWatcherProtocol>,
        queue_tx: Option<Sender<DeviceIdManagerProtocol<ServiceIdentity, DeviceId>>>,
    ) -> () {
        info!("Starting Device ID Manager");
        DeviceIdManagerRunner::initialize(&mut self.dm).await;
        queue_debug!(DeviceIdManagerProtocol::<ServiceIdentity, DeviceId>::DeviceIdManagerInitialized => queue_tx);

        if let Err(err) = creator_proto_tx
            .send(DeviceIdCreatorProtocol::StartCreator)
            .await
        {
            error!("Error starting Device ID Creator: {}", err)
        }

        DeviceIdManagerRunner::run_device_id_manager(
            &mut self.dm,
            manager_proto_rx,
            manager_proto_tx,
            creator_proto_tx,
            watcher_proto_tx,
            queue_tx.as_ref(),
        )
        .await;
    }
}

#[cfg(test)]
mod tests {
    use super::{
        DeviceIdAPI, DeviceIdManager, DeviceIdManagerPool, DeviceIdManagerProtocol, DeviceIdPool,
        DeviceIdProvider,
    };
    use crate::DeviceIdCreatorProtocol;
    use crate::DeviceIdManagerRunner;
    use crate::ServiceIdentityWatcherProtocol;
    use futures::future;
    use kube::error::Error;
    use sdp_common::crd::{DeviceId, DeviceIdSpec, ServiceIdentity, ServiceIdentitySpec};
    use sdp_common::service::ServiceCredentialsRef;
    use sdp_macros::{credentials_ref, device_id, service_identity};
    use sdp_test_macros::{assert_message, assert_no_message};
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
        (($dm:ident($vs:expr), $queue_rx:ident, $manager_tx:ident, $creator_rx:ident, $watcher_rx:ident, $counters:ident) => $e:expr) => {
            let ($manager_tx, manager_rx) =
                channel::<DeviceIdManagerProtocol<ServiceIdentity, DeviceId>>(10);
            let (creator_tx, mut $creator_rx) = channel::<DeviceIdCreatorProtocol>(10);
            let (watcher_tx, mut $watcher_rx) = channel::<ServiceIdentityWatcherProtocol>(10);
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
                    .run(
                        manager_rx,
                        manager_tx_2,
                        creator_tx,
                        watcher_tx,
                        Some(queue_tx),
                    )
                    .await
            });
            $e
        };

        (($queue_rx:ident, $manager_tx:ident, $creator_rx:ident, $watcher_rx:ident, $counters:ident) => $e:expr) => {
            let device_ids = vec![device_id!(1), device_id!(2), device_id!(3)];
            test_device_id_manager! {
                (dm(device_ids), $queue_rx, $manager_tx, $creator_rx, $watcher_rx, $counters) => {
                    $e
                }
            }
        };
    }

    #[tokio::test]
    async fn test_device_id_manager_init() {
        test_device_id_manager! {
            (queue_rx, manager_tx, creator_rx, watcher_rx, counters) => {
                assert_message! {
                    (msg :: DeviceIdManagerProtocol::DeviceIdManagerInitialized in queue_rx) => {
                        assert_eq!(counters.lock().unwrap().list_calls, 1);
                        assert_message!(msg :: DeviceIdManagerProtocol::DeviceIdManagerStarted in queue_rx);
                    }
                }
            }
        }
    }
}
