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

    ($logger:ident | $($arg:tt)+) => {
        $logger!("{}", $($arg)+);
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

    ($component:literal | $target:expr $(, $arg:expr)*) => {
        let t = format!($target $(, $arg)*);
        sdp_log!(info | format!("[{}] {}", $component, t));
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
macro_rules! when_ok {
    (($v:ident : $ty:ty = $when:expr) $then:expr) => {
        match $when {
            Err(e) => {
                error!("Error: {}", e);
                None::<$ty>
            }
            Ok($v) => $then,
        }
    };

    (($v:ident = $when:expr) $then:expr) => {
        match $when {
            Err(e) => {
                error!("Error: {}", e);
                None
            }
            Ok($v) => {
                $then;
                Some(())
            }
        }
    };
}

#[macro_export]
macro_rules! deployment {
    ($namespace:literal, $name:literal) => {{
        let mut d = Deployment::default();
        d.metadata.name = Some($name.to_string());
        d.metadata.namespace = Some($namespace.to_string());
        d
    }};
}

#[macro_export]
macro_rules! service_user {
    ($n:expr) => {
        ServiceUser {
            id: format!("{}{}", stringify!(service_user_id), $n),
            name: format!("{}{}", stringify!(service_user), $n),
            password: format!("{}{}", stringify!(password), $n),
            profile_url: format!("{}{}", stringify!(profile_url), $n),
        }
    };
}

#[macro_export]
macro_rules! service_identity {
    ($n:expr) => {{
        let service_name = format!("{}{}", stringify!(srv), $n);
        let service_ns = format!("{}{}", stringify!(ns), $n);
        let mut id: ServiceIdentity = ServiceIdentity::new(
            format!("{}-{}", &service_ns, &service_name).as_str(),
            ServiceIdentitySpec {
                service_user: service_user!($n),
                service_namespace: service_ns.to_string(),
                service_name: service_name.to_string(),
                labels: HashMap::new(),
                disabled: false,
            },
        );
        id.metadata.namespace = Some("sdp-system".to_string());
        id
    }};
}

#[macro_export]
macro_rules! service_device_ids {
    ($n:tt) => {
        DeviceId::new(
            format!("{}{}", stringify!(id), $n).as_str(),
            DeviceIdSpec {
                service_name: format!("{}{}", stringify!(srv), $n),
                service_namespace: format!("{}{}", stringify!(ns), $n),
                uuids: vec![format!(
                    "00000000-0000-0000-0000-0000000000{:0width$}",
                    $n,
                    width = 2
                )],
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
