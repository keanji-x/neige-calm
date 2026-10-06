//! #2132 item 1: the login route compares credentials in constant time and refuses a peer that keeps failing.
//! Every request goes through the production router assembly with the peer address a real listener attaches.

use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::body::Body;
use axum::http::{HeaderMap, Request, StatusCode, header};
use calm_server::auth::login_throttle::{FIRST_LOCKOUT, FREE_FAILURES, LoginThrottle, MAX_PEERS};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tower::ServiceExt;

use super::auth::{fresh_state, live_auth_state};

const ATTACKER: &str = "100.64.0.7:5000";
const OWNER: &str = "100.64.0.8:5000";

struct Fixture {
    app: axum::Router,
    throttle: LoginThrottle,
    now: Arc<Mutex<Instant>>,
}

impl Fixture {
    async fn new() -> Self {
        let now = Arc::new(Mutex::new(Instant::now()));
        let read = now.clone();
        let throttle = LoginThrottle::with_clock(Arc::new(move || *read.lock().unwrap()));
        let auth = live_auth_state("alice", "hunter2").with_login_throttle(throttle.clone());
        Self {
            app: calm_server::routes::application_router(fresh_state().await, auth),
            throttle,
            now,
        }
    }

    fn advance(&self, by: Duration) {
        *self.now.lock().unwrap() += by;
    }

    async fn login(
        &self,
        peer: Option<&str>,
        username: &str,
        password: &str,
    ) -> (StatusCode, HeaderMap, Value) {
        let mut request =
            Request::post("/api/auth/login").header(header::CONTENT_TYPE, "application/json");
        if let Some(peer) = peer {
            request = request.extension(axum::extract::ConnectInfo(
                peer.parse::<SocketAddr>().unwrap(),
            ));
        }
        let body = Body::from(json!({ "username": username, "password": password }).to_string());
        let response = self
            .app
            .clone()
            .oneshot(request.body(body).unwrap())
            .await
            .unwrap();
        let status = response.status();
        let headers = response.headers().clone();
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        (
            status,
            headers,
            serde_json::from_slice(&bytes).unwrap_or(Value::Null),
        )
    }

    async fn fail(&self, peer: &str, times: u32) {
        for _ in 0..times {
            assert_eq!(
                self.login(Some(peer), "alice", "wrong").await.0,
                StatusCode::UNAUTHORIZED
            );
        }
    }
}

fn assert_throttled((status, headers, body): (StatusCode, HeaderMap, Value), retry_after: &str) {
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS, "{body}");
    assert_eq!(body["code"], "login_throttled", "{body}");
    assert_eq!(
        body["error"],
        format!("Too many failed sign-in attempts. Try again in {retry_after} seconds.")
    );
    assert_eq!(
        headers
            .get(header::RETRY_AFTER)
            .map(|v| v.to_str().unwrap()),
        Some(retry_after)
    );
    assert!(
        headers.get(header::SET_COOKIE).is_none(),
        "a refused login mints no session"
    );
}

#[tokio::test]
async fn a_peer_that_keeps_failing_is_refused_even_with_the_right_password() {
    let f = Fixture::new().await;
    f.fail(ATTACKER, FREE_FAILURES).await;
    // Another source port of the same host is the same peer.
    assert_throttled(
        f.login(Some("100.64.0.7:5001"), "alice", "hunter2").await,
        "2",
    );
    f.advance(Duration::from_millis(500));
    assert_throttled(f.login(Some(ATTACKER), "alice", "hunter2").await, "2");
}

#[tokio::test]
async fn after_the_window_the_right_password_signs_in_and_clears_the_peer() {
    let f = Fixture::new().await;
    f.fail(ATTACKER, FREE_FAILURES).await;
    assert_throttled(f.login(Some(ATTACKER), "alice", "hunter2").await, "2");
    f.advance(FIRST_LOCKOUT);
    let (status, headers, _) = f.login(Some(ATTACKER), "alice", "hunter2").await;
    assert_eq!(status, StatusCode::OK);
    assert!(headers.get(header::SET_COOKIE).is_some());
    assert_eq!(f.throttle.tracked_peers(), 0, "a success forgets the peer");
    // A fresh budget: the failures before the success no longer count.
    f.fail(ATTACKER, FREE_FAILURES - 1).await;
    assert_eq!(
        f.login(Some(ATTACKER), "alice", "hunter2").await.0,
        StatusCode::OK
    );
}

#[tokio::test]
async fn a_different_peer_is_unaffected() {
    let f = Fixture::new().await;
    f.fail(ATTACKER, FREE_FAILURES).await;
    assert_throttled(f.login(Some(ATTACKER), "alice", "hunter2").await, "2");
    assert_eq!(
        f.login(Some(OWNER), "alice", "wrong").await.0,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        f.login(Some(OWNER), "alice", "hunter2").await.0,
        StatusCode::OK
    );
}

#[tokio::test]
async fn either_credential_alone_is_refused() {
    let f = Fixture::new().await;
    for (username, password) in [
        ("mallory", "hunter2"),
        ("alice", "hunter3"),
        ("alic", "hunter2"),
        ("", ""),
    ] {
        let (status, headers, body) = f.login(Some(OWNER), username, password).await;
        assert_eq!(
            status,
            StatusCode::UNAUTHORIZED,
            "{username:?}/{password:?}: {body}"
        );
        assert!(headers.get(header::SET_COOKIE).is_none());
    }
}

#[tokio::test]
async fn a_login_without_a_peer_address_fails_closed() {
    let f = Fixture::new().await;
    let (status, headers, _) = f.login(None, "alice", "hunter2").await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
    assert!(headers.get(header::SET_COOKIE).is_none());
}

#[tokio::test]
async fn the_peer_record_stays_bounded() {
    let f = Fixture::new().await;
    let peers = MAX_PEERS + 16;
    for i in 0..peers {
        let peer = format!("10.{}.{}.1:4000", i / 256, i % 256);
        f.fail(&peer, 1).await;
        f.advance(Duration::from_millis(1));
    }
    assert_eq!(f.throttle.tracked_peers(), MAX_PEERS);
    // The newest peer kept its record: its earlier failure still counts toward the budget.
    let newest = format!("10.{}.{}.1:4000", (peers - 1) / 256, (peers - 1) % 256);
    f.fail(&newest, FREE_FAILURES - 1).await;
    assert_throttled(f.login(Some(&newest), "alice", "hunter2").await, "2");
}
