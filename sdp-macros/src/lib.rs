#[macro_export]
macro_rules! queue_info {
    ($msg:expr => $q:ident) => {
        if let Some(ref q) = $q {
            if let Err(err) = q.send($msg).await {
                log::error!("Error notifying external watcher:{:?} => {}", $msg, err)
            }
        }
    };
}

// Generates log methods for modules
#[macro_export]
macro_rules! logger {
    ($module:literal) => {
        with_dollar_sign! {
            ($d:tt) => {
                #[allow(unused_macros)]
                macro_rules! info {
                    ($d target:expr $d(, $d arg:expr)*) => {
                        sdp_info!($module | ($d target $d(, $d arg)*))
                    };

                    ($d protocol:path | ($d target:expr $d(, $d arg:expr)*) => $d q:ident) => {
                        sdp_info!($module | $d protocol | ($d target $d(, $d arg)*) => $d q)
                    };
                }

                #[allow(unused_macros)]
                macro_rules! debug {
                    ($d target:expr $d(, $d arg:expr)*) => {
                        sdp_debug!($module | ($d target $d(, $d arg)*))
                    };

                    ($d protocol:path | ($d target:expr $d(, $d arg:expr)*) => $d q:ident) => {
                        sdp_debug!($module | $d protocol | ($d target $d(, $d arg)*) => $d q)
                    };
                }

                #[allow(unused_macros)]
                macro_rules! warn {
                    ($d target:expr $d(, $d arg:expr)*) => {
                        sdp_warn!($module | ($d target $d(, $d arg)*))
                    };

                    ($d protocol:path | ($d target:expr $d(, $d arg:expr)*) => $d q:ident) => {
                        sdp_warn!($module | $d protocol | ($d target $d(, $d arg)*) => $d q)
                    };
                }

                #[allow(unused_macros)]
                macro_rules! error {
                    ($d target:expr $d(, $d arg:expr)*) => {
                        sdp_error!($module | ($d target $d(, $d arg)*))
                    };

                    ($d protocol:path | ($d target:expr $d(, $d arg:expr)*) => $d q:ident) => {
                        sdp_error!($module | $d protocol | ($d target $d(, $d arg)*) => $d q)
                    };
                }
            }
        }
    };

    ($module:ident) => {
        with_dollar_sign! {
            ($d:tt) => {
                #[allow(unused_macros)]
                macro_rules! info {
                    ($d target:expr $d(, $d arg:expr)*) => {
                        sdp_info!($module | ($d target $d(, $d arg)*))
                    };

                    ($d protocol:path | ($d target:expr $d(, $d arg:expr)*) => $d q:ident) => {
                        sdp_info!($module | $d protocol | ($d target $d(, $d arg)*) => $d q)
                    };
                }

                #[allow(unused_macros)]
                macro_rules! debug {
                    ($d target:expr $d(, $d arg:expr)*) => {
                        sdp_debug!($module | ($d target $d(, $d arg)*))
                    };

                    ($d protocol:path | ($d target:expr $d(, $d arg:expr)*) => $d q:ident) => {
                        sdp_debug!($module | $d protocol | ($d target $d(, $d arg)*) => $d q)
                    };
                }

                #[allow(unused_macros)]
                macro_rules! warn {
                    ($d target:expr $d(, $d arg:expr)*) => {
                        sdp_warn!($module | ($d target $d(, $d arg)*))
                    };

                    ($d protocol:path | ($d target:expr $d(, $d arg:expr)*) => $d q:ident) => {
                        sdp_warn!($module | $d protocol | ($d target $d(, $d arg)*) => $d q)
                    };
                }

                #[allow(unused_macros)]
                macro_rules! error {
                    ($d target:expr $d(, $d arg:expr)*) => {
                        sdp_error!($module | ($d target $d(, $d arg)*))
                    };

                    ($d protocol:path | ($d target:expr $d(, $d arg:expr)*) => $d q:ident) => {
                        sdp_error!($module | $d protocol | ($d target $d(, $d arg)*) => $d q)
                    };
                }
            }
        }
    };
}

