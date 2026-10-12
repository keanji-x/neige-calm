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

/// B3: callers that queue behind a slow token resolution are paced after
/// it, not released together. The production `fetch` runs against a local
/// server that records when each request arrives. The token step is the
/// one substitution: a successful refresh cannot be faked without the real
/// OAuth host's TLS certificate, so the resolver models the `OAuth` lock a
/// refresh holds (`oauth/src/client.rs:93-112`): every call takes one shared
/// lock, and the first holds it for a second. The HTTP client's own token
/// is `OAuth::from_token`, so the SDK's resolution inside `send()` is
/// immediate, as it is after a real refresh.
#[tokio::test]
async fn fetches_queued_behind_a_token_refresh_are_paced() {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::Instant;

    let arrivals = Arc::new(Mutex::new(Vec::<Instant>::new()));
    let log = arrivals.clone();
    let app = axum::Router::new().fallback(move || {
        let log = log.clone();
        async move {
            log.lock().unwrap().push(Instant::now());
            r#"{"code":0,"message":"","data":{}}"#
        }
    });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

    let lock = Arc::new(tokio::sync::Mutex::new(()));
    let first = Arc::new(AtomicBool::new(true));
    let resolver: ResolveToken = Arc::new(move || {
        let (lock, first) = (lock.clone(), first.clone());
        Box::pin(async move {
            let _held = lock.lock().await;
            if first.swap(false, Ordering::SeqCst) {
                tokio::time::sleep(Duration::from_secs(1)).await;
            }
            Ok("ap_fresh".to_owned())
        })
    });
    let rps = Rps::new(4).unwrap();
    crate::dotenv_reach::assert_no_dotenv_reach();
    let source = LongbridgeSource::with_resolver(
        OAuth::from_token("ap_fresh"),
        &url,
        rps,
        Timeouts::PRODUCTION,
        resolver,
    );
    let endpoint = *crate::catalog::Catalog::compiled().get("ratings").unwrap();
    let key = Key::new("AAPL.US").unwrap();
    let (a, b, c, d) = tokio::join!(
        source.fetch(&endpoint, &key),
        source.fetch(&endpoint, &key),
        source.fetch(&endpoint, &key),
        source.fetch(&endpoint, &key),
    );
    for fetched in [a, b, c, d] {
        assert_eq!(fetched.response(), &Response::Body(b"{}".to_vec()));
    }
    let mut times = arrivals.lock().unwrap().clone();
    times.sort();
    assert_eq!(times.len(), 4);
    let gaps: Vec<Duration> = times.windows(2).map(|w| w[1] - w[0]).collect();
    // The interval is 250 ms; 50 ms absorbs scheduling jitter. Released
    // together, the gaps would be about 0.
    for gap in &gaps {
        assert!(*gap >= Duration::from_millis(200), "{gaps:?}");
    }
}

#[test]
fn production_bounds_are_finite_and_ordered() {
    let t = Timeouts::PRODUCTION;
    assert!(t.token > Duration::ZERO && t.token <= Duration::from_secs(60));
    // Above the SDK's own 30 s request timeout.
    assert!(t.fetch > Duration::from_secs(30) && t.fetch <= Duration::from_secs(120));
}
