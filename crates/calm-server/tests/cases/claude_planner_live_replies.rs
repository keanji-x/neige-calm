//! #1923 S2 P3: a Claude Planner's reply text while it streams, through the production boot, the
//! real session and run loop, and `GET /api/cards/{id}/harness/live`. The fake `claude` replays
//! `p1923_stream.ndjson`, a captured `--include-partial-messages` turn (see the `claude_planner`
//! unit tests), cut where a test needs it.

use std::time::{Duration, Instant};

use axum::http::StatusCode;
use serde_json::{Value, json};

use super::claude_planner_stack_fixture::{Root, Stack};
use super::claude_planner_wiring::wait_file;

const STREAM: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../provider/tests/fixtures/claude_planner_stream/p1923_stream.ndjson"
);
/// The second message's text block, the reply the tests cut.
const REPLY: &str = "msg_REDACTED0028:1";
/// Its first text delta, where the tests cut it.
const CUT_AT: &str = "The secret word is mar";

fn stream_lines() -> Vec<String> {
    std::fs::read_to_string(STREAM)
        .expect("fixture")
        .lines()
        .map(str::to_string)
        .collect()
}

/// The position of the first line containing `needle`.
fn line_of(lines: &[String], needle: &str) -> usize {
    lines
        .iter()
        .position(|line| line.contains(needle))
        .unwrap_or_else(|| panic!("no line has {needle}"))
}

