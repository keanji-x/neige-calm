//! #1822: the Claude CLI's model list is fetched by the #1817 availability check, through the
//! production boot and routes with the fake `claude` (see `claude_planner_stack_fixture.rs`),
//! whose `initialize` calls are counted. The list is fetched by the boot check, the first read and
//! a Recheck, never by the 30 s TTL re-check; a list that cannot be read makes Claude unavailable
//! with neige's own reason, and the account the answer carries never leaves the server.

use std::sync::{Arc, Mutex};

use axum::http::StatusCode;
use serde_json::{Value, json};

use super::claude_planner_stack_fixture::{Root, Stack};

const ACCOUNT: [&str; 2] = ["owner@example.invalid", "fake org"];

fn root_with_catalog(catalog: &str) -> Root {
    let root = Root::new("exit");
    std::fs::write(root.fake_dir().join("catalog"), catalog).expect("catalog answer");
    root
}

fn count(root: &Root, file: &str) -> usize {
    root.read_fake(file)
        .map(|calls| calls.lines().count())
        .unwrap_or(0)
}

async fn claude_entry(stack: &Stack, query: &str) -> Value {
    let (status, body) = stack
        .send("GET", &format!("/api/agent-providers{query}"), None)
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    no_account(&body);
    body.as_array()
        .expect("providers")
        .iter()
        .find(|entry| entry["provider"] == "claude")
        .expect("a claude entry")
        .clone()
}

async fn claude_models(stack: &Stack) -> Value {
    let (status, body) = stack.send("GET", "/api/models?provider=claude", None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    no_account(&body);
    body
}

fn values(models: &Value) -> Vec<String> {
    models["models"]
        .as_array()
        .expect("models")
        .iter()
        .map(|m| m["model"].as_str().expect("model").to_string())
        .collect()
}

fn no_account(body: &Value) {
    let text = body.to_string();
    for identity in ACCOUNT {
        assert!(!text.contains(identity), "{identity} leaked: {text}");
    }
}

/// The first read fetches; a read inside the TTL is the cache; the TTL re-check runs version and
/// login again but keeps the list, even though the CLI now lists less; a Recheck fetches it.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_first_read_and_a_recheck_fetch_the_list_and_the_ttl_recheck_does_not() {
    let root = root_with_catalog("ok");
    let stack = Stack::boot(&root).await;
    assert_eq!(
        count(&root, "init-calls"),
        0,
        "the fixture boot runs no check"
    );

    let first = claude_models(&stack).await;
    assert_eq!(first["source"], "live", "{first}");
    assert_eq!(count(&root, "init-calls"), 1, "the first read fetched");
    assert_eq!(claude_models(&stack).await, first, "cached");
    assert_eq!(count(&root, "init-calls"), 1);

    std::fs::write(root.fake_dir().join("catalog"), "restricted").expect("restrict");
    stack
        .state
        .age_provider_availability_past_ttl_for_test()
        .await;
    let auth_before = count(&root, "auth-calls");
    assert_eq!(claude_entry(&stack, "").await["status"], "ready");
    assert_eq!(
        count(&root, "auth-calls"),
        auth_before + 1,
        "the TTL re-check asked for the login again"
    );
    assert_eq!(count(&root, "init-calls"), 1, "and kept the list");
    assert_eq!(values(&claude_models(&stack).await), values(&first));

    assert_eq!(
        claude_entry(&stack, "?refresh=true").await["status"],
        "ready"
    );
    assert_eq!(count(&root, "init-calls"), 2, "a recheck fetched");
    assert_eq!(values(&claude_models(&stack).await), ["haiku"]);
}

/// The boot check fetches the list, and the reads after it are answered from it.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_boot_check_fetches_the_list() {
    let root = root_with_catalog("ok");
    let stack = Stack::boot(&root).await;
    calm_server::agent_providers::spawn_boot_check(&stack.state)
        .await
        .expect("boot check");
    assert_eq!(count(&root, "init-calls"), 1);
    assert_eq!(claude_models(&stack).await["source"], "live");
    let (_track, _card) = stack
        .create_claude_track_with(json!({"model": "claude-fable-5-1[1m]"}))
        .await;
    assert_eq!(
        count(&root, "init-calls"),
        1,
        "answered from the boot check's list"
    );
}

