//! #1822: a Claude Planner chooses its model from the Claude CLI's own model list, through the
//! real routes and the fake `claude` (see `claude_planner_stack_fixture.rs`), which answers
//! `initialize` with the list measured from the pinned CLI. The card is read at issue time, so a
//! PUT between turns reaches the next spawn's `--model` and `--effort`.

use std::time::Duration;

use axum::http::StatusCode;
use calm_server::model::CardPatch;
use serde_json::{Value, json};

use super::claude_planner_stack_fixture::{Root, Stack};

/// Every value the fake CLI lists besides its `default`, in its order.
const LISTED: &str = "opus[1m], claude-fable-5-1[1m], sonnet, haiku";

/// The value after `flag` in the last spawn's argv, or `None` when it passed no `flag`.
fn spawned_flag(root: &Root, flag: &str) -> Option<String> {
    let argv = root.read_fake("argv").expect("a spawn recorded its argv");
    let argv: Vec<&str> = argv.lines().collect();
    let at = argv.iter().position(|arg| *arg == flag)?;
    Some(argv[at + 1].to_string())
}

async fn put_model(stack: &Stack, card_id: &str, body: Value) -> (StatusCode, Value) {
    stack
        .send(
            "PUT",
            &format!("/api/cards/{card_id}/planner/model"),
            Some(body),
        )
        .await
}

async fn payload(stack: &Stack, card_id: &str) -> Value {
    stack
        .repo()
        .card_get(card_id)
        .await
        .expect("card read")
        .expect("card")
        .payload
}

/// Acceptance: create with Fable + `high` → the first turn passes
/// `--model claude-fable-5-1[1m] --effort high`; PUT `haiku` → `--model haiku` and no effort;
/// PUT the default with `max` → no `--model` and `--effort max`; PUT `null`/`null` → neither.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_chosen_model_and_effort_reach_each_next_turn() {
    let root = Root::new("exit");
    let stack = Stack::boot(&root).await;
    let (_track, card_id) = stack
        .create_claude_track_with(
            json!({"model": "claude-fable-5-1[1m]", "reasoning_effort": "high"}),
        )
        .await;

    let (outcome, _) = stack.run_turn(&root, &card_id, "exit", "first").await;
    assert_eq!(outcome["status"], "completed", "{outcome}");
    assert_eq!(
        spawned_flag(&root, "--model").as_deref(),
        Some("claude-fable-5-1[1m]")
    );
    assert_eq!(spawned_flag(&root, "--effort").as_deref(), Some("high"));

    for (body, model, effort) in [
        (
            json!({"model": "haiku", "reasoning_effort": null}),
            Some("haiku"),
            None,
        ),
        (
            json!({"model": null, "reasoning_effort": "max"}),
            None,
            Some("max"),
        ),
        (json!({"model": null, "reasoning_effort": null}), None, None),
    ] {
        let (status, answer) = put_model(&stack, &card_id, body.clone()).await;
        assert_eq!(status, StatusCode::OK, "{body}: {answer}");
        assert_eq!(answer["model"], body["model"], "{answer}");
        assert_eq!(
            answer["reasoning_effort"], body["reasoning_effort"],
            "{answer}"
        );
        assert_eq!(answer["effort_adjusted"], false, "{answer}");
        assert_eq!(answer["unknown_model"], false, "{answer}");
        let (outcome, _) = stack.run_turn(&root, &card_id, "exit", "next").await;
        assert_eq!(outcome["status"], "completed", "{outcome}");
        assert_eq!(spawned_flag(&root, "--model").as_deref(), model, "{body}");
        assert_eq!(spawned_flag(&root, "--effort").as_deref(), effort, "{body}");
    }
}

/// Create and PUT refuse a value the list does not carry (naming the listed values, and the
/// pre-#1822 alias `opus` among them), the CLI's own `default` as a value, and an effort the entry
/// does not declare, with 400, and store nothing.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_value_or_effort_the_list_does_not_carry_is_refused_at_create_and_put() {
    let root = Root::new("exit");
    let stack = Stack::boot(&root).await;
    let refusals = [
        (json!({"model": "opus"}), format!("choose one of {LISTED}")),
        (
            json!({"model": "default"}),
            format!("choose one of {LISTED}"),
        ),
        (json!({"model": "gpt-5"}), format!("choose one of {LISTED}")),
        (
            json!({"model": "haiku", "reasoning_effort": "low"}),
            "`haiku` declares no reasoning effort".to_string(),
        ),
        (
            json!({"model": "sonnet", "reasoning_effort": "ultra"}),
            "choose one of low, medium, high, xhigh, max".to_string(),
        ),
        (
            json!({"reasoning_effort": "ultra"}),
            "the Claude CLI's default does not support".to_string(),
        ),
    ];
    for (extra, expected) in &refusals {
        let area = stack.area().await;
        let mut request = json!({
            "planner_provider": "claude",
            "area_id": area,
            "title": "refused",
            "theme": {"fg": [216, 219, 226], "bg": [15, 20, 24]},
        });
        for (key, value) in extra.as_object().unwrap() {
            request[key] = value.clone();
        }
        let (status, body) = stack.send("POST", "/api/tracks", Some(request)).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{extra}: {body}");
        assert!(
            body.to_string().contains(expected.as_str()),
            "{extra}: {body}"
        );
        let tracks = stack
            .repo()
            .tracks_by_area(&area)
            .await
            .expect("tracks read");
        assert!(tracks.is_empty(), "{extra}: no track is minted");
    }

    let (_track, card_id) = stack
        .create_claude_track_with(json!({"model": "sonnet"}))
        .await;
    let before = payload(&stack, &card_id).await;
    for (extra, expected) in &refusals {
        let body = json!({
            "model": extra.get("model").cloned().unwrap_or(Value::Null),
            "reasoning_effort": extra.get("reasoning_effort").cloned().unwrap_or(Value::Null),
        });
        let (status, answer) = put_model(&stack, &card_id, body.clone()).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}: {answer}");
        assert!(
            answer.to_string().contains(expected.as_str()),
            "{body}: {answer}"
        );
    }
    assert_eq!(payload(&stack, &card_id).await, before, "nothing stored");
}

