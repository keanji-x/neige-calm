//! A token refresh that never answers cannot hang `build_oauth` or
//! `LongbridgeSource::fetch`.
//!
//! Its own test binary, because it changes the process environment: every
//! proxy variable points at a local listener that records each request line
//! and never answers, `NO_PROXY` is removed, and `LONGBRIDGE_REGION` is set
//! so the SDK skips its geo probe. Both reqwest versions in the graph read
//! those variables by default (reqwest 0.12 for the SDK client,
//! `async_impl/client.rs:419-421`; reqwest 0.11 for oauth2's refresh client,
//! `async_impl/client.rs:255`, `proxy.rs:897-918`), so the refresh's
//! `CONNECT openapi.longbridge.com:443` lands on the listener and goes no
//! further. The tokens are fabricated.

use std::{
    io::{BufRead, BufReader},
    net::TcpListener,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use research_archive::auth::{self, ClientId, DataDirTokenStorage, OAuthUnavailable};
use research_archive::catalog::Catalog;
use research_archive::source::{LongbridgeSource, Rps};
use research_archive::types::{ErrorClass, ErrorDetail, FailureClass, Key, Response};

const CLIENT: &str = "test-client";
const DEADLINE: Duration = Duration::from_secs(2);
const REFRESH_TARGET: &str = "CONNECT openapi.longbridge.com:443 HTTP/1.1";

/// A proxy that records each request line and holds the connection open
/// without answering.
fn black_hole_proxy() -> (String, Arc<Mutex<Vec<String>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let seen = Arc::new(Mutex::new(Vec::new()));
    let log = seen.clone();
    std::thread::spawn(move || {
        let mut held = Vec::new();
        for stream in listener.incoming() {
            let Ok(stream) = stream else { break };
            let mut line = String::new();
            let _ = BufReader::new(stream.try_clone().unwrap()).read_line(&mut line);
            log.lock().unwrap().push(line.trim_end().to_owned());
            held.push(stream);
        }
    });
    (url, seen)
}

fn write_token(storage: &DataDirTokenStorage, expires_at: u64) {
    let token: auth::TokenFile = serde_json::from_value(serde_json::json!({
        "client_id": CLIENT,
        "access_token": "ap_m_FABRICATED-ACCESS",
        "refresh_token": "FABRICATED-REFRESH",
        "expires_at": expires_at,
    }))
    .unwrap();
    storage.save_checked(&token).unwrap();
}

fn refreshes(seen: &Mutex<Vec<String>>) -> usize {
    let seen = seen.lock().unwrap();
    assert!(
        seen.iter().all(|line| line == REFRESH_TARGET),
        "unexpected proxy traffic: {seen:?}"
    );
    seen.len()
}

#[test]
fn a_hanging_token_refresh_is_bounded() {
    let (proxy, seen) = black_hole_proxy();
    // SAFETY: the only other thread is the proxy, which never reads the
    // environment; the runtime is built below.
    unsafe {
        for var in [
            "HTTP_PROXY",
            "HTTPS_PROXY",
            "ALL_PROXY",
            "http_proxy",
            "https_proxy",
            "all_proxy",
        ] {
            std::env::set_var(var, &proxy);
        }
        for var in ["NO_PROXY", "no_proxy", "LONGBRIDGE_ENV", "LONGPORT_REGION"] {
            std::env::remove_var(var);
        }
        std::env::set_var("LONGBRIDGE_REGION", "global");
    }
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let root = tempfile::tempdir().unwrap();
    let storage = DataDirTokenStorage::open(root.path().join("oauth")).unwrap();
    let client = ClientId::parse(CLIENT).unwrap();

    // 1. A token inside the SDK's five-minute margin: the build refreshes,
    //    and the deadline ends the wait.
    write_token(&storage, auth::unix_now() + 10);
    let start = Instant::now();
    let built = runtime.block_on(auth::build_oauth(&client, storage.clone(), DEADLINE));
    assert_eq!(built.err(), Some(OAuthUnavailable::TimedOut));
    assert!(start.elapsed() >= DEADLINE);
    assert_eq!(refreshes(&seen), 1);

    // 2. A token that loads without a refresh, then enters the margin.
    write_token(&storage, auth::unix_now() + 302);
    let oauth = runtime
        .block_on(auth::build_oauth(&client, storage.clone(), DEADLINE))
        .expect("the token is outside the margin at build");
    assert_eq!(refreshes(&seen), 1);
    std::thread::sleep(Duration::from_secs(3));

    // The request target is never reached; the refresh hangs first.
    let source =
        LongbridgeSource::new(oauth, "http://127.0.0.1:1", Rps::new(20).unwrap(), DEADLINE);
    let ratings = *Catalog::compiled().get("ratings").unwrap();
    let timed_out = Response::Failed {
        class: FailureClass::Transient,
        error: ErrorDetail {
            class: ErrorClass::Transport,
            code: "request_timeout".into(),
        },
    };
    for expected_refreshes in [2, 3] {
        let start = Instant::now();
        let fetched = runtime.block_on(source.fetch(&ratings, &Key::new("AAPL.US").unwrap()));
        assert_eq!(fetched.response(), &timed_out);
        assert!(start.elapsed() >= DEADLINE);
        // A new refresh per fetch: the timed-out one released the `OAuth`
        // lock when its future was dropped.
        assert_eq!(refreshes(&seen), expected_refreshes);
    }
}