/// The fixture up to and including the reply's first delta, and the rest.
fn cut_mid_reply() -> (Vec<String>, Vec<String>) {
    let mut head = stream_lines();
    let rest = head.split_off(line_of(&head, &format!(r#""text":"{CUT_AT}""#)) + 1);
    (head, rest)
}

fn write_fake(root: &Root, name: &str, lines: &[String]) {
    std::fs::write(root.fake_dir().join(name), lines.join("\n") + "\n").expect("fake input");
}

async fn live(stack: &Stack, card_id: &str) -> Value {
    let (status, body) = stack
        .send("GET", &format!("/api/cards/{card_id}/harness/live"), None)
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    body
}

/// Wait until the live items are `items`; returns the answer.
async fn wait_live_items(stack: &Stack, card_id: &str, items: Value) -> Value {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let body = live(stack, card_id).await;
        if body["items"] == items {
            return body;
        }
        assert!(
            Instant::now() < deadline,
            "live items never became {items}; last {body}"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// The card's transcript rows, oldest first, as `(method, params)`, read through
/// `GET harness/items`.
async fn transcript(stack: &Stack, card_id: &str) -> Vec<(String, Value)> {
    let (status, body) = stack
        .send("GET", &format!("/api/cards/{card_id}/harness/items"), None)
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    body.as_array()
        .expect("rows")
        .iter()
        .map(|row| {
            (
                row["method"].as_str().expect("method").to_owned(),
                serde_json::from_str(row["params"].as_str().expect("params")).expect("json"),
            )
        })
        .collect()
}

/// The transcript's item rows.
async fn item_rows(stack: &Stack, card_id: &str) -> Vec<(String, Value)> {
    let mut rows = transcript(stack, card_id).await;
    rows.retain(|(method, _)| method.starts_with("item/"));
    rows
}

/// The model's items (no user message): `(method, id, type, text)`.
fn model_items(rows: &[(String, Value)]) -> Vec<(String, String, String, Value)> {
    rows.iter()
        .filter(|(_, params)| params["item"]["type"] != "userMessage")
        .map(|(method, params)| {
            let item = &params["item"];
            (
                method.clone(),
                item["id"].as_str().expect("id").to_owned(),
                item["type"].as_str().expect("type").to_owned(),
                item["text"].clone(),
            )
        })
        .collect()
}

fn row(method: &str, id: &str, item_type: &str, text: Value) -> (String, String, String, Value) {
    (method.into(), id.into(), item_type.into(), text)
}

async fn start_turn(stack: &Stack, root: &Root, card_id: &str) {
    let (status, body) = stack.post_input(card_id, "what is the secret word?").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    wait_file(root, "streamed").await;
}

/// Mid-reply the streamed text is served live and not stored; once the turn completes it is stored
/// and no longer live, and the stored items are the turn's records: what the translator stored
/// before the stream, under `<message.id>:<index>` ids.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_streamed_reply_is_live_mid_turn_and_stored_once_complete() {
    let root = Root::new("stream");
    let (head, rest) = cut_mid_reply();
    write_fake(&root, "stream", &head);
    write_fake(&root, "stream-rest", &rest);
    let stack = Stack::boot(&root).await;
    let (_, card_id) = stack.create_claude_track().await;
    let runtime = stack.runtime(&card_id).await;
    start_turn(&stack, &root, &card_id).await;

    let body = wait_live_items(
        &stack,
        &card_id,
        json!([{ "item_id": REPLY, "text": CUT_AT }]),
    )
    .await;
    assert!(body["turn_id"].is_string(), "{body}");
    let mid = model_items(&item_rows(&stack, &card_id).await);
    assert_eq!(
        mid.last(),
        Some(&row("item/started", REPLY, "agentMessage", json!(""))),
        "the reply is started and not completed: {mid:?}"
    );

    std::fs::write(root.fake_dir().join("release"), "").expect("release");
    let outcomes = stack.wait_outcomes(&card_id, 1).await;
    assert_eq!(outcomes[0]["status"], "completed", "{outcomes:?}");
    stack.wait_phase(&runtime.id, "turn_completed").await;
    assert_eq!(
        live(&stack, &card_id).await,
        json!({ "turn_id": null, "items": [] })
    );
    let first = "msg_REDACTED0027";
    let tool = "toolu_01F1VAfHM25psKYSFr8qDMN9";
    assert_eq!(
        model_items(&item_rows(&stack, &card_id).await),
        [
            row(
                "item/started",
                &format!("{first}:0"),
                "reasoning",
                Value::Null
            ),
            row(
                "item/completed",
                &format!("{first}:0"),
                "reasoning",
                Value::Null
            ),
            row(
                "item/started",
                &format!("{first}:1"),
                "agentMessage",
                json!("")
            ),
            row(
                "item/completed",
                &format!("{first}:1"),
                "agentMessage",
                json!("I'll read the file to find the secret word.")
            ),
            row("item/started", tool, "dynamicToolCall", Value::Null),
            row("item/completed", tool, "dynamicToolCall", Value::Null),
            row(
                "item/started",
                "msg_REDACTED0028:0",
                "reasoning",
                Value::Null
            ),
            row(
                "item/completed",
                "msg_REDACTED0028:0",
                "reasoning",
                Value::Null
            ),
            row("item/started", REPLY, "agentMessage", json!("")),
            row(
                "item/completed",
                REPLY,
                "agentMessage",
                json!("The secret word is marmalade.")
            ),
        ]
    );
}

/// An interrupt mid-reply: the text block got deltas and no record, so the run loop's settle hook
/// stores it as one `_partial` completion with the streamed text.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_interrupted_reply_is_stored_once_as_a_partial() {
    let root = Root::new("stream-hold");
    write_fake(&root, "stream", &cut_mid_reply().0);
    let stack = Stack::boot(&root).await;
    let (_, card_id) = stack.create_claude_track().await;
    let runtime = stack.runtime(&card_id).await;
    start_turn(&stack, &root, &card_id).await;
    wait_live_items(
        &stack,
        &card_id,
        json!([{ "item_id": REPLY, "text": CUT_AT }]),
    )
    .await;

    let (status, body) = stack
        .send(
            "POST",
            &format!("/api/cards/{card_id}/planner/interrupt"),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let outcomes = stack.wait_outcomes(&card_id, 1).await;
    assert_eq!(outcomes[0]["status"], "interrupted", "{outcomes:?}");
    stack.wait_phase(&runtime.id, "turn_completed").await;
    let rows = item_rows(&stack, &card_id).await;
    let reply: Vec<&Value> = rows
        .iter()
        .filter(|(method, params)| method == "item/completed" && params["item"]["id"] == REPLY)
        .map(|(_, params)| params)
        .collect();
    assert_eq!(reply.len(), 1, "{rows:?}");
    assert_eq!(reply[0]["_partial"], true);
    assert_eq!(
        reply[0]["item"],
        json!({ "id": REPLY, "type": "agentMessage", "text": CUT_AT })
    );
    assert_eq!(
        rows.iter()
            .filter(|(_, params)| params.get("_partial").is_some())
            .count(),
        1
    );
    assert_eq!(
        live(&stack, &card_id).await,
        json!({ "turn_id": null, "items": [] })
    );
}

/// The reply's text arrives as 300 deltas (the captured delta line, repeated), which the fake holds
/// until the baseline is read: none stores a row, logs an event or writes the snapshot, which every
/// write of `handle_state_json` does.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_claude_delta_writes_no_row_no_event_and_no_snapshot() {
    const DELTAS: usize = 300;
    let root = Root::new("stream");
    let lines = stream_lines();
    let first_delta = line_of(&lines, &format!(r#""text":"{CUT_AT}""#));
    let piece = lines[first_delta].replace(CUT_AT, "x");
    let rest = lines[line_of(&lines, r#""text":"malade."}"#) + 1..].to_vec();
    write_fake(&root, "stream", &lines[..first_delta]);
    write_fake(&root, "stream-burst", &vec![piece; DELTAS]);
    write_fake(&root, "stream-rest", &rest);
    let stack = Stack::boot(&root).await;
    let (_, card_id) = stack.create_claude_track().await;
    let runtime = stack.runtime(&card_id).await;

    let pool = sqlx::SqlitePool::connect(&root.db_url())
        .await
        .expect("pool");
    for statement in [
        "CREATE TABLE test_snapshot_writes (n INTEGER NOT NULL)",
        "INSERT INTO test_snapshot_writes (n) VALUES (0)",
        "CREATE TRIGGER test_count_snapshot_writes AFTER UPDATE OF handle_state_json \
         ON worker_sessions BEGIN UPDATE test_snapshot_writes SET n = n + 1; END",
    ] {
        sqlx::query(statement)
            .execute(&pool)
            .await
            .expect("trigger");
    }
    let count = |sql: &'static str| {
        let pool = pool.clone();
        async move {
            sqlx::query_scalar::<_, i64>(sql)
                .fetch_one(&pool)
                .await
                .expect("count")
        }
    };
    let writes = || count("SELECT n FROM test_snapshot_writes");
    let events = || count("SELECT COUNT(*) FROM events");

    start_turn(&stack, &root, &card_id).await;
    // The reply's start is stored, so everything before the held deltas has been handled.
    let deadline = Instant::now() + Duration::from_secs(20);
    while !model_items(&item_rows(&stack, &card_id).await).contains(&row(
        "item/started",
        REPLY,
        "agentMessage",
        json!(""),
    )) {
        assert!(Instant::now() < deadline, "the reply never started");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let before = (
        writes().await,
        events().await,
        transcript(&stack, &card_id).await.len(),
    );
    std::fs::write(root.fake_dir().join("release-burst"), "").expect("release the deltas");
    wait_live_items(
        &stack,
        &card_id,
        json!([{ "item_id": REPLY, "text": "x".repeat(DELTAS) }]),
    )
    .await;
    let after = (
        writes().await,
        events().await,
        transcript(&stack, &card_id).await.len(),
    );
    assert_eq!((after.1, after.2), (before.1, before.2), "no event, no row");
    assert!(
        after.0 - before.0 <= 1,
        "{DELTAS} deltas wrote the snapshot {} times",
        after.0 - before.0
    );

    std::fs::write(root.fake_dir().join("release"), "").expect("release");
    stack.wait_outcomes(&card_id, 1).await;
    stack.wait_phase(&runtime.id, "turn_completed").await;
}