/// A Claude card whose stored value the list no longer carries (here the pre-#1822 alias `opus`)
/// is refused at issue, naming the listed values; the reader is told, nothing is spawned or
/// substituted, and a PUT of a listed value releases the queued message.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_stored_value_the_list_does_not_carry_is_refused_at_issue() {
    let root = Root::new("exit");
    let stack = Stack::boot(&root).await;
    let (_track, card_id) = stack.create_claude_track().await;
    let runtime = stack.runtime(&card_id).await;
    let mut stored = payload(&stack, &card_id).await;
    stored["model"] = json!("opus");
    stored["model_ever_set"] = json!(true);
    stack
        .repo()
        .card_update(
            &card_id,
            CardPatch {
                title: None,
                kind: None,
                sort: None,
                payload: Some(stored),
                deletable: None,
            },
        )
        .await
        .expect("write a value the list does not carry");

    let harness = stack.harness(&runtime.id);
    let (status, body) = stack.post_input(&card_id, "hello?").await;
    assert_eq!(status, StatusCode::OK, "the message is queued: {body}");
    let mut block = None;
    for _ in 0..200 {
        block = harness.issuance_block().await;
        if block.is_some() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let block = block.expect("the reader is told why nothing is sent");
    assert!(block.contains("`opus`"), "{block}");
    assert!(
        block.contains(&format!("choose one of {LISTED}")),
        "{block}"
    );
    assert!(root.read_fake("spawns").is_none(), "nothing was spawned");
    assert!(stack.outcomes(&card_id).await.is_empty());

    let (status, body) = put_model(
        &stack,
        &card_id,
        json!({"model": "opus[1m]", "reasoning_effort": null}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let outcomes = stack.wait_outcomes(&card_id, 1).await;
    assert_eq!(outcomes[0]["status"], "completed", "{outcomes:?}");
    assert_eq!(spawned_flag(&root, "--model").as_deref(), Some("opus[1m]"));
}

/// `GET /api/models` for a Claude Planner: the CLI's list with its resolved models and effort
/// levels, and the CLI's `default` entry as the default; `unavailable` without the flag.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_catalog_is_the_cli_list_with_the_flag() {
    let root = Root::new("exit");
    let stack = Stack::boot(&root).await;
    let (_track, card_id) = stack.create_claude_track().await;
    let uris = [
        format!("/api/models?card_id={card_id}"),
        "/api/models?provider=claude".to_string(),
    ];
    let levels: Vec<Value> = ["low", "medium", "high", "xhigh", "max"]
        .iter()
        .map(|level| json!({"reasoning_effort": level, "description": null}))
        .collect();
    for uri in &uris {
        let (status, body) = stack.send("GET", uri, None).await;
        assert_eq!(status, StatusCode::OK, "{uri}: {body}");
        assert_eq!(body["source"], "live", "{uri}: {body}");
        assert_eq!(body["default_source"], "claude_cli", "{uri}: {body}");
        assert_eq!(
            body["default"],
            json!({"model": "claude-opus-5-5[1m]", "reasoning_effort": null,
                   "supported_reasoning_efforts": levels}),
            "{uri}"
        );
        assert!(
            body["fetched_at_ms"].as_i64().is_some_and(|ms| ms > 0),
            "{body}"
        );
        let entries: Vec<(&str, &str, &str)> = body["models"]
            .as_array()
            .expect("models")
            .iter()
            .map(|m| {
                (
                    m["model"].as_str().expect("model"),
                    m["resolved_model"].as_str().expect("resolved model"),
                    m["display_name"].as_str().expect("display name"),
                )
            })
            .collect();
        assert_eq!(
            entries,
            [
                ("opus[1m]", "claude-opus-5-5[1m]", "Opus (1M context)"),
                ("claude-fable-5-1[1m]", "claude-fable-5-1", "Fable"),
                ("sonnet", "claude-sonnet-5", "Sonnet"),
                ("haiku", "claude-haiku-4-5-20251001", "Haiku"),
            ],
            "{uri}"
        );
        for entry in body["models"].as_array().unwrap() {
            assert_eq!(entry["id"], entry["model"], "{entry}");
            assert_eq!(entry["is_default"], false, "{entry}");
            assert_eq!(entry["default_reasoning_effort"], Value::Null, "{entry}");
            let expected = if entry["model"] == "haiku" {
                json!([])
            } else {
                json!(levels)
            };
            assert_eq!(entry["supported_reasoning_efforts"], expected, "{entry}");
        }
        assert!(
            !body.to_string().contains("owner@example.invalid"),
            "{body}"
        );
    }
    stack.shutdown().await;

    let stack = Stack::boot_with(&root, false).await;
    for uri in &uris {
        let (status, body) = stack.send("GET", uri, None).await;
        assert_eq!(status, StatusCode::OK, "{uri}: {body}");
        assert_eq!(body["source"], "unavailable", "{uri}: {body}");
        assert_eq!(body["models"], json!([]), "{uri}: {body}");
    }
}
