//! A token refresh that never answers cannot hang `build_oauth` or
//! `LongbridgeSource::fetch`, and a timed-out refresh releases the `OAuth`
//! lock.

use std::time::{Duration, Instant};

use longbridge::oauth::OAuth;
use research_archive::auth::{self, ClientId, DataDirTokenStorage, OAuthUnavailable};
use research_archive::catalog::Catalog;
use research_archive::source::{Fetched, LongbridgeSource, Rps, Timeouts};
use research_archive::types::{ErrorClass, ErrorDetail, FailureClass, Key, Response};

use crate::hermetic;

pub const CLIENT: &str = "test-client";
pub const DEADLINE: Duration = Duration::from_secs(2);
pub const TIMEOUTS: Timeouts = Timeouts {
    token: DEADLINE,
    fetch: DEADLINE,
};

pub fn write_token(storage: &DataDirTokenStorage, expires_at: u64) {
    let token: auth::TokenFile = serde_json::from_value(serde_json::json!({
        "client_id": CLIENT,
        "access_token": "ap_m_FABRICATED-ACCESS",
        "refresh_token": "FABRICATED-REFRESH",
        "expires_at": expires_at,
    }))
    .unwrap();
    storage.save_checked(&token).unwrap();
}

pub fn current_thread() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
}

/// An `OAuth` whose token loads without a refresh and, by the time this
/// returns, is inside the SDK's five-minute margin.
pub fn token_entering_the_margin(
    runtime: &tokio::runtime::Runtime,
    storage: &DataDirTokenStorage,
) -> OAuth {
    write_token(storage, auth::unix_now() + 302);
    let oauth = runtime
        .block_on(auth::build_oauth(
            &ClientId::parse(CLIENT).unwrap(),
            storage.clone(),
            DEADLINE,
        ))
        .expect("the token is outside the margin at build");
    std::thread::sleep(Duration::from_secs(3));
    oauth
}

pub fn fetch_once(runtime: &tokio::runtime::Runtime, source: &LongbridgeSource) -> Fetched {
    let ratings = *Catalog::compiled().get("ratings").unwrap();
    runtime.block_on(source.fetch(&ratings, &Key::new("AAPL.US").unwrap()))
}

pub fn refresh_timed_out() -> Response {
    Response::Failed {
        class: FailureClass::AuthUnavailable,
        error: ErrorDetail {
            class: ErrorClass::Auth,
            code: "refresh_timeout".into(),
        },
    }
}

#[test]
fn a_hanging_token_refresh_is_bounded() {
    let (_serial, proxy) = hermetic::enter(true);
    let runtime = current_thread();
    let root = tempfile::tempdir().unwrap();
    let storage = DataDirTokenStorage::open(root.path().join("oauth")).unwrap();
    let client = ClientId::parse(CLIENT).unwrap();

    // 1. A token inside the margin: the build refreshes, and the deadline
    //    ends the wait.
    write_token(&storage, auth::unix_now() + 10);
    let start = Instant::now();
    let built = runtime.block_on(auth::build_oauth(&client, storage.clone(), DEADLINE));
    assert_eq!(built.err(), Some(OAuthUnavailable::TimedOut));
    assert!(start.elapsed() >= DEADLINE);
    assert_eq!(proxy.oauth_requests(), 1);

    // 2. A token that loads without a refresh, then enters the margin. The
    //    request target is never reached: token resolution comes first, and
    //    a hung one is an auth pause, not a request failure.
    let oauth = token_entering_the_margin(&runtime, &storage);
    assert_eq!(proxy.oauth_requests(), 1);
    let source =
        LongbridgeSource::new(oauth, "http://127.0.0.1:1", Rps::new(20).unwrap(), TIMEOUTS);
    for expected_requests in [2, 3] {
        let start = Instant::now();
        let fetched = fetch_once(&runtime, &source);
        assert_eq!(fetched.response(), &refresh_timed_out());
        assert!(start.elapsed() >= DEADLINE);
        // A new refresh per fetch: the timed-out one released the `OAuth`
        // lock when its future was dropped.
        assert_eq!(proxy.oauth_requests(), expected_requests);
    }
}
