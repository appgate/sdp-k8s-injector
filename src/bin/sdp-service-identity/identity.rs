mod identity {
    use futures::StreamExt;
    use k8s_openapi::api::apps::v1::Deployment;
    use kube::{Api, Client, api::ListParams};
    use kube_runtime::watcher;
    use kube_runtime::watcher::Event;
    use log::info;
    use tokio::sync::mpsc::{channel, Receiver, Sender};

    /// Messages exchanged between different components
    pub enum IdentityMessage {
        /// Message used to request a new user
        RequestIdentity {
            service_name: String,
            service_ns: String,
        },
        /// Message used to send a new user
        NewIdentity { secret_name: String },
    }

    pub struct IdentityManager;

    async fn identity_manager(mut rx: Receiver<IdentityMessage>) -> () {
        while let Some(msg) = rx.recv().await {
            match msg {
                IdentityMessage::RequestIdentity {
                    service_name,
                    service_ns,
                } => {
                    info!(
                        "New user requested for service {}[{}]",
                        service_name, service_ns
                    );
                }
                _ => {
                    info!("Unknown message");
                }
            }
        }
    }

    impl IdentityManager {  
        async fn run() -> Sender<IdentityMessage> {
            let (tx, mut rx) = channel::<IdentityMessage>(50);
            tokio::spawn(async move {
                identity_manager(rx).await;
            });
            tx
        }
    }

    pub struct DeploymentWatcher;
    async fn watch_deployments(client: Client) -> () {
        let deployments_api: Api<Deployment> = Api::all(client);
        watcher(deployments_api, ListParams::default())
        .for_each_concurrent(5, |res| async move {
            match res {
                Ok(Event::Restarted(deployments)) => {
                    println!("Watcher restarted");
                },
                Ok(Event::Applied(deployment)) => {
                    println!("New deployment or modified deployment");
                },
                Ok(Event::Deleted(deployment)) => {
                    println!("Deleted deployment");
                },
                Err(err) => {
                    println!("Some error")
                }
            }
        }).await
    }

    impl DeploymentWatcher {
        async fn run() -> () {
            tokio::spawn(async move {
                let client = Client::try_default().await.expect("Unable to create K8S client");
                watch_deployments(client);
            });
        }
    }
}
