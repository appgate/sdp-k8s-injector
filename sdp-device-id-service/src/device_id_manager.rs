use crate::device_id_creator::DeviceIdCreatorProtocol;
use crate::service_identity_watcher::ServiceIdentityWatcherProtocol;
use kube::api::{DeleteParams, ListParams, PostParams};
use kube::{Api, Client, Error as KError};
use log::{error, info};
use sdp_common::crd::{DeviceId, ServiceIdentity};
use sdp_common::device_id::DeviceIdCandidate;
use sdp_common::kubernetes::SDP_K8S_NAMESPACE;
use sdp_common::service::{HasCredentials, ServiceCandidate};
use std::collections::{HashMap, VecDeque};
use std::future::Future;
use std::pin::Pin;
use tokio::sync::mpsc::{Receiver, Sender};

#[derive(Debug)]
pub enum DeviceIdManagerProtocol<From: ServiceCandidate + HasCredentials, To: DeviceIdCandidate> {
    RequestDeviceId { device_id_candidate: From },
    DeleteDeviceId { device_id: To },
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

#[sdp_macros::device_id_provider()]
#[derive(sdp_macros::DeviceIdProvider)]
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

    async fn run_device_id_manager<F: ServiceCandidate + HasCredentials + Send>(
        dm: &mut Box<dyn DeviceIdManager<F, DeviceId> + Send + Sync>,
        mut manager_proto_rx: Receiver<DeviceIdManagerProtocol<F, DeviceId>>,
        manager_proto_tx: Sender<DeviceIdManagerProtocol<F, DeviceId>>,
        creator_proto_tx: Sender<DeviceIdCreatorProtocol>,
        watcher_proto_tx: Sender<ServiceIdentityWatcherProtocol>,
        option: Option<&Sender<DeviceIdManagerProtocol<F, DeviceId>>>,
    ) {
        info!("Entering Device ID Manager main loop");
        while let Some(message) = manager_proto_rx.recv().await {
            match message {
                DeviceIdManagerProtocol::DeviceIdManagerStarted => {}
                DeviceIdManagerProtocol::RequestDeviceId {
                    device_id_candidate: _,
                } => {}
                DeviceIdManagerProtocol::DeleteDeviceId { device_id: _ } => {}
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
    use crate::device_id_manager::{
        DeviceIdAPI, DeviceIdManager, DeviceIdManagerPool, DeviceIdPool, DeviceIdProvider,
    };
    use crate::DeviceIdManagerRunner;
    use futures::future;
    use kube::error::Error;
    use sdp_common::crd::{DeviceId, ServiceIdentity};
    use std::future::Future;
    use std::pin::Pin;
    use std::process::Output;
    use std::sync::{Arc, Mutex};

    #[derive(Default)]
    struct APICounters {
        delete_calls: usize,
        create_calls: usize,
        list_calls: usize,
    }

    #[sdp_macros::device_id_provider()]
    #[derive(sdp_macros::DeviceIdProvider, Default)]
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
}
