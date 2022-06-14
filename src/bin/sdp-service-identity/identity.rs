mod identity {
    use std::borrow::BorrowMut;

    use futures::{channel::mpsc::SendError, StreamExt};
    use k8s_openapi::api::apps::v1::Deployment;
    use kube::{api::ListParams, Api, Client};
    use kube_runtime::watcher;
    use kube_runtime::watcher::Event;
    use log::{error, info, warn};
    use tokio::sync::mpsc::{channel, Receiver, Sender};

    #[derive(Clone)]
    pub struct ServiceIdentity {
        secret: String,
        field: String,
    }

    /// Messages exchanged between different components
    pub enum IdentityMessageRequest {
        /// Message used to request a new user
        RequestIdentity {
            service_name: String,
            service_ns: String,
            channel: Sender<IdentityMessageResponse>,
        },
    }

    pub enum IdentityMessageResponse {
        /// Message used to send a new user
        NewIdentity(ServiceIdentity),
        IdentityUnavailable,
    }

    pub struct IdentityManager {
        pool_size: u8,
        pool: Vec<ServiceIdentity>,
        idx: usize,
    }

    impl IdentityManager {
        fn new() -> IdentityManager {
            IdentityManager {
                pool_size: 30,
                pool: vec![],
                idx: 0,
            }
        }

        fn next_identity(&mut self) -> Option<ServiceIdentity> {
            let identity = self.pool.get(self.idx).map(|id| id.clone());
            if let Some(_) = identity {
                self.idx += 1;
            }
            identity
        }

        async fn run(&mut self, mut rx: Receiver<IdentityMessageRequest>) -> () {
            while let Some(msg) = rx.recv().await {
                match msg {
                    IdentityMessageRequest::RequestIdentity {
                        service_name,
                        service_ns,
                        channel,
                    } => {
                        info!(
                            "New user requested for service {}[{}]",
                            service_name, service_ns
                        );
                        let event = self
                            .next_identity()
                            .map(|id| IdentityMessageResponse::NewIdentity(id))
                            .unwrap_or_else(|| {
                                warn!("Unable to get next ServiceIdentity");
                                IdentityMessageResponse::IdentityUnavailable
                            });
                        if let Err(err) = channel.send(event).await {
                            error!("Found error when notifying ServiceIdentify: {}", err);
                        }
                    }
                }
            }
        }

        async fn initialize(&mut self) -> () {
            ()
        }
    }

    async fn run() -> Sender<IdentityMessageRequest> {
        let (tx, mut rx) = channel::<IdentityMessageRequest>(50);
        let mut identity_manager = IdentityManager::new();
        tokio::spawn(async move {
            identity_manager.borrow_mut().run(rx).await;
        });
        tx
    }

    pub struct DeploymentWatcher;

    async fn watch_deployments(client: Client) -> () {
        let deployments_api: Api<Deployment> = Api::all(client);
        watcher(deployments_api, ListParams::default())
            .for_each_concurrent(5, |res| async move {
                match res {
                    Ok(Event::Restarted(deployments)) => {
                        println!("Watcher restarted");
                    }
                    Ok(Event::Applied(deployment)) => {
                        println!("New deployment or modified deployment");
                    }
                    Ok(Event::Deleted(deployment)) => {
                        println!("Deleted deployment");
                    }
                    Err(err) => {
                        println!("Some error")
                    }
                }
            })
            .await
    }

    impl DeploymentWatcher {
        async fn run() -> () {
            tokio::spawn(async move {
                let client = Client::try_default()
                    .await
                    .expect("Unable to create K8S client");
                watch_deployments(client);
            });
        }
    }
}
