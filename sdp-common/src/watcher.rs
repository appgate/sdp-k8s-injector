use futures::{StreamExt, TryStreamExt};
use std::fmt::Debug;

use kube::{
    api::ListParams,
    runtime::watcher::{self, Event},
    Api, Resource,
};
use log::{error, info};
use serde::de::DeserializeOwned;
use tokio::sync::mpsc::{Receiver, Sender};

use crate::errors::SDPServiceError;

pub trait SimpleWatchingProtocol<P> {
    fn initialized(&self) -> Option<P>;
    fn applied(&self) -> Option<P>;
    fn deleted(&self) -> Option<P>;
}

pub struct WatcherWaitReady<R>(pub Receiver<R>, pub fn(R) -> bool);

pub struct Watcher<E, P> {
    pub api: Api<E>,
    pub queue_tx: Sender<P>,
    pub notification_message: Option<P>,
}

pub async fn watch<E, P, R>(
    watcher: Watcher<E, P>,
    wait_ready: Option<WatcherWaitReady<R>>,
) -> Result<(), SDPServiceError>
where
    E: Clone + Debug + Send + DeserializeOwned + Resource + SimpleWatchingProtocol<P> + 'static,
{
    // Initializing watcher
    let xs = watcher
        .api
        .list(&ListParams::default())
        .await
        .map_err(|e| format!("Error initializing watcher: {}", e))?;

    let init_msgs = xs
        .items
        .iter()
        .map(|e| e.initialized())
        .filter(|e| e.is_some());
    for msg in init_msgs {
        if let Err(e) = watcher.queue_tx.send(msg.unwrap()).await {
            error!("Error sending Initialized message: {}", e.to_string());
        };
    }

    // Notify if needed
    if let Some(msg) = watcher.notification_message {
        if let Err(err) = watcher.queue_tx.send(msg).await {
            error!("Error sending notification message: {}", err);
        }
    }

    if let Some(WatcherWaitReady(mut queue_rx, continue_f)) = wait_ready {
        while let Some(msg) = queue_rx.recv().await {
            if continue_f(msg) {
                info!("Watcher is ready");
                break;
            }
        }
    }

    // Run the watcher
    let xs = watcher::watcher(watcher.api, ListParams::default());
    let mut xs = xs.boxed();
    loop {
        match xs.try_next().await {
            Ok(Some(Event::Applied(e))) => {
                if let Some(msg) = e.applied() {
                    if let Err(e) = watcher.queue_tx.send(msg).await {
                        error!("Error sending Applied message: {}", e.to_string())
                    }
                }
            }
            Ok(Some(Event::Deleted(e))) => {
                if let Some(msg) = e.deleted() {
                    if let Err(e) = watcher.queue_tx.send(msg).await {
                        error!("Error sending Deleted message: {}", e.to_string())
                    }
                }
            }
            Ok(Some(Event::Restarted(_))) => {}
            Ok(None) => {}
            Err(e) => {
                error!("Error reading ServiceIdentity events: {}", e.to_string());
            }
        }
    }
}
