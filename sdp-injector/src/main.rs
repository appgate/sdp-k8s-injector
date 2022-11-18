use crate::deviceid::{DeviceIdProvider, DeviceIdProviderRequestProtocol};
use crate::injector::{
    injector_handler, load_sidecar_containers, load_ssl, KubeIdentityStore, SDPInjectorContext,
    SDPSidecars,
};
use futures_util::stream::StreamExt;
use http::Uri;
use hyper::server::accept;
use hyper::server::conn::AddrIncoming;
use hyper::service::{make_service_fn, service_fn};
use hyper::Server;
use k8s_openapi::api::core::v1::Pod;
use kube::{Api, Client, Config};
use sdp_common::crd::{DeviceId, ServiceIdentity};
use sdp_common::kubernetes::{KUBE_SYSTEM_NAMESPACE, SDP_K8S_NAMESPACE};
use sdp_common::watcher::{watch, Watcher};
use sdp_macros::{logger, sdp_debug, sdp_error, sdp_info, sdp_log, with_dollar_sign};
use std::collections::HashMap;
use std::convert::Infallible;
use std::error::Error;
use std::future::ready;
use std::sync::Arc;
use tls_listener::TlsListener;
use tokio::sync::mpsc::channel;
use tokio::sync::Mutex;

const SDP_K8S_HOST_ENV: &str = "SDP_K8S_HOST";
const SDP_K8S_HOST_DEFAULT: &str = "kubernetes.default.svc";
const SDP_K8S_NO_VERIFY_ENV: &str = "SDP_K8S_NO_VERIFY";

mod device_id_watcher;
mod deviceid;
mod errors;
mod injector;
mod pod_watcher;
mod service_identity_watcher;

pub type Acceptor = tokio_rustls::TlsAcceptor;

logger!("Main");

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    debug!("Initializing logger");
    log4rs::init_file("/opt/sdp-injector/log4rs.yaml", Default::default()).unwrap();

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
    let pods_api: Api<Pod> = Api::all(k8s_client.clone());
    let device_ids_api: Api<DeviceId> = Api::namespaced(k8s_client.clone(), SDP_K8S_NAMESPACE);
    let (device_id_tx, device_id_rx) =
        channel::<DeviceIdProviderRequestProtocol<ServiceIdentity>>(50);
    let store = KubeIdentityStore {
        device_id_q_tx: device_id_tx.clone(),
    };
    let sdp_sidecars: SDPSidecars =
        load_sidecar_containers().expect("Unable to load the sidecar context");
    let sdp_injector_context = Arc::new(SDPInjectorContext {
        sdp_sidecars: Arc::new(sdp_sidecars),
        ns_api: Api::all(k8s_client.clone()),
        services_api: Api::namespaced(k8s_client, KUBE_SYSTEM_NAMESPACE),
        identity_store: Mutex::new(store),
        attempts_store: Mutex::new(HashMap::new()),
    });

    let ssl_config = load_ssl()?;
    let addr = ([0, 0, 0, 0], 8443).into();
    let tls_acceptor: Acceptor = Arc::new(ssl_config).into();
    let make_service = {
        make_service_fn(move |_conn| {
            let sdp_injector_context = sdp_injector_context.clone();
            async move {
                let sdp_injector_context = sdp_injector_context.clone();
                Ok::<_, Infallible>(service_fn(move |req| {
                    injector_handler(req, sdp_injector_context.clone())
                }))
            }
        })
    };
    let incoming = TlsListener::new(tls_acceptor, AddrIncoming::bind(&addr)?).filter(|c| {
        if let Err(e) = c {
            error!("Error running SDP Injector server: {:?}", e);
            ready(false)
        } else {
            ready(true)
        }
    });

    // Thread to watch ServiceIdentity entities
    // We register new ServiceIdentity entities in the store when created and de unregister them when deleted.
    let (watcher_tx, watcher_rx) = channel::<DeviceIdProviderRequestProtocol<ServiceIdentity>>(50);
    let watcher_tx2 = watcher_tx.clone();
    let watcher_tx3 = watcher_tx.clone();
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

    // Thread to watch DeviceId entities
    // We register new DeviceId entities in the store when created and de unregister them when deleted.
    tokio::spawn(async move {
        let watcher = Watcher {
            api_ns: None,
            api: device_ids_api,
            queue_tx: watcher_tx2,
            notification_message: None,
        };
        let w = watch::<
            DeviceId,
            DeviceIdProviderRequestProtocol<ServiceIdentity>,
            DeviceIdProviderRequestProtocol<ServiceIdentity>,
        >(watcher, None);
        if let Err(e) = w.await {
            panic!("Unable to start DeviceID Watcher: {}", e);
        }
    });

    // Thread to watch Pod entities
    // When a Pod that is a candidate has been deleted, we just return back the device id
    // it was using to the device ids provider.
    tokio::spawn(async move {
        let watcher = Watcher {
            api_ns: None,
            api: pods_api,
            queue_tx: watcher_tx3,
            notification_message: None,
        };
        let w = watch::<
            Pod,
            DeviceIdProviderRequestProtocol<ServiceIdentity>,
            DeviceIdProviderRequestProtocol<ServiceIdentity>,
        >(watcher, None);
        if let Err(e) = w.await {
            panic!("Unable to start Pod Watcher: {}", e);
        }
    });

    // Spawn the main Store
    tokio::spawn(async move {
        let mut device_id_provider = DeviceIdProvider::new(None);
        device_id_provider.run(device_id_rx, watcher_rx).await;
    });

    info!("Starting SDP Injector server");
    let server = Server::builder(accept::from_stream(incoming)).serve(make_service);
    server.await?;
    Ok(())
}
