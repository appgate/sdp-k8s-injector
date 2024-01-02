#[macro_export]
macro_rules! assert_message {
    (($var:ident :: $pattern:pat_param in $queue:ident) => $e:expr) => {
        let value = timeout(Duration::from_millis(1000), $queue.recv())
            .await
            .expect(format!("No messages of type {} found", stringify!($pattern)).as_str());
        assert!(value.is_some(), "Trying to read from a closed channel!");
        let $var = value.unwrap();
        assert!(
            matches!($var, $pattern),
            "Got wrong message {:?}, expected of type {}",
            $var,
            stringify!($pattern)
        );
        $e
    };

    ($var:ident :: $pattern:pat_param in $queue:ident) => {
        assert_message! {
            ($var :: $pattern in $queue) => {}
        }
    };
}

#[macro_export]
macro_rules! assert_no_message {
    ($queue:ident | $timeout:literal) => {
        let value = timeout(Duration::from_millis($timeout), $queue.recv()).await;
        match value {
            Ok(Some(v)) => {
                assert!(false, "Expected not messages in queue, found {:?}", v);
            }
            Ok(None) => {
                assert!(false, "Trying to read from a closed channel!");
            }
            _ => (),
        }
    };

    ($queue:ident) => {
        assert_no_message!($queue | 1000);
    };
}

#[macro_export]
macro_rules! set_pod_field {
    ($pod:expr, containers => $cs:expr) => {
        let test_cs: Vec<Container> = $cs
            .iter()
            .map(|x| {
                let mut c: Container = Default::default();
                c.name = x.to_string();
                c
            })
            .collect();
        if let Some(spec) = $pod.spec.as_mut() {
            spec.containers = test_cs;
        }
    };
    ($pod:expr, init_containers => $cs:expr) => {
        let test_cs: Vec<Container> = $cs
            .iter()
            .map(|x| {
                let mut c: Container = Default::default();
                c.name = x.to_string();
                c
            })
            .collect();
        if let Some(spec) = $pod.spec.as_mut() {
            spec.init_containers = Some(test_cs);
        }
    };
    ($pod:expr, volumes => $vs:expr) => {
        let volumes: Vec<Volume> = $vs
            .iter()
            .map(|x| {
                let mut v: Volume = Default::default();
                v.name = x.to_string();
                v
            })
            .collect();

        if volumes.len() > 0 {
            if let Some(spec) = $pod.spec.as_mut() {
                spec.volumes = Some(volumes);
            }
        }
    };
    ($pod:expr, annotations => $cs:expr) => {
        let mut bm = BTreeMap::new();
        for (k, v) in $cs {
            bm.insert(k.to_string(), v.to_string());
        }
        $pod.metadata.annotations = Some(bm);
    };
    ($pod:expr, image_pull_secrets => $ps:expr) => {
        if let Some(spec) = $pod.spec.as_mut() {
            let ss = $ps
                .iter()
                .map(|s| LocalObjectReference {
                    name: Some(format!("{}", s)),
                })
                .collect();
            spec.image_pull_secrets = Some(ss);
        }
    };
    ($pod:expr, sysctls => $ss:expr) => {
        if let Some(spec) = $pod.spec.as_mut() {
            if let Some(sc) = spec.security_context.as_mut() {
                sc.sysctls = Some($ss);
            } else {
                let mut sc = PodSecurityContext::default();
                sc.sysctls = Some($ss);
                spec.security_context = Some(sc);
            }
        }
    };
}

#[macro_export]
macro_rules! pod {
    ($n:tt) => {{
        let mut pod: Pod = Default::default();
        pod.spec = Some(Default::default());
        pod!($n, pod)
    }};
    ($n:tt, $pod:expr) => {{
        $pod.metadata.namespace = Some(format!("{}{}", stringify!(ns), $n));
        $pod.metadata.name = Some(format!("srv{}-replica{}-testpod{}", stringify!($n), stringify!($n), stringify!($n)));
        $pod
    }};
    ($n:tt, $($fs:ident => $es:expr),*) => {{
        let mut pod: Pod = Default::default();
        pod.spec = Some(Default::default());
        pod!($n, pod, $($fs => $es),*)
    }};
    ($n:tt, $pod:expr,$f:ident => $e:expr) => {{
        set_pod_field!($pod, $f => $e);
        pod!($n, $pod)
    }};
    ($n:tt, $pod:expr,$f:ident => $e:expr,$($fs:ident => $es:expr),*) => {{
        set_pod_field!($pod, $f => $e);
        pod!($n, $pod,$($fs => $es),*)
    }};
}

#[macro_export]
macro_rules! replicaset {
    ($n:tt, $owner_references:expr) => {{
        let mut rs = replicaset!($n, 0, $owner_references);
        rs.spec = None;
        rs
    }};
    ($n:tt, $replicas:expr, $owner_references:expr) => {{
        let mut rs = ReplicaSet::default();
        let mut spec = ReplicaSetSpec::default();
        spec.replicas = $replicas;
        rs.spec = Some(spec);
        rs.metadata.namespace = Some(format!("{}{}", stringify!(ns), $n));
        rs.metadata.name = Some(format!(
            "srv{}-replica{}-testpod{}",
            stringify!($n),
            stringify!($n),
            stringify!($n)
        ));
        rs.metadata.owner_references = $owner_references;
        rs
    }};
}

#[macro_export]
macro_rules! owner_reference {
    ($name:expr) => {{
        let mut or = OwnerReference::default();
        or.name = format!("{}", $name);
        or
    }};
}
