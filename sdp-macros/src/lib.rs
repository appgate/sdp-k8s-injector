#[macro_export]
macro_rules! queue_debug {
    ($msg:expr => $q:ident) => {
        if cfg!(debug_assertions) {
            if let Some(ref q) = $q {
                if let Err(err) = q.send($msg).await {
                    error!("Error notifying external watcher:{:?} => {}", $msg, err)
                }
            }
        }
    };
}

#[macro_export]
macro_rules! sdp_log {
    ($logger:ident | $protocol:path | ($target:expr $(, $arg:expr)*) => $q:ident) => {
        if cfg!(debug_assertions) {
            let t = format!($target $(, $arg)*);
            queue_debug!($protocol(t.to_string()) => $q);
        }
        $logger!($target $(, $arg)*);
    };
}

#[macro_export]
macro_rules! sdp_info {
    ($protocol:path | ($target:expr $(, $arg:expr)*) => $q:ident) => {
        sdp_log!(info | $protocol | ($target $(, $arg)*) => $q);
    };

    ($protocol:path | $target:expr $(, $arg:expr)*) => {
        sdp_log!(info | $protocol | ($target $(, $arg)*) => None);
    };
}

#[macro_export]
macro_rules! sdp_warn {
    ($protocol:path | ($target:expr $(, $arg:expr)*) => $q:ident) => {
        sdp_log!(warn | $protocol |($target $(, $arg)*) => $q);
    };

    ($protocol:path | $target:expr $(, $arg:expr)*) => {
        sdp_log!(warn | $protocol | ($target $(, $arg)*) => None);
    };
}

#[macro_export]
macro_rules! sdp_debug {
    ($protocol:path | ($target:expr $(, $arg:expr)*) => $q:ident) => {
        sdp_log!(debug | $protocol | ($target $(, $arg)*) => $q);
    };

    ($protocol:path | $target:expr $(, $arg:expr)*) => {
        sdp_log!(debug | $protocol | ($target $(, $arg)*) => None);
    };
}

#[macro_export]
macro_rules! sdp_error {
    ($protocol:path | ($target:expr $(, $arg:expr)*) => $q:ident) => {
        sdp_log!(error | $protocol | ($target $(, $arg)*) => $q);
    };

    ($protocol:path | $target:expr $(, $arg:expr)*) => {
        sdp_log!(error | $protocol | ($target $(, $arg)*) => None);
    };
}

#[macro_export]
macro_rules! deployment {
    ($name:literal, $namespace:literal) => {{
        let mut d = Deployment::default();
        d.metadata.name = Some($name.to_string());
        d.metadata.namespace = Some($namespace.to_string());
        d
    }};
}

#[macro_export]
macro_rules! credentials_ref {
    ($n:expr, $secret:expr) => {
        ServiceCredentialsRef {
            id: format!("{}{}", stringify!(id), $n).to_string(),
            name: format!("{}{}", stringify!(name), $n).to_string(),
            secret: $secret,
            user_field: format!("{}{}", stringify!(user_field), $n).to_string(),
            password_field: format!("{}{}", stringify!(password_field), $n).to_string(),
            client_profile_url: format!("{}{}", stringify!(url_field), $n).to_string(),
        }
    };

    ($n:expr) => {
        credentials_ref!($n, format!("{}{}", stringify!(secret), $n).to_string())
    };
}

#[macro_export]
macro_rules! service_identity {
    ($n:expr, $secret:expr) => {{
        let service_name = format!("{}{}", stringify!(srv), $n);
        let service_ns = format!("{}{}", stringify!(ns), $n);
        let mut id: ServiceIdentity = ServiceIdentity::new(
            format!("{}-{}", &service_ns, &service_name).as_str(),
            ServiceIdentitySpec {
                service_credentials: credentials_ref!($n, $secret),
                service_name: service_name.to_string(),
                service_namespace: service_ns.to_string(),
                labels: HashMap::new(),
                disabled: false,
            },
        );
        id.metadata.namespace = Some("sdp-system".to_string());
        id
    }};

    ($n:expr) => {
        service_identity!($n, format!("{}{}", stringify!(secret), $n).to_string())
    };
}

#[macro_export]
macro_rules! service_device_ids {
    ($n:tt) => {
        DeviceId::new(
            format!("{}{}", stringify!(id), $n).as_str(),
            DeviceIdSpec {
                service_name: format!("{}{}", stringify!(srv), $n),
                service_namespace: format!("{}{}", stringify!(ns), $n),
                uuids: vec![format!("{}{}", stringify!(666), $n)],
            },
        )
    };
}

#[macro_export]
macro_rules! device_id {
    ($n: tt) => {
        DeviceId::new(
            concat!(stringify!(id), $n),
            DeviceIdSpec {
                uuids: vec![],
                service_name: concat!(stringify!(srv), $n).to_string(),
                service_namespace: concat!(stringify!(ns), $n).to_string(),
            },
        )
    };
}
