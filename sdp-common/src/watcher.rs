use futures::{StreamExt, TryStreamExt};
use k8s_openapi::api::core::v1::Namespace;
use std::{collections::HashSet, fmt::Debug};

use kube::{
    api::ListParams,
    runtime::watcher::{self, Event},
    Api, Resource, ResourceExt,
};
use log::{error, info, warn};
use serde::de::DeserializeOwned;
use tokio::sync::mpsc::{Receiver, Sender};

use crate::errors::SDPServiceError;

pub trait SimpleWatchingProtocol<P> {
    fn initialized(&self, ns: Option<Namespace>) -> Option<P>;
    fn applied(&self) -> Option<P>;
    fn reapplied(&self) -> Option<P>;
    fn deleted(&self) -> Option<P>;
    fn key(&self) -> Option<String>;
}

pub struct WatcherWaitReady<R>(pub Receiver<R>, pub fn(&R) -> bool);

pub struct Watcher<E, P> {
    pub api_ns: Option<Api<Namespace>>,
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
    R: Debug,
{
    let mut applied = HashSet::<String>::new();

    info!("Initializing watcher");
    let xs = watcher
        .api
        .list(&ListParams::default())
        .await
        .map_err(|e| format!("Error initializing watcher: {}", e))?;

    let init_msgs = xs.items.iter().map(|e| (e.key(), e));

    for (key, e) in init_msgs {
        // Get namespace annotations to be make is_candidate to work properly
        // We want to check the labels in the namespace of the entity
        let ns = if watcher.api_ns.is_some() {
            if let Some(ns) = e.namespace() {
                match watcher.api_ns.as_ref().unwrap().get_opt(&ns).await {
                    Ok(ns) => ns,
                    Err(err) => {
                        error!(
                            "Unable to get Namespace for resource {}/ {}: {}",
                            e.name_any(),
                            e.namespace().unwrap_or(format!("Unknown")),
                            err
                        );
                        None
                    }
                }
            } else {
                error!("Namespace not found!");
                None
            }
        } else {
            warn!("NS api not defined");
            None
        };
        // Register as applied if possible
        if let Some(msg) = e.initialized(ns) {
            if let Err(e) = watcher.queue_tx.send(msg).await {
                error!("Error sending Initialized message: {}", e.to_string());
            };
        }
        if let Some(key) = key {
            applied.insert(key);
        }
    }

    // Notify if needed
    if let Some(msg) = watcher.notification_message {
        info!("Notifying other services that we are ready");
        if let Err(err) = watcher.queue_tx.send(msg).await {
            error!("Error sending notification message: {}", err);
        }
    }

    if let Some(WatcherWaitReady(mut queue_rx, continue_f)) = wait_ready {
        info!("Waiting for other services to be ready");
        while let Some(msg) = queue_rx.recv().await {
            if continue_f(&msg) {
                info!("Got message {:?}, watcher is ready to continue:", msg);
                break;
            }
        }
    }

    // Run the watcher
    let xs = watcher::watcher(watcher.api, ListParams::default());
    let mut xs = xs.boxed();
    info!("Starting watcher loop");
    loop {
        match xs.try_next().await {
            Ok(Some(Event::Applied(e))) => {
                let key = e.key();
                let needs_apply = match &key {
                    Some(key) if applied.contains(key) => false,
                    _ => true,
                };
                if needs_apply {
                    if let Some(msg) = e.applied() {
                        if let Err(e) = watcher.queue_tx.send(msg).await {
                            error!("Error sending Applied message: {}", e.to_string())
                        } else {
                            if let Some(key) = key {
                                applied.insert(key);
                            }
                        }
                    }
                }
            }
            Ok(Some(Event::Deleted(e))) => {
                if let Some(msg) = e.deleted() {
                    if let Err(e) = watcher.queue_tx.send(msg).await {
                        error!("Error sending Deleted message: {}", e.to_string())
                    } else {
                        if let Some(key) = e.key() {
                            applied.remove(&key);
                        }
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
