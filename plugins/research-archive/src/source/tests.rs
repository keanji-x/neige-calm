use super::*;

#[test]
fn rps_is_bounded() {
    assert!(Rps::new(0).is_none());
    assert!(Rps::new(21).is_none());
    assert_eq!(Rps::new(2).unwrap().get(), 2);
    assert_eq!(Rps::new(20).unwrap().get(), 20);
}

#[tokio::test(start_paused = true)]
async fn limiter_spaces_starts_by_the_interval() {
    let limiter = RateLimiter::new(Rps::new(5).unwrap());
    let start = tokio::time::Instant::now();
    for _ in 0..6 {
        limiter.acquire().await;
    }
    // Six starts at 0, 200, ..., 1000 ms.
    assert_eq!(start.elapsed(), Duration::from_millis(1000));
}

/// Concurrent callers are released one interval apart, in any order.
#[tokio::test(start_paused = true)]
async fn concurrent_callers_are_spaced_by_the_interval() {
    let limiter = std::sync::Arc::new(RateLimiter::new(Rps::new(4).unwrap()));
    let start = tokio::time::Instant::now();
    let mut tasks = tokio::task::JoinSet::new();
    for _ in 0..8 {
        let limiter = limiter.clone();
        tasks.spawn(async move {
            limiter.acquire().await;
            start.elapsed()
        });
    }
    let mut released: Vec<_> = tasks.join_all().await;
    released.sort();
    let expected: Vec<_> = (0..8).map(|i| Duration::from_millis(250 * i)).collect();
    assert_eq!(released, expected);
}

#[test]
fn retry_after_is_parsed_from_the_throttle_message() {
    let ms = Duration::from_millis;
    for (message, wait) in [
        (
            "rate limit of 1-second interval has been reached, please retry after: 0.2s",
            ms(200),
        ),
        ("please Retry After: 0.8s", ms(800)),
        ("retry after:3s", Duration::from_secs(3)),
        ("retry after: 60s", Duration::from_secs(60)),
        // A longer wait is clamped to the maximum.
        ("retry after: 61s", RETRY_AFTER_MAX),
        ("retry after: 3600.5s", RETRY_AFTER_MAX),
        // No wait, no unit, zero, or garbage: the fallback.
        ("rate limit reached", RETRY_AFTER_FALLBACK),
        ("retry after: 0.5", RETRY_AFTER_FALLBACK),
        ("retry after: 0s", RETRY_AFTER_FALLBACK),
        ("retry after: 1.2.3s", RETRY_AFTER_FALLBACK),
        ("", RETRY_AFTER_FALLBACK),
    ] {
        assert_eq!(parse_retry_after(message), wait, "{message:?}");
    }
}

#[test]
fn non_envelope_statuses_map_onto_the_stored_classes() {
    assert_eq!(
        class_for_status(429),
        (FailureClass::Throttled, ErrorClass::HttpStatus)
    );
    for s in [401, 403] {
        assert_eq!(
            class_for_status(s),
            (FailureClass::AuthUnavailable, ErrorClass::HttpStatus)
        );
    }
    for s in [408, 500, 502, 503, 599] {
        assert_eq!(
            class_for_status(s),
            (FailureClass::Transient, ErrorClass::HttpStatus)
        );
    }
    for s in [400, 404, 410, 499] {
        assert_eq!(
            class_for_status(s),
            (FailureClass::Permanent, ErrorClass::HttpStatus)
        );
    }
    for s in [201, 204, 302] {
        assert_eq!(
            class_for_status(s),
            (FailureClass::Transient, ErrorClass::UnexpectedResponse)
        );
    }
}

#[test]
fn production_bounds_are_finite_and_ordered() {
    let t = Timeouts::PRODUCTION;
    assert!(t.token > Duration::ZERO && t.token <= Duration::from_secs(60));
    // Above the SDK's own 30 s request timeout.
    assert!(t.fetch > Duration::from_secs(30) && t.fetch <= Duration::from_secs(120));
}
