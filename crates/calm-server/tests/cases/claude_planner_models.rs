//! #1810: a Claude Planner chooses its model from the alias list, through the real routes and the
//! fake `claude` (see `claude_planner_stack_fixture.rs`). The card is read at issue time, so a PUT
//! between turns reaches the next spawn's `--model`.

use std::time::Duration;

use axum::http::StatusCode;
use calm_server::model::CardPatch;
use serde_json::{Value, json};

use super::claude_planner_stack_fixture::{Root, Stack};

/// The last spawn's `--model` value, or `None` when it passed none.
fn spawned_model(root: &Root) -> Option<String> {
    let argv = root.read_fake("argv").expect("a spawn recorded its argv");
    let argv: Vec<&str> = argv.lines().collect();
    let at = argv.iter().position(|arg| *arg == "--model")?;
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

/// Acceptance: create with `sonnet` → the first turn passes `--model sonnet`; PUT `haiku` → the
/// next turn passes `--model haiku`; PUT `null` → the next turn passes no `--model`.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_chosen_alias_reaches_each_next_turn() {
    let root = Root::new("exit");
    let stack = Stack::boot(&root).await;
    let (_track, card_id) = stack
        .create_claude_track_with(json!({"model": "sonnet"}))
        .await;

    let (outcome, _) = stack.run_turn(&root, &card_id, "exit", "first").await;
    assert_eq!(outcome["status"], "completed", "{outcome}");
    assert_eq!(spawned_model(&root).as_deref(), Some("sonnet"));

    let (status, body) = put_model(
        &stack,
        &card_id,
        json!({"model": "haiku", "reasoning_effort": null}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["model"], "haiku", "{body}");
    assert_eq!(body["effort_adjusted"], false, "{body}");
    assert_eq!(body["unknown_model"], false, "{body}");
    let (outcome, _) = stack.run_turn(&root, &card_id, "exit", "second").await;
    assert_eq!(outcome["status"], "completed", "{outcome}");
    assert_eq!(spawned_model(&root).as_deref(), Some("haiku"));

    let (status, body) = put_model(
        &stack,
        &card_id,
        json!({"model": null, "reasoning_effort": null}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (outcome, _) = stack.run_turn(&root, &card_id, "exit", "third").await;
    assert_eq!(outcome["status"], "completed", "{outcome}");
    assert_eq!(spawned_model(&root), None, "the default passes no --model");
}

/// Create and PUT refuse an unknown model and any effort with 400, and store nothing.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_unknown_model_or_an_effort_is_refused_at_create_and_put() {
    let root = Root::new("exit");
    let stack = Stack::boot(&root).await;
    for extra in [
        json!({"model": "claude-sonnet-4-5"}),
        json!({"model": "gpt-5"}),
        json!({"model": "sonnet", "reasoning_effort": "high"}),
        json!({"reasoning_effort": "low"}),
    ] {
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
        let tracks = stack
            .repo()
            .tracks_by_area(&area)
            .await
            .expect("tracks read");
        assert!(tracks.is_empty(), "{extra}: no track is minted");
    }

    let (_track, card_id) = stack
        .create_claude_track_with(json!({"model": "opus"}))
        .await;
    let before = payload(&stack, &card_id).await;
    for body in [
        json!({"model": "some-model", "reasoning_effort": null}),
        json!({"model": "opus", "reasoning_effort": "high"}),
        json!({"model": null, "reasoning_effort": "medium"}),
    ] {
        let (status, answer) = put_model(&stack, &card_id, body.clone()).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}: {answer}");
    }
    assert_eq!(payload(&stack, &card_id).await, before, "nothing stored");
}

/// A Claude card whose stored selection the alias list cannot satisfy is refused at issue (the
/// reader is told, nothing is spawned), and a PUT of an alias releases the queued message.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_payload_the_alias_list_cannot_run_is_refused_at_issue() {
    let root = Root::new("exit");
    let stack = Stack::boot(&root).await;
    let (_track, card_id) = stack.create_claude_track().await;
    let runtime = stack.runtime(&card_id).await;
    let mut stored = payload(&stack, &card_id).await;
    stored["model"] = json!("gpt-5");
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
        .expect("write an unrunnable selection");

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
    assert!(block.contains("gpt-5"), "{block}");
    assert!(root.read_fake("spawns").is_none(), "nothing was spawned");
    assert!(stack.outcomes(&card_id).await.is_empty());

    let (status, body) = put_model(
        &stack,
        &card_id,
        json!({"model": "opus", "reasoning_effort": null}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let outcomes = stack.wait_outcomes(&card_id, 1).await;
    assert_eq!(outcomes[0]["status"], "completed", "{outcomes:?}");
    assert_eq!(spawned_model(&root).as_deref(), Some("opus"));
}

/// `GET /api/models` for a Claude Planner: the alias catalog with the flag, `unavailable` without.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_catalog_is_the_alias_list_only_with_the_flag() {
    let root = Root::new("exit");
    let stack = Stack::boot(&root).await;
    let (_track, card_id) = stack.create_claude_track().await;
    let uris = [
        format!("/api/models?card_id={card_id}"),
        "/api/models?provider=claude".to_string(),
    ];
    for uri in &uris {
        let (status, body) = stack.send("GET", uri, None).await;
        assert_eq!(status, StatusCode::OK, "{uri}: {body}");
        assert_eq!(body["source"], "built_in", "{uri}: {body}");
        assert_eq!(body["default_source"], "unknown", "{uri}: {body}");
        assert_eq!(
            body["default"],
            json!({"model": null, "reasoning_effort": null})
        );
        assert_eq!(body["fetched_at_ms"], Value::Null);
        let aliases: Vec<&str> = body["models"]
            .as_array()
            .expect("models")
            .iter()
            .map(|m| m["model"].as_str().expect("alias"))
            .collect();
        assert_eq!(aliases, ["opus", "sonnet", "haiku"], "{uri}");
        for entry in body["models"].as_array().unwrap() {
            assert_eq!(entry["id"], entry["model"], "{entry}");
            assert_eq!(entry["is_default"], false, "{entry}");
            assert_eq!(entry["supported_reasoning_efforts"], json!([]), "{entry}");
            assert_eq!(entry["default_reasoning_effort"], Value::Null, "{entry}");
            assert!(
                entry["display_name"]
                    .as_str()
                    .is_some_and(|s| !s.is_empty())
            );
            assert!(entry["description"].as_str().is_some_and(|s| !s.is_empty()));
        }
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
