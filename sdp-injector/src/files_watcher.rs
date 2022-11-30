use std::{path::Path, sync::Arc, time::Duration};

use futures::executor::block_on;
use notify::{
    Config as NotifyConfig, Event as NotifyEvent, EventHandler, PollWatcher, RecursiveMode,
    Result as NotifyResult, Watcher as INotifyWatcher,
};
use sdp_macros::{logger, sdp_debug, sdp_info, sdp_log, with_dollar_sign};
use tokio::sync::Mutex as AsyncMutex;

logger!("FilesWatcher");

pub const SDP_FILE_WATCHER_POLL_INTERVAL_ENV: &str = "SDP_FILE_WATCHER_POLL_INTERVAL";
pub const SDP_FILE_WATCHER_POLL_INTERVAL: u64 = 900;

struct TokioSenderHandler {
    pub sender: tokio::sync::mpsc::Sender<NotifyResult<NotifyEvent>>,
}

impl EventHandler for TokioSenderHandler {
    fn handle_event(&mut self, event: NotifyResult<NotifyEvent>) {
        debug!("Got iNotify event: {:?}", event);
        block_on(async {
            let _ = self.sender.send(event).await;
        });
    }
}

pub struct FilesWatcher {
    _watcher: PollWatcher,
    pub receiver: tokio::sync::mpsc::Receiver<NotifyResult<NotifyEvent>>,
}

impl FilesWatcher {
    pub fn new<'a>(paths: Vec<&'a str>, interval: Duration) -> NotifyResult<Self> {
        let (tx, rx) = tokio::sync::mpsc::channel::<NotifyResult<NotifyEvent>>(1);
        let mut watcher = PollWatcher::new(
            TokioSenderHandler { sender: tx },
            NotifyConfig::default()
                .with_compare_contents(true)
                .with_poll_interval(interval),
        )?;

        for p in paths {
            if let Err(e) = watcher.watch(Path::new(p), RecursiveMode::Recursive) {
                panic!("Error when watching changes in file {:?}: {}", p, e);
            };
            info!("Watching for changes on path: {}", p);
        }
        Ok(FilesWatcher {
            _watcher: watcher,
            receiver: rx,
        })
    }
}

pub async fn watch_files(
    mut file_watcher: FilesWatcher,
    reload_certs: Arc<AsyncMutex<bool>>,
) -> NotifyResult<()> {
    info!("Waiting now for events! {:?}", file_watcher.receiver);

    while let Some(res) = file_watcher.receiver.recv().await {
        match res {
            Ok(NotifyEvent {
                kind,
                paths: _,
                attrs: _,
            }) if kind.is_modify() => {
                info!("Certificate has been updated");
                *reload_certs.lock().await = true;
            }
            Ok(event) => {
                info!("Ignoring iNotify event: {:?}", event);
            }
            Err(e) => {
                info!("iNotify error: {:?}", e);
            }
        }
    }
    Ok(())
}
