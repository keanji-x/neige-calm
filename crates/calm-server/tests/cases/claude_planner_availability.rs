//! #1817: `GET /api/agent-providers` and the track-create gate, through the production boot and
//! routes with the fake `claude` (see `claude_planner_stack_fixture.rs`; its shared Codex daemon
//! is down). Covers every Claude status and reason, the cache, one check in flight for concurrent
//! readers, and that no account identity the CLI prints reaches a response.

use std::time::Duration;

use axum::http::StatusCode;
use serde_json::{Value, json};

use super::claude_planner_stack_fixture::{Root, Stack};

async fn providers(stack: &Stack, query: &str) -> Value {
    let (status, body) = stack
        .send("GET", &format!("/api/agent-providers{query}"), None)
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    body
}

fn entry<'a>(body: &'a Value, provider: &str) -> &'a Value {
    body.as_array()
        .expect("an array of providers")
        .iter()
        .find(|entry| entry["provider"] == provider)
        .unwrap_or_else(|| panic!("no {provider} entry in {body}"))
}

fn root_with_auth(auth: &str) -> Root {
    let root = Root::new("exit");
    std::fs::write(root.fake_dir().join("auth"), auth).expect("auth answer");
    root
}

fn auth_calls(root: &Root) -> usize {
    root.read_fake("auth-calls")
        .map(|calls| calls.lines().count())
        .unwrap_or(0)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_logged_in_claude_is_ready_and_no_identity_leaves_the_server() {
    let root = root_with_auth("logged-in");
    let stack = Stack::boot(&root).await;
    let body = providers(&stack, "").await;

    let claude = entry(&body, "claude");
    assert_eq!(claude["status"], "ready", "{body}");
    assert_eq!(claude["reason"], Value::Null, "{body}");
    assert!(
        claude["checked_at_ms"].as_i64().is_some_and(|ms| ms > 0),
        "{body}"
    );
    let text = body.to_string();
    for identity in [
        "owner@example.invalid",
        "fake org",
        "claude.ai",
        "firstParty",
    ] {
        assert!(!text.contains(identity), "{identity} leaked: {text}");
    }

    // The check ran the binary the turn runs, with the turn's own allowlist and config dir.
    let env = root
        .read_fake("auth-env")
        .expect("auth status recorded its env");
    let config_dir = root.path().join("claude-config");
    assert!(
        env.lines()
            .any(|line| line == format!("CLAUDE_CONFIG_DIR={}", config_dir.display())),
        "{env}"
    );
    assert!(!env.contains("NEIGE_MCP_TOKEN"), "{env}");
    assert!(!env.contains("ANTHROPIC_"), "{env}");

    // The shared Codex daemon of this stack is down.
    let codex = entry(&body, "codex");
    assert_eq!(codex["status"], "unavailable", "{body}");
    assert!(
        codex["reason"]
            .as_str()
            .is_some_and(|reason| reason.contains("not running")),
        "{body}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_logged_out_claude_is_unavailable_with_the_login_hint_and_create_is_refused() {
    let root = root_with_auth("logged-out");
    let stack = Stack::boot(&root).await;
    let body = providers(&stack, "").await;
    let claude = entry(&body, "claude");
    assert_eq!(claude["status"], "unavailable", "{body}");
    let reason = claude["reason"].as_str().expect("a reason");
    let hint = format!(
        "not logged in — run `claude /login` with CLAUDE_CONFIG_DIR={}",
        root.path().join("claude-config").display()
    );
    assert_eq!(reason, hint, "{body}");

    let area = stack.area().await;
    let (status, refusal) = stack
        .send(
            "POST",
            "/api/tracks",
            Some(json!({
                "planner_provider": "claude",
                "area_id": area,
                "title": "refused",
                "theme": {"fg": [216, 219, 226], "bg": [15, 20, 24]},
            })),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{refusal}");
    assert!(refusal.to_string().contains(&hint), "{refusal}");
    let tracks = stack
        .repo()
        .tracks_by_area(&area)
        .await
        .expect("tracks read");
    assert!(
        tracks.is_empty(),
        "a refused create mints nothing: {tracks:?}"
    );
    assert!(root.read_fake("spawns").is_none(), "nothing was spawned");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_answer_without_a_boolean_is_unavailable_never_a_guess() {
    for answer in ["no-field", "not-bool", "not-json"] {
        let root = root_with_auth(answer);
        let stack = Stack::boot(&root).await;
        let body = providers(&stack, "").await;
        let claude = entry(&body, "claude");
        assert_eq!(claude["status"], "unavailable", "{answer}: {body}");
        assert!(
            claude["reason"]
                .as_str()
                .is_some_and(|reason| reason.contains("printed no boolean `loggedIn`")),
            "{answer}: {body}"
        );
        assert!(
            !body.to_string().contains("owner@example.invalid"),
            "{body}"
        );
    }
}

/// The first failed check is the reason: a wrong version is reported and `auth status` never runs.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_version_mismatch_is_the_reason_before_login_is_asked() {
    let root = root_with_auth("logged-in");
    std::fs::write(root.fake_dir().join("version"), "2.1.999").expect("version");
    let stack = Stack::boot(&root).await;
    let body = providers(&stack, "").await;
    let claude = entry(&body, "claude");
    assert_eq!(claude["status"], "unavailable", "{body}");
    let reason = claude["reason"].as_str().expect("a reason");
    assert!(
        reason.contains(r#"reports "2.1.999", the config pins "2.1.280""#),
        "{reason}"
    );
    assert!(
        reason.contains("`claude_version`"),
        "the fix names the field: {reason}"
    );
    assert_eq!(
        auth_calls(&root),
        0,
        "login is not asked after a failed check"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn without_the_flag_claude_is_not_configured() {
    let root = root_with_auth("logged-in");
    let stack = Stack::boot_with(&root, false).await;
    let body = providers(&stack, "").await;
    let claude = entry(&body, "claude");
    assert_eq!(claude["status"], "not_configured", "{body}");
    assert!(
        claude["reason"]
            .as_str()
            .is_some_and(|reason| reason.contains("--claude-planner-config")),
        "{body}"
    );
    assert_eq!(auth_calls(&root), 0);
}

/// A second read inside the TTL is the cached answer; `refresh=true` checks again, and sees a
/// login that happened since.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn answers_are_cached_until_a_refresh_rechecks() {
    let root = root_with_auth("logged-out");
    let stack = Stack::boot(&root).await;
    let first = providers(&stack, "").await;
    assert_eq!(entry(&first, "claude")["status"], "unavailable");
    assert_eq!(auth_calls(&root), 1);

    std::fs::write(root.fake_dir().join("auth"), "logged-in").expect("log in");
    let cached = providers(&stack, "").await;
    assert_eq!(entry(&cached, "claude"), entry(&first, "claude"), "cached");
    assert_eq!(auth_calls(&root), 1);

    let rechecked = providers(&stack, "?refresh=true").await;
    assert_eq!(
        entry(&rechecked, "claude")["status"],
        "ready",
        "{rechecked}"
    );
    assert_eq!(auth_calls(&root), 2);
}

/// Readers that arrive while a check runs share it.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_readers_share_one_check_in_flight() {
    let root = root_with_auth("hold");
    let stack = Stack::boot(&root).await;
    let first = tokio::spawn({
        let app = stack.app.clone();
        async move { get(app).await }
    });
    let entered = root.fake_dir().join("auth-entered");
    for _ in 0..400 {
        if entered.exists() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    assert!(entered.exists(), "the first check reached auth status");
    let second = tokio::spawn({
        let app = stack.app.clone();
        async move { get(app).await }
    });
    // Let the second reader queue behind the running check before it is released.
    tokio::time::sleep(Duration::from_millis(300)).await;
    std::fs::write(root.fake_dir().join("release-auth"), "").expect("release");
    let (first, second) = (first.await.expect("first"), second.await.expect("second"));
    assert_eq!(entry(&first, "claude")["status"], "ready", "{first}");
    assert_eq!(entry(&first, "claude"), entry(&second, "claude"));
    assert_eq!(auth_calls(&root), 1, "one check served both readers");
}

async fn get(app: axum::Router) -> Value {
    use http_body_util::BodyExt;
    use tower::ServiceExt;
    let response = app
        .oneshot(
            axum::http::Request::builder()
                .uri("/api/agent-providers")
                .header("x-calm-actor", "user")
                .body(axum::body::Body::empty())
                .expect("request"),
        )
        .await
        .expect("response");
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    serde_json::from_slice(&bytes).expect("json")
}
