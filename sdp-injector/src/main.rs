use crate::deviceid::{DeviceIdProvider, DeviceIdProviderRequestProtocol};
use crate::files_watcher::{
    watch_files, FilesWatcher, SDP_FILE_WATCHER_POLL_INTERVAL, SDP_FILE_WATCHER_POLL_INTERVAL_ENV,
};
use crate::injector::{
    get_cert_path, get_key_path, injector_handler, load_sidecar_containers, load_ssl,
    InjectorDeviceIdRequester, SDPInjectorContext, SDPSidecars,
};
use futures_util::stream::StreamExt;
use http::Uri;
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper_util::rt::TokioIo;
use kube::{Api, Client, Config};
use sdp_common::crd::ServiceIdentity;
use sdp_common::kubernetes::{KUBE_SYSTEM_NAMESPACE, SDP_K8S_NAMESPACE};
use sdp_common::service::get_log_config_path;
use sdp_common::watcher::{watch, Watcher};
use sdp_macros::{logger, sdp_debug, sdp_error, sdp_info, sdp_log, sdp_warn, with_dollar_sign};
use std::collections::HashMap;
use std::error::Error;
use std::sync::Arc;
use std::time::Duration;
use tls_listener::TlsListener;
use tokio::net::TcpListener;
use tokio::sync::mpsc::channel;
use tokio::sync::Mutex as AsyncMutex;
use tokio::time::timeout;

const SDP_K8S_HOST_ENV: &str = "SDP_K8S_HOST";
const SDP_K8S_HOST_DEFAULT: &str = "kubernetes.default.svc";
const SDP_K8S_NO_VERIFY_ENV: &str = "SDP_K8S_NO_VERIFY";

mod deviceid;
mod errors;
mod files_watcher;
mod injector;
mod service_identity_watcher;

pub type Acceptor = tokio_rustls::TlsAcceptor;

