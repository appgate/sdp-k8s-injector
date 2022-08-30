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
    ($n:expr) => {
        ServiceCredentialsRef {
            id: format!("{}{}", stringify!(id), $n).to_string(),
            name: format!("{}{}", stringify!(name), $n).to_string(),
            secret: format!("{}{}", stringify!(secret), $n).to_string(),
            user_field: format!("{}{}", stringify!(field_field), $n).to_string(),
            password_field: format!("{}{}", stringify!(password_field), $n).to_string(),
        }
    };
}

#[macro_export]
macro_rules! service_identity {
    ($n:tt) => {
        ServiceIdentity::new(
            concat!(stringify!(id), $n),
            ServiceIdentitySpec {
                service_credentials: credentials_ref!($n),
                service_name: concat!(stringify!(srv), $n).to_string(),
                service_namespace: concat!(stringify!(ns), $n).to_string(),
                labels: HashMap::new(),
                disabled: false,
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
                uuids: HashMap::new(),
                service_name: concat!(stringify!(srv), $n).to_string(),
                service_namespace: concat!(stringify!(ns), $n).to_string(),
            },
        )
    };
}
