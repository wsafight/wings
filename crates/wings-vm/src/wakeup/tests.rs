use std::io;
use std::time::Duration;

use super::wait_with_timeout;

#[test]
fn wait_helper_retries_interruptions_and_early_timeouts() {
    let mut calls = 0;
    assert!(
        wait_with_timeout(Duration::from_secs(1), |_| {
            calls += 1;
            match calls {
                1 => Err(io::Error::from(io::ErrorKind::Interrupted)),
                2 => Ok(false),
                _ => Ok(true),
            }
        })
        .unwrap()
    );
    assert_eq!(calls, 3);
}

#[test]
fn wait_helper_zero_timeout_still_polls_and_propagates_errors() {
    let mut calls = 0;
    assert!(
        !wait_with_timeout(Duration::ZERO, |_| {
            calls += 1;
            Ok(false)
        })
        .unwrap()
    );
    assert_eq!(calls, 1);
    assert!(wait_with_timeout(Duration::ZERO, |_| Ok(true)).unwrap());
    let error = wait_with_timeout(Duration::from_secs(1), |_| {
        Err(io::Error::from(io::ErrorKind::PermissionDenied))
    })
    .unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
    let error = wait_with_timeout(Duration::MAX, |_| {
        panic!("overflow must reject before polling")
    })
    .unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
}