/// A list that cannot be read is `unavailable` with the reason and no list; create and PUT are
/// refused with it, nothing is spawned, and once the CLI lists its models again the next check
/// after the TTL fetches the list (none is cached).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_malformed_or_empty_list_is_unavailable_with_a_reason_and_no_list() {
    for answer in ["malformed", "empty-list", "empty"] {
        let root = root_with_catalog(answer);
        let stack = Stack::boot(&root).await;
        let claude = claude_entry(&stack, "").await;
        assert_eq!(claude["status"], "unavailable", "{answer}: {claude}");
        let reason = claude["reason"].as_str().expect("a reason");
        assert!(
            reason.contains("-p (initialize) printed no usable model list"),
            "{answer}: {reason}"
        );
        let models = claude_models(&stack).await;
        assert_eq!(models["source"], "unavailable", "{answer}: {models}");
        assert_eq!(models["models"], json!([]), "{answer}: {models}");

        let (status, refusal) = stack
            .send(
                "POST",
                "/api/tracks",
                Some(json!({
                    "planner_provider": "claude",
                    "area_id": stack.area().await,
                    "title": "refused",
                    "theme": {"fg": [216, 219, 226], "bg": [15, 20, 24]},
                })),
            )
            .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{answer}: {refusal}");
        assert!(refusal.to_string().contains(reason), "{answer}: {refusal}");
        assert!(
            root.read_fake("spawns").is_none(),
            "{answer}: nothing spawned"
        );

        std::fs::write(root.fake_dir().join("catalog"), "ok").expect("fix the CLI");
        stack
            .state
            .age_provider_availability_past_ttl_for_test()
            .await;
        assert_eq!(
            claude_entry(&stack, "").await["status"],
            "ready",
            "{answer}"
        );
        assert_eq!(count(&root, "init-calls"), 2, "{answer}: fetched again");
    }
}

/// A card created while the list was readable, whose CLI then stops listing: a PUT needs Claude
/// ready (6′), so it is refused with the reason, in the create gate's words, and nothing is stored.
/// A turn consults no catalog: it still runs with the stored selection.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn with_no_list_a_put_is_refused_and_a_turn_still_runs() {
    let root = root_with_catalog("ok");
    let stack = Stack::boot(&root).await;
    let (_track, card_id) = stack
        .create_claude_track_with(json!({"model": "sonnet", "reasoning_effort": "high"}))
        .await;
    std::fs::write(root.fake_dir().join("catalog"), "empty").expect("break the CLI");
    let claude = claude_entry(&stack, "?refresh=true").await;
    assert_eq!(claude["status"], "unavailable");
    let reason = claude["reason"].as_str().expect("a reason").to_string();

    let (status, body) = stack
        .send(
            "PUT",
            &format!("/api/cards/{card_id}/planner/model"),
            Some(json!({"model": "haiku", "reasoning_effort": null})),
        )
        .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    let expected = format!("`planner_provider` `claude` is unavailable: {reason}");
    assert!(body.to_string().contains(&expected), "{body}");
    let stored = stack
        .repo()
        .card_get(&card_id)
        .await
        .expect("card read")
        .expect("card")
        .payload;
    assert_eq!(stored["model"], "sonnet", "nothing stored: {stored}");

    let (outcome, _) = stack.run_turn(&root, &card_id, "exit", "hello?").await;
    assert_eq!(outcome["status"], "completed", "{outcome}");
    let argv = root.read_fake("argv").expect("argv");
    assert!(argv.lines().any(|arg| arg == "--model=sonnet"), "{argv}");
    assert!(argv.lines().any(|arg| arg == "--effort=high"), "{argv}");
}

/// Every log line of the process, captured for the account check below.
#[derive(Clone, Default)]
struct Captured(Arc<Mutex<Vec<u8>>>);

impl std::io::Write for Captured {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.lock().expect("log buffer").extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// The account the `initialize` answer carries reaches no response and no log line, whether the
/// list is read (the boot check, the routes) or refused (the boot warning names the reason).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_account_reaches_no_response_and_no_log_line() {
    let captured = Captured::default();
    let writer = captured.clone();
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::TRACE)
        .with_ansi(false)
        .with_writer(move || writer.clone())
        .try_init()
        .expect("nextest runs each test in its own process, so this is the only subscriber");

    for answer in ["ok", "malformed"] {
        let root = root_with_catalog(answer);
        let stack = Stack::boot(&root).await;
        calm_server::agent_providers::spawn_boot_check(&stack.state)
            .await
            .expect("boot check");
        let claude = claude_entry(&stack, "").await;
        let models = claude_models(&stack).await;
        if answer == "ok" {
            assert_eq!(claude["status"], "ready", "{claude}");
            let (_track, card_id) = stack.create_claude_track().await;
            let (status, body) = stack
                .send("GET", &format!("/api/models?card_id={card_id}"), None)
                .await;
            assert_eq!(status, StatusCode::OK, "{body}");
            no_account(&body);
        } else {
            assert_eq!(models["source"], "unavailable", "{models}");
        }
    }
    let logs = String::from_utf8(captured.0.lock().expect("log buffer").clone()).expect("utf-8");
    assert!(
        logs.contains("planner provider unavailable at boot")
            && logs.contains("printed no usable model list"),
        "the refused list was logged with its reason: {logs}"
    );
    for identity in ACCOUNT {
        assert!(!logs.contains(identity), "{identity} reached a log line");
    }
}
