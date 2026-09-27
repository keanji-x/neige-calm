//! #1822: a Claude Planner chooses its model from the Claude CLI's own model list, through the
//! real routes and the fake `claude` (see `claude_planner_stack_fixture.rs`), which answers
//! `initialize` with the list measured from the pinned CLI and refuses a model it does not list as
//! the pinned CLI does. A write is advised against the list as Codex's is (6′); the card is read at
//! issue time, so a PUT between turns reaches the next spawn's `--model=` and `--effort=`.

use axum::http::StatusCode;
use serde_json::{Value, json};

use super::claude_planner_stack_fixture::{Root, Stack};

/// The value of the last spawn's `<flag>=<value>` token, or `None` when it passed no `flag`.
fn spawned_flag(root: &Root, flag: &str) -> Option<String> {
    let argv = root.read_fake("argv").expect("a spawn recorded its argv");
    let prefix = format!("{flag}=");
    let values: Vec<String> = argv
        .lines()
        .filter_map(|arg| arg.strip_prefix(&prefix).map(str::to_string))
        .collect();
    assert!(values.len() <= 1, "{flag} passed twice: {argv}");
    assert!(
        !argv.lines().any(|arg| arg == flag),
        "{flag} passed as its own token: {argv}"
    );
    values.into_iter().next()
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

/// A PUT is advised like Codex's (6′): a value the list does not carry (the pre-#1822 alias `opus`)
/// is stored and reported `unknown_model`; an effort the entry does not declare is dropped, since a
/// Claude entry declares no default effort, and reported `effort_adjusted`; a null model's effort
/// is judged on the CLI's `default` entry.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_put_stores_an_unlisted_value_as_unknown_and_drops_an_undeclared_effort() {
    let root = Root::new("exit");
    let stack = Stack::boot(&root).await;
    let (_track, card_id) = stack.create_claude_track().await;
    for (body, stored_effort, adjusted, unknown) in [
        (
            json!({"model": "opus", "reasoning_effort": null}),
            None,
            false,
            true,
        ),
        (
            json!({"model": "haiku", "reasoning_effort": "low"}),
            None,
            true,
            false,
        ),
        (
            json!({"model": null, "reasoning_effort": "ultra"}),
            None,
            true,
            false,
        ),
        (
            json!({"model": "sonnet", "reasoning_effort": "max"}),
            Some("max"),
            false,
            false,
        ),
        (
            json!({"model": null, "reasoning_effort": "xhigh"}),
            Some("xhigh"),
            false,
            false,
        ),
    ] {
        let (status, answer) = put_model(&stack, &card_id, body.clone()).await;
        assert_eq!(status, StatusCode::OK, "{body}: {answer}");
        assert_eq!(answer["model"], body["model"], "{body}: {answer}");
        assert_eq!(
            answer["reasoning_effort"],
            json!(stored_effort),
            "{body}: {answer}"
        );
        assert_eq!(answer["effort_adjusted"], adjusted, "{body}: {answer}");
        assert_eq!(answer["unknown_model"], unknown, "{body}: {answer}");
        let stored = payload(&stack, &card_id).await;
        assert_eq!(stored["model"], body["model"], "{body}: {stored}");
        assert_eq!(
            stored["reasoning_effort"],
            json!(stored_effort),
            "{body}: {stored}"
        );
    }
}

/// Create uses the same advice and, as for Codex, refuses an effort the entry does not declare
/// rather than adjusting it; a value the list does not carry is minted, for the CLI to judge.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn create_refuses_an_undeclared_effort_and_mints_an_unlisted_value() {
    let root = Root::new("exit");
    let stack = Stack::boot(&root).await;
    for (extra, expected) in [
        (
            json!({"model": "haiku", "reasoning_effort": "low"}),
            "reasoning_effort `low` is unsupported for model `haiku`; it declares no default effort",
        ),
        (
            json!({"reasoning_effort": "ultra"}),
            "reasoning_effort `ultra` is unsupported for the default model",
        ),
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
        assert!(body.to_string().contains(expected), "{extra}: {body}");
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
    assert_eq!(payload(&stack, &card_id).await["model"], "opus");
}

/// At issue no catalog is consulted: a stored value the CLI does not list (one that looks like a
/// flag among them) reaches argv as a single `--model=` token, and the CLI fails the turn in its
/// own words; nothing substitutes another model.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_model_the_cli_rejects_fails_the_turn_in_the_cli_words() {
    let root = Root::new("exit");
    let stack = Stack::boot(&root).await;
    let (_track, card_id) = stack.create_claude_track().await;
    for (at, model) in ["claude-bogus-9-9", "-x"].into_iter().enumerate() {
        let (status, answer) = put_model(
            &stack,
            &card_id,
            json!({"model": model, "reasoning_effort": null}),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{model}: {answer}");
        assert_eq!(answer["unknown_model"], true, "{model}: {answer}");
        let (status, body) = stack.post_input(&card_id, "hello?").await;
        assert_eq!(status, StatusCode::OK, "{model}: {body}");
        let outcome = stack.wait_outcomes(&card_id, at + 1).await[at].clone();
        assert_eq!(outcome["status"], "failed", "{model}: {outcome}");
        let words = format!(
            "There's an issue with the selected model ({model}). It may not exist or you may not \
             have access to it."
        );
        assert!(outcome.to_string().contains(&words), "{model}: {outcome}");
        assert_eq!(spawned_flag(&root, "--model").as_deref(), Some(model));
        let argv = root.read_fake("argv").expect("argv");
        assert!(
            !argv.lines().any(|arg| arg == model),
            "{model} as its own token: {argv}"
        );
    }
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
