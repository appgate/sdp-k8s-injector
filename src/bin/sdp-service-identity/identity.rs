mod identity {
    use log::info;
    use tokio::sync::mpsc::{channel, Receiver, Sender};

    /// Messages exchanged between different components
    pub enum Message {
        /// Message used to request a new user
        RequestIdentity {
            service_name: String,
            service_ns: String,
        },
        /// Message used to send a new user
        NewIdentity { secret_name: String },
    }

    async fn identity_dispatcher(mut rx: Receiver<Message>) -> () {
        while let Some(msg) = rx.recv().await {
            match msg {
                Message::RequestIdentity {
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

    async fn run() -> Sender<Message> {
        let (tx, mut rx) = channel::<Message>(50);
        tokio::spawn(async move {
            identity_dispatcher(rx).await;
        });
        tx
    }
}
