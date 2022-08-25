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
