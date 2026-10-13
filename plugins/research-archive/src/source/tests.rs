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
            Some(ms(200)),
        ),
        ("please Retry After: 0.8s", Some(ms(800))),
        ("retry after:3s", Some(Duration::from_secs(3))),
        ("retry after: 60s", Some(Duration::from_secs(60))),
        // A longer wait is clamped to the maximum.
        ("retry after: 61s", Some(RETRY_AFTER_MAX)),
        ("retry after: 3600.5s", Some(RETRY_AFTER_MAX)),
        // No wait, no unit, zero, or garbage: none named.
        ("rate limit reached", None),
        ("retry after: 0.5", None),
        ("retry after: 0s", None),
        ("retry after: 1.2.3s", None),
        ("", None),
    ] {
        assert_eq!(parse_retry_after(message), wait, "{message:?}");
    }
}

/// The whole 429xxx family is a throttle. A named wait wins; otherwise the
/// 1-second interval limit (429002) waits 1 s and any other member, whose
/// interval is unknown, the longest wait a server may name.
#[test]
fn every_429xxx_envelope_code_is_a_throttle() {
    let named = "please retry after: 0.4s";
    let unnamed = "api request is limited";
    for (code, message, wait) in [
        (429_002, named, Some(Duration::from_millis(400))),
        (429_002, unnamed, Some(RETRY_AFTER_FALLBACK)),
        (429_003, named, Some(Duration::from_millis(400))),
        (429_003, unnamed, Some(RETRY_AFTER_MAX)),
        (429_003, "retry after: 120s", Some(RETRY_AFTER_MAX)),
        (429_000, unnamed, Some(RETRY_AFTER_MAX)),
        (429_001, unnamed, Some(RETRY_AFTER_MAX)),
        (429_999, named, Some(Duration::from_millis(400))),
        // Outside the family: no throttle, whatever the message says.
        (428_999, named, None),
        (430_000, named, None),
        (429, named, None),
        (2_601_400, unnamed, None),
        (0, unnamed, None),
        (-429_002, named, None),
    ] {
        assert_eq!(throttle_wait(code, message), wait, "{code} {message:?}");
    }
}

/// `classify` stores a 429xxx envelope as `throttled` with its code and
/// hands back its wait; any other envelope code stays `permanent`.
#[test]
fn envelope_throttles_are_classified_by_the_family() {
    let envelope = |code: i32| HttpClientError::OpenApi {
        code,
        message: "api request is limited".to_owned(),
        trace_id: String::new(),
    };
    for (code, class, wait) in [
        (429_002, FailureClass::Throttled, Some(RETRY_AFTER_FALLBACK)),
        (429_003, FailureClass::Throttled, Some(RETRY_AFTER_MAX)),
        (429_001, FailureClass::Throttled, Some(RETRY_AFTER_MAX)),
        (428_999, FailureClass::Permanent, None),
        (430_000, FailureClass::Permanent, None),
        (500_100, FailureClass::Permanent, None),
    ] {
        let fetched = classify(envelope(code));
        assert_eq!(
            fetched.response(),
            &failed(class, ErrorClass::Envelope, code.to_string()),
            "{code}"
        );
        assert_eq!(fetched.retry_after(), wait, "{code}");
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
