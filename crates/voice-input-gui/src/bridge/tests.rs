use std::time::Duration;

use super::{Backoff, is_outdated};

#[test]
fn reconnect_backoff_doubles_to_a_cap_and_resets_after_hello() {
    let mut backoff = Backoff::default();
    let intervals = std::iter::from_fn(|| Some(backoff.next()))
        .take(5)
        .collect::<Vec<_>>();
    assert_eq!(
        intervals,
        [
            Duration::from_millis(500),
            Duration::from_secs(1),
            Duration::from_secs(2),
            Duration::from_secs(4),
            Duration::from_secs(5),
        ]
    );
    assert_eq!(backoff.next(), Duration::from_secs(5));
    backoff = Backoff::default();
    assert_eq!(backoff.next(), Duration::from_millis(500));
}

#[test]
fn outdated_errors_are_limited_to_protocol_rejections() {
    assert!(is_outdated("invalid request: unknown variant `subscribe`"));
    assert!(is_outdated("unknown variant `toggle`"));
    assert!(!is_outdated("too many subscribers"));
    assert!(!is_outdated("not recording"));
}