#[macro_export]
macro_rules! with_dollar_sign {
    ($($body:tt)*) => {
        macro_rules! __with_dollar_sign { $($body)* }
        __with_dollar_sign!($);
    }
}

#[macro_export]
macro_rules! sdp_log {
    ($logger:ident | $protocol:path | $module:literal | ($target:expr $(, $arg:expr)*) => $q:ident) => {
        let t = format!($target $(, $arg)*);
        queue_info!($protocol(t.to_string()) => $q);
        log::$logger!("[{}] {}", $module, t);
    };

    ($logger:ident | $protocol:path | ($target:expr $(, $arg:expr)*) => $q:ident) => {
        let t = format!($target $(, $arg)*);
        queue_info!($protocol(t.to_string()) => $q);
        log::$logger!($target $(, $arg)*);
    };

    ($logger:ident | $module:literal | ($target:expr $(, $arg:expr)*)) => {
        log::$logger!("[{}] {}", $module, $target $(, $arg)*);
    };

    ($logger:ident | ($target:expr $(, $arg:expr)*)) => {
        log::$logger!($target $(, $arg)*);
    };
}

#[macro_export]
macro_rules! sdp_info {
    ($module:literal | $protocol:path | ($target:expr $(, $arg:expr)*) => $q:ident) => {
        sdp_log!(info | $protocol | $module | ($target $(, $arg)*) => $q);
    };

    ($module:ident | $protocol:path | ($target:expr $(, $arg:expr)*) => $q:ident) => {
        sdp_log!(info | $protocol | $module | ($target $(, $arg)*) => $q);
    };

    ($module:literal | $protocol:path | ($target:expr $(, $arg:expr)*)) => {
        sdp_log!(info | $protocol | $module | ($target $(, $arg)*) => None);
    };

    ($module:ident | $protocol:path | ($target:expr $(, $arg:expr)*)) => {
        sdp_log!(info | $protocol | $module | ($target $(, $arg)*) => None);
    };

    ($module:literal | ($target:expr $(, $arg:expr)*)) => {
        let t = format!($target $(, $arg)*);
        sdp_log!(info | ("[{}] {}", $module, t));
    };

    ($module:ident | ($target:expr $(, $arg:expr)*)) => {
        let t = format!($target $(, $arg)*);
        sdp_log!(info | ("[{}] {}", $module, t));
    };
}

#[macro_export]
macro_rules! sdp_warn {
    ($module:literal | $protocol:path | ($target:expr $(, $arg:expr)*) => $q:ident) => {
        sdp_log!(warn | $protocol | $module | ($target $(, $arg)*) => $q);
    };

    ($module:ident | $protocol:path | ($target:expr $(, $arg:expr)*) => $q:ident) => {
        sdp_log!(warn | $protocol | $module | ($target $(, $arg)*) => $q);
    };

    ($module:literal | $protocol:path | ($target:expr $(, $arg:expr)*)) => {
        sdp_log!(warn | $protocol | $module | ($target $(, $arg)*) => None);
    };

    ($module:ident | $protocol:path | ($target:expr $(, $arg:expr)*)) => {
        sdp_log!(warn | $protocol | $module | ($target $(, $arg)*) => None);
    };

    ($module:literal | ($target:expr $(, $arg:expr)*)) => {
        let t = format!($target $(, $arg)*);
        sdp_log!(warn | ("[{}] {}", $module, t));
    };

    ($module:ident | ($target:expr $(, $arg:expr)*)) => {
        let t = format!($target $(, $arg)*);
        sdp_log!(warn | ("[{}] {}", $module, t));
    };
}