logger!("Main");

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    debug!("Initializing logger");
    log4rs::init_file(get_log_config_path(), Default::default()).unwrap();

    let mut k8s_host = String::from("https://");
    k8s_host.push_str(&std::env::var(SDP_K8S_HOST_ENV).unwrap_or(SDP_K8S_HOST_DEFAULT.to_string()));
    let k8s_uri = k8s_host.parse::<Uri>().expect(
        format!(
            "Unable to parse SDP_K8S_HOST environment value: {}",
            k8s_host
        )
        .as_str(),
    );
    let mut k8s_config = Config::infer()
        .await
        .expect("Unable to infer Kubernetes config");
    k8s_config.cluster_url = k8s_uri;
    k8s_config.accept_invalid_certs = std::env::var(SDP_K8S_NO_VERIFY_ENV)
        .map(|v| v == "1" || v.to_lowercase() == "true")
        .unwrap_or(false);
    debug!("Kubernetes config: {:?}", k8s_config);
    let k8s_client: Client =
        Client::try_from(k8s_config).expect("Unable to create kubernetes client");
    let service_identity_api: Api<ServiceIdentity> =
        Api::namespaced(k8s_client.clone(), SDP_K8S_NAMESPACE);
    let (device_id_tx, device_id_rx) =
        channel::<DeviceIdProviderRequestProtocol<ServiceIdentity>>(50);
    let injector_device_id_requester = InjectorDeviceIdRequester {
        device_id_q_tx: device_id_tx,
    };
    let sdp_sidecars: SDPSidecars =
        load_sidecar_containers().expect("Unable to load the sidecar context");
    let version = k8s_client
        .apiserver_version()
        .await?
        .minor
        .chars()
        .filter(|c| c.is_alphanumeric())
        .collect::<String>()
        .parse::<u32>()?;
    info!("Found Kubernetes server version: {}", version);
    let sdp_injector_context = Arc::new(SDPInjectorContext {
        sdp_sidecars: Arc::new(sdp_sidecars),
        ns_api: Api::all(k8s_client.clone()),
        services_api: Api::namespaced(k8s_client, KUBE_SYSTEM_NAMESPACE),
        device_id_requester: Arc::new(injector_device_id_requester),
        attempts_store: AsyncMutex::new(HashMap::new()),
        server_version: version,
    });

    let ssl_config = load_ssl()?;
    let tls_acceptor: Acceptor = Arc::new(ssl_config).into();

    let addr: std::net::SocketAddr = ([0, 0, 0, 0], 8443).into();

    let reload_certs: Arc<AsyncMutex<bool>> = Arc::new(AsyncMutex::new(false));
    let tcp_listener = TcpListener::bind(&addr).await?;
    let mut tls_listener = TlsListener::new(tls_acceptor, tcp_listener);

    // Thread to watch ServiceIdentity entities
    // We register new ServiceIdentity entities in the store when created and de unregister them when deleted.
    let (watcher_tx, watcher_rx) = channel::<DeviceIdProviderRequestProtocol<ServiceIdentity>>(50);
    tokio::spawn(async move {
        let watcher: Watcher<ServiceIdentity, DeviceIdProviderRequestProtocol<ServiceIdentity>> =
            Watcher {
                api_ns: None,
                api: service_identity_api,
                queue_tx: watcher_tx,
                notification_message: None,
            };
        let w = watch::<
            ServiceIdentity,
            DeviceIdProviderRequestProtocol<ServiceIdentity>,
            DeviceIdProviderRequestProtocol<ServiceIdentity>,
        >(watcher, None);
        if let Err(e) = w.await {
            panic!("Unable to start IdentityService Watcher: {}", e);
        }
    });

    // Spawn the DeviceIdProvider
    tokio::spawn(async move {
        let mut device_id_provider = DeviceIdProvider::new(None);
        // TODO: Once the services are merged we should pass an SDP Client here and make it mandatory
        device_id_provider.run(device_id_rx, watcher_rx, None).await;
    });

    // Thread to watch for notify
    let reload_certs_watcher = Arc::clone(&reload_certs);
    tokio::spawn(async move {
        let poll_interval: u64 = std::env::var(SDP_FILE_WATCHER_POLL_INTERVAL_ENV)
            .map(|s| {
                let secs = s.trim().parse::<u64>();
                if let Err(_) = secs {
                    error!(
                        "Wrong value {} for file watcher poll interval, it must be seconds.",
                        s
                    );
                    SDP_FILE_WATCHER_POLL_INTERVAL
                } else {
                    secs.unwrap()
                }
            })
            .unwrap_or(SDP_FILE_WATCHER_POLL_INTERVAL);
        let file_watchers = FilesWatcher::new(
            vec![&get_cert_path(), &get_key_path()],
            Duration::from_secs(poll_interval),
        );
        if let Err(e) = file_watchers {
            panic!("Unable to create FileWatcher: {}", e);
        }
        if let Err(e) = watch_files(file_watchers.unwrap(), reload_certs_watcher).await {
            panic!("Unable to watch files: {}", e);
        }
    });

    info!("Starting SDP Injector server");
    let reload_certs_lock = Arc::clone(&reload_certs);
    loop {
        // Reload the TLS certificates if the file watcher signalled a change.
        let should_reload = match timeout(Duration::from_millis(10), reload_certs_lock.lock()).await
        {
            Ok(v) => *v,
            Err(_e) => {
                warn!("Timeout waiting for ReloadCert lock");
                false
            }
        };
        if should_reload {
            info!("Reloading TLS certificates");
            match load_ssl() {
                Ok(ssl_config) => {
                    let tls_acceptor: Acceptor = Arc::new(ssl_config).into();
                    tls_listener.replace_acceptor(tls_acceptor);
                    *reload_certs_lock.lock().await = false;
                }
                Err(e) => {
                    error!("Unable to reload TLS certificates: {}", e);
                }
            }
        }

        // Accept the next TLS connection.
        let (tls_stream, _addr) = match tls_listener.next().await {
            Some(Ok(conn)) => conn,
            Some(Err(e)) => {
                error!("Error accepting TLS connection: {:?}", e);
                continue;
            }
            None => break,
        };

        let sdp_injector_context = sdp_injector_context.clone();
        tokio::spawn(async move {
            let io = TokioIo::new(tls_stream);
            let service = service_fn(move |req| injector_handler(req, sdp_injector_context.clone()));
            if let Err(e) = http1::Builder::new().serve_connection(io, service).await {
                error!("Error serving SDP Injector connection: {:?}", e);
            }
        });
    }
    Ok(())
}
