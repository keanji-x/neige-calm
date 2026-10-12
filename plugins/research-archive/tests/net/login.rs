//! `auth::login` ends by its deadline even when the code exchange after the
//! browser callback never answers, and saves nothing.
//!
//! The callback is the one a browser would send after a consent: a
//! fabricated code with the state the SDK put in the authorization URL. The
//! SDK then exchanges the code through oauth2's HTTP client, which has no
//! timeout; here that request lands on the black-hole proxy. The CLI passes
//! `auth::LOGIN_TIMEOUT`; this test injects a short deadline instead.

use std::{
    io::{Read, Write},
    net::TcpStream,
    sync::mpsc,
    time::{Duration, Instant},
};

use research_archive::auth::{
    self, CallbackPort, ClientId, DataDirTokenStorage, LOGIN_TIMEOUT, OAuthUnavailable,
};

use crate::hermetic;
use crate::refresh::{CLIENT, current_thread};

/// A local port nothing listens on.
fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

#[test]
fn a_hanging_code_exchange_ends_the_login_by_its_deadline() {
    let (_serial, proxy) = hermetic::enter(true);
    let runtime = current_thread();
    let root = tempfile::tempdir().unwrap();
    let storage = DataDirTokenStorage::open(root.path().join("oauth")).unwrap();
    let client = ClientId::parse(CLIENT).unwrap();
    let port = free_port();

    // The "browser": wait for the URL, then call back with its state.
    let (url_tx, url_rx) = mpsc::channel::<String>();
    let browser = std::thread::spawn(move || {
        let url = url_rx.recv_timeout(Duration::from_secs(30)).unwrap();
        let url = url::Url::parse(&url).unwrap();
        let state = url
            .query_pairs()
            .find(|(k, _)| k == "state")
            .map(|(_, v)| v.into_owned())
            .unwrap();
        let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
        write!(
            stream,
            "GET /callback?code=FABRICATED-CODE&state={state} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n"
        )
        .unwrap();
        let mut reply = String::new();
        let _ = stream.read_to_string(&mut reply);
        reply
    });

    let deadline = Duration::from_secs(4);
    let start = Instant::now();
    let result = runtime.block_on(auth::login(
        &client,
        storage.clone(),
        CallbackPort::new(port).unwrap(),
        move |url| url_tx.send(url.to_owned()).unwrap(),
        deadline,
    ));
    let elapsed = start.elapsed();

    assert_eq!(result, Err(OAuthUnavailable::TimedOut));
    assert!(
        elapsed >= deadline && elapsed < deadline + Duration::from_secs(5),
        "{elapsed:?}"
    );
    assert!(browser.join().unwrap().starts_with("HTTP/1.1 200"));
    // The exchange went to the proxy, and nothing was saved.
    assert_eq!(proxy.oauth_requests(), 1);
    assert!(storage.load_checked(&client).unwrap().is_none());
}

#[test]
fn the_cli_login_bound_covers_the_browser_wait_and_is_finite() {
    // The SDK waits five minutes for the callback (`oauth/src/callback.rs:12`).
    assert!(LOGIN_TIMEOUT > Duration::from_secs(300));
    assert!(LOGIN_TIMEOUT <= Duration::from_secs(600));
}