#[macro_export]
macro_rules! sdp_debug {
    ($module:literal | $protocol:path | ($target:expr $(, $arg:expr)*) => $q:ident) => {
        sdp_log!(debug | $protocol | $module | ($target $(, $arg)*) => $q);
    };

    ($module:ident | $protocol:path | ($target:expr $(, $arg:expr)*) => $q:ident) => {
        sdp_log!(debug | $protocol | $module | ($target $(, $arg)*) => $q);
    };

    ($module:literal | $protocol:path | ($target:expr $(, $arg:expr)*)) => {
        sdp_log!(debug | $protocol | $module | ($target $(, $arg)*) => None);
    };

    ($module:ident | $protocol:path | ($target:expr $(, $arg:expr)*)) => {
        sdp_log!(debug | $protocol | $module | ($target $(, $arg)*) => None);
    };

    ($module:literal | ($target:expr $(, $arg:expr)*)) => {
        let t = format!($target $(, $arg)*);
        sdp_log!(debug | ("[{}] {}", $module, t));
    };

    ($module:ident | ($target:expr $(, $arg:expr)*)) => {
        let t = format!($target $(, $arg)*);
        sdp_log!(debug | ("[{}] {}", $module, t));
    };
}

#[macro_export]
macro_rules! sdp_error {
    ($module:literal | $protocol:path | ($target:expr $(, $arg:expr)*) => $q:ident) => {
        sdp_log!(error | $protocol | $module | ($target $(, $arg)*) => $q);
    };

    ($module:ident | $protocol:path | ($target:expr $(, $arg:expr)*) => $q:ident) => {
        sdp_log!(error | $protocol | $module | ($target $(, $arg)*) => $q);
    };

    ($module:literal | $protocol:path | ($target:expr $(, $arg:expr)*)) => {
        sdp_log!(error | $protocol | $module | ($target $(, $arg)*) => None);
    };

    ($module:ident | $protocol:path | ($target:expr $(, $arg:expr)*)) => {
        sdp_log!(error | $protocol | $module | ($target $(, $arg)*) => None);
    };

    ($module:literal | ($target:expr $(, $arg:expr)*)) => {
        let t = format!($target $(, $arg)*);
        sdp_log!(error | ("[{}] {}", $module, t));
    };

    ($module:ident | ($target:expr $(, $arg:expr)*)) => {
        let t = format!($target $(, $arg)*);
        sdp_log!(error | ("[{}] {}", $module, t));
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
macro_rules! sdp_service {
    ($namespace:literal, $name:literal, $kind:literal) => {{
        let mut spec = SDPServiceSpec {
            name: $name.to_string(),
            kind: $kind.to_string(),
        };
        let mut d = SDPService::new($name, spec);
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
        device_id!($n, vec![])
    };
    ($n: tt, $uuids:expr) => {
        DeviceId::new(
            concat!(stringify!(id), $n),
            DeviceIdSpec {
                uuids: $uuids,
                service_name: concat!(stringify!(srv), $n).to_string(),
                service_namespace: concat!(stringify!(ns), $n).to_string(),
            },
        )
    };
}

#[macro_export]
macro_rules! service_candidate_protocol {
    ($t:ident) => {
        impl ServiceCandidateProtocol<$t> for $t {
            fn service<'a>(&'a self) -> &'a $t {
                &self
            }
        }

        impl SimpleWatchingProtocol<IdentityManagerProtocol<$t, ServiceIdentity>> for $t {
            fn initialized(
                &self,
                ns: Option<Namespace>,
            ) -> Option<IdentityManagerProtocol<$t, ServiceIdentity>> {
                ServiceCandidateProtocol::initialized(self, ns)
            }

            fn applied(
                &self,
                ns: Option<Namespace>,
            ) -> Option<IdentityManagerProtocol<$t, ServiceIdentity>> {
                ServiceCandidateProtocol::applied(self, ns)
            }

            fn reapplied(
                &self,
                ns: Option<Namespace>,
            ) -> Option<IdentityManagerProtocol<$t, ServiceIdentity>> {
                ServiceCandidateProtocol::reapplied(self, ns)
            }

            fn deleted(
                &self,
                ns: Option<Namespace>,
            ) -> Option<IdentityManagerProtocol<$t, ServiceIdentity>> {
                ServiceCandidateProtocol::deleted(self, ns)
            }

            fn key(&self) -> Option<String> {
                ServiceCandidateProtocol::key(self)
            }
        }
    };
}
