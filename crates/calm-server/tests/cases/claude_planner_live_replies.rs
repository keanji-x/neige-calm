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
    // The reply-start row commits before its item-added event. Wait for both before
    // taking the baseline, so that event cannot be mistaken for a delta write.
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
    while !sqlx::query_scalar::<_, bool>(
        "SELECT EXISTS (SELECT 1 FROM events WHERE kind = 'harness.item.added' \
         AND json_extract(payload, '$.worker_session_id') = ?1 \
         AND json_extract(payload, '$.item_uuid') = ?2 \
         AND json_extract(payload, '$.method') = 'item/started')",
    )
    .bind(&runtime.id)
    .bind(REPLY)
    .fetch_one(&pool)
    .await
    .expect("reply-start event")
    {
        assert!(
            Instant::now() < deadline,
            "the reply start was never announced"
        );
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

// Blank canonical creation uses production owners; only native CLI transport is fake.
async fn main_control_blank(stack: &Stack, root: &Root) -> (String, String) {
    let (status, body) = stack
        .send(
            "POST",
            "/api/tracks",
            Some(json!({
                "planner_provider":"claude", "area_id":stack.area().await,
                "title":"", "theme":{"fg":[216,219,226],"bg":[15,20,24]}
            })),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let track = body["id"].as_str().unwrap().to_owned();
    let card = stack
        .repo()
        .cards_by_track(&track)
        .await
        .unwrap()
        .into_iter()
        .find(|c| {
            stack.state.write().verify_role(&c.id) == Some(calm_server::model::CardRole::Planner)
        })
        .unwrap();
    let runtime = stack.runtime(card.id.as_str()).await;
    let frame = runtime.handle_state_json.as_ref().unwrap();
    assert_eq!(frame["canonical_inputs"]["pending"], json!([]));
    assert_eq!(frame["canonical_inputs"]["active"], Value::Null);
    assert_eq!(runtime.thread_id, None);
    assert!(stack.state.harness.get(&runtime.id).is_none());
    assert!(root.read_fake("spawns").is_none());
    eprintln!("blank canonical runtime={} frame={frame}", runtime.id);
    (track, card.id.to_string())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn p1_workspace_upload_blank_canonical_claude_is_created() {
    use axum::{body::Body, http::Request};
    use http_body_util::BodyExt;
    use tower::ServiceExt;
    let _ = tracing_subscriber::fmt()
        .with_env_filter("error")
        .with_test_writer()
        .try_init();
    let root = Root::new("exit");
    let stack = Stack::boot(&root).await;
    let (track, card) = main_control_blank(&stack, &root).await;
    let workspace = stack
        .repo()
        .track_get(&track)
        .await
        .unwrap()
        .unwrap()
        .workspace;
    assert_eq!(
        workspace.kind,
        calm_server::model::TrackWorkspaceKind::Managed
    );
    assert!(
        !std::path::Path::new(&workspace.path).exists(),
        "workspace must still be lazy"
    );
    let image = vec![
        137, 80, 78, 71, 13, 10, 26, 10, 0, 0, 0, 13, 73, 72, 68, 82, 0, 0, 0, 1, 0, 0, 0, 1, 8, 6,
        0, 0, 0, 31, 21, 196, 137, 0, 0, 0, 13, 73, 68, 65, 84, 120, 156, 99, 248, 207, 192, 240,
        31, 0, 5, 0, 1, 255, 137, 153, 61, 29, 0, 0, 0, 0, 73, 69, 78, 68, 174, 66, 96, 130,
    ];
    let before = stack.runtime(&card).await;
    let response = stack
        .app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/api/cards/{card}/planner/attachments"))
                .header("x-calm-actor", "user")
                .header("content-type", "image/png")
                .body(Body::from(image.clone()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    eprintln!(
        "P1 upload status={status} body={}",
        String::from_utf8_lossy(&bytes)
    );
    assert_eq!(
        status,
        StatusCode::CREATED,
        "{}",
        String::from_utf8_lossy(&bytes)
    );

    let stored: Value = serde_json::from_slice(&bytes).unwrap();
    let response = stack
        .app
        .clone()
        .oneshot(
            Request::builder()
                .uri(stored["url"].as_str().unwrap())
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response
            .into_body()
            .collect()
            .await
            .unwrap()
            .to_bytes()
            .as_ref(),
        image
    );
    let after = stack.runtime(&card).await;
    assert_eq!(after.id, before.id);
    assert_eq!(after.handle_state_json, before.handle_state_json);
    assert_eq!(after.thread_id, None);
    assert!(stack.state.harness.get(&after.id).is_none());
    assert!(root.read_fake("spawns").is_none());
    stack.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn p2_terminal_canonical_native_interrupted_end_is_read() {
    let root = Root::new("stream-hold");
    write_fake(&root, "stream", &cut_mid_reply().0);
    let stack = Stack::boot(&root).await;
    let (_, card) = main_control_blank(&stack, &root).await;
    let (status, body) = stack.post_input(&card, "what is the secret word?").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    wait_file(&root, "streamed").await;
    let deadline = Instant::now() + Duration::from_secs(20);
    let live = loop {
        let (status, body) = stack
            .send(
                "GET",
                &format!("/api/cards/{card}/harness/live?contract=conversation_v1"),
                None,
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        if body.to_string().contains(CUT_AT) {
            break body;
        }
        assert!(
            Instant::now() < deadline,
            "native live reply not observed: {body}"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    };
    eprintln!("P2 live={live}");
    std::fs::write(root.fake_dir().join("hold-terminal"), "").unwrap();
    let (status, body) = stack
        .send(
            "POST",
            &format!("/api/cards/{card}/planner/interrupt"),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["stopped"], true);
    wait_file(&root, "terminal-held").await;
    let (before_status, before_end) = stack
        .send(
            "GET",
            &format!("/api/cards/{card}/planner/run?contract=conversation_v1"),
            None,
        )
        .await;
    std::fs::write(root.fake_dir().join("release-terminal"), "").unwrap();
    let outcomes = stack.wait_outcomes(&card, 1).await;
    eprintln!("P2 persisted native END={outcomes:?}");
    assert_eq!(outcomes[0]["status"], "interrupted", "{outcomes:?}");
    let runtime = stack.runtime(&card).await;
    stack.wait_provider_clear(&runtime.id).await;
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let current = stack.runtime(&card).await;
        let frame = current.handle_state_json.unwrap();
        if frame["phase"] == "turn_completed" && frame["canonical_inputs"]["active"].is_null() {
            eprintln!("P2 committed END frame={frame}");
            break;
        }
        assert!(Instant::now() < deadline, "END not yet consumed: {frame}");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let (status, run) = stack
        .send(
            "GET",
            &format!("/api/cards/{card}/planner/run?contract=conversation_v1"),
            None,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{run}");
    stack.shutdown().await;
    assert_eq!(before_status, StatusCode::OK, "{before_end}");
    assert!(
        before_end.get("terminal_outcome").is_some(),
        "required nullable fact: {before_end}"
    );
    assert_eq!(
        before_end["terminal_outcome"],
        Value::Null,
        "ACK cannot fabricate END"
    );
    assert_eq!(
        run["terminal_outcome"]["status"], "interrupted",
        "qualified END missing: {run}"
    );
    assert_eq!(run["terminal_outcome"]["worker_session_id"], runtime.id);
    assert_eq!(run["terminal_outcome"]["card_id"], card);
    assert_eq!(run["terminal_outcome"]["turn_id"], outcomes[0]["id"]);
}
