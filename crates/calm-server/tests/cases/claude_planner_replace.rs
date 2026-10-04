//! `POST /api/cards/{id}/planner/input` with `replaces_turn` on a Claude Planner (#1923, #2043),
//! through the production boot, routes and session against the fake `claude`: the cut is dry-run
//! before anything changes and rides the replacing spawn as `--resume-session-at` /
//! `--resume-drops-turn`.

use axum::http::StatusCode;
use serde_json::{Value, json};

use super::claude_planner_stack_fixture::{Root, Stack};

async fn replace(stack: &Stack, card_id: &str, turn_id: &str, text: &str) -> (StatusCode, Value) {
    stack
        .send_input(card_id, json!({ "text": text, "replaces_turn": turn_id }))
        .await
}

/// `(id, turn_id, item_type, params)` of every transcript row the card's read route serves.
async fn transcript(
    stack: &Stack,
    card_id: &str,
) -> Vec<(i64, Option<String>, Option<String>, String)> {
    let (status, rows) = stack
        .send("GET", &format!("/api/cards/{card_id}/harness/items"), None)
        .await;
    assert_eq!(status, StatusCode::OK, "{rows}");
    rows.as_array()
        .expect("an array of rows")
        .iter()
        .map(|row| {
            let text = |key: &str| row[key].as_str().map(str::to_string);
            (
                row["id"].as_i64().unwrap(),
                text("turn_id"),
                text("item_type"),
                text("params").unwrap(),
            )
        })
        .collect()
}

/// `(id, turn_id)` of every row of the card, oldest first.
async fn rows(stack: &Stack, card_id: &str) -> Vec<(i64, Option<String>)> {
    transcript(stack, card_id)
        .await
        .into_iter()
        .map(|(id, turn, _, _)| (id, turn))
        .collect()
}

async fn stored_snapshot(stack: &Stack, card_id: &str) -> Value {
    stack
        .runtime(card_id)
        .await
        .handle_state_json
        .expect("a persisted snapshot")
}

/// The dashed form of the client id the user message of `turn_id` was projected under.
async fn prompt_line_uuid(stack: &Stack, card_id: &str, turn_id: &str) -> String {
    let (_, _, _, params) = transcript(stack, card_id)
        .await
        .into_iter()
        .find(|(_, turn, item_type, _)| {
            turn.as_deref() == Some(turn_id) && item_type.as_deref() == Some("userMessage")
        })
        .expect("the turn's user message");
    let params: Value = serde_json::from_str(&params).unwrap();
    uuid::Uuid::parse_str(params["item"]["clientId"].as_str().unwrap())
        .unwrap()
        .to_string()
}

/// Two finished turns under `exit-trailing`; returns the card and both outcomes.
async fn two_turns(root: &Root, stack: &Stack) -> (String, Value, Value) {
    let (_track_id, card_id) = stack.create_claude_track().await;
    let (a, _) = stack
        .run_turn(root, &card_id, "exit-trailing", "first")
        .await;
    let (b, _) = stack
        .run_turn(root, &card_id, "exit-trailing", "second")
        .await;
    (card_id, a, b)
}

fn arg_after<'a>(argv: &'a [&'a str], flag: &str) -> &'a str {
    let at = argv.iter().position(|arg| *arg == flag).expect(flag);
    argv[at + 1]
}

/// A crash right after the commit is the lost-answer case: the restarted conversation holds the
/// cut and the message, and its first turn applies the one and sends the other.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_cut_is_checked_then_rides_the_replacing_spawn_and_survives_a_restart() {
    let root = Root::new("exit-trailing");
    let stack = Stack::boot(&root).await;
    let (card_id, a, b) = two_turns(&root, &stack).await;
    let turn_b = b["id"].as_str().unwrap().to_string();
    let anchor = a["lastRecordUuid"]
        .as_str()
        .expect("the outcome names its last record");
    assert_ne!(
        Some(anchor),
        b["lastRecordUuid"].as_str(),
        "premise: one per spawn"
    );
    let drops = prompt_line_uuid(&stack, &card_id, &turn_b).await;
    let runtime = stack.runtime(&card_id).await;
    let thread = runtime.thread_id.clone().expect("thread");
    stack.harness(&runtime.id).pause_issuance_for_dev();
    let spawns_before = root.read_fake("spawns").unwrap_or_default().lines().count();

    let (status, body) = replace(&stack, &card_id, &turn_b, "edited").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body["entry_id"].is_string(), "{body}");
    let check: Vec<String> = root
        .read_fake("check-argv")
        .expect("the dry run ran")
        .lines()
        .map(str::to_string)
        .collect();
    assert_eq!(
        check,
        [
            "-p",
            "--input-format",
            "stream-json",
            "--output-format",
            "stream-json",
            "--verbose",
            "--resume",
            &thread,
            &format!("--resume-session-at={anchor}"),
            &format!("--resume-drops-turn={drops}"),
            "--setting-sources",
            "project",
            "--disable-slash-commands",
            "--strict-mcp-config",
        ]
    );
    assert!(
        rows(&stack, &card_id)
            .await
            .iter()
            .all(|(_, turn)| turn.as_deref() != Some(&turn_b)),
        "B's rows are gone"
    );
    let stored = stored_snapshot(&stack, &card_id).await;
    assert_eq!(
        stored["pending_rewind"],
        json!({ "provider": "claude", "resume_at": anchor, "drops_turn": drops })
    );
    assert_eq!(stored["last_turn_id"], a["id"]);
    assert!(
        stored["pending_queue"].to_string().contains("edited"),
        "the message is in the same snapshot: {stored}"
    );

    stack.shutdown().await;
    let stack = Stack::boot(&root).await;
    let outcomes = stack.wait_outcomes(&card_id, 2).await;
    assert_eq!(outcomes[0], a, "B's outcome stays gone: {outcomes:?}");
    assert_eq!(outcomes[1]["status"], json!("completed"), "{outcomes:?}");
    assert_ne!(outcomes[1]["id"], b["id"]);
    assert_eq!(
        root.read_fake("spawns").unwrap_or_default().lines().count(),
        spawns_before + 1,
        "one spawn: the replacing turn"
    );
    let argv = root.read_fake("argv").expect("argv");
    let argv: Vec<&str> = argv.lines().collect();
    let resume = argv
        .iter()
        .position(|arg| *arg == "--resume")
        .expect("resumed");
    assert_eq!(argv[resume + 1], thread);
    assert_eq!(argv[resume + 2], format!("--resume-session-at={anchor}"));
    assert_eq!(argv[resume + 3], format!("--resume-drops-turn={drops}"));
    assert!(arg_after(&argv, "--mcp-config").contains("\"neige\""));
    let runtime = stack.runtime(&card_id).await;
    stack.wait_phase(&runtime.id, "turn_completed").await;
    assert!(
        stored_snapshot(&stack, &card_id).await["pending_rewind"].is_null(),
        "consumed by the start"
    );
    stack.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_first_turn_of_a_claude_conversation_is_refused_and_nothing_changes() {
    let root = Root::new("exit-trailing");
    let stack = Stack::boot(&root).await;
    let (_track_id, card_id) = stack.create_claude_track().await;
    let (a, _) = stack
        .run_turn(&root, &card_id, "exit-trailing", "first")
        .await;
    let rows_before = rows(&stack, &card_id).await;
    let snapshot_before = stored_snapshot(&stack, &card_id).await;

    let (status, body) = replace(&stack, &card_id, a["id"].as_str().unwrap(), "edited").await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(
        body["code"],
        json!("planner_turn_not_replaceable"),
        "{body}"
    );
    assert!(
        body["error"].as_str().unwrap().contains("first message"),
        "{body}"
    );
    assert!(
        root.read_fake("checks").is_none(),
        "no dry run for a refusal"
    );
    assert_eq!(rows(&stack, &card_id).await, rows_before);
    assert_eq!(stored_snapshot(&stack, &card_id).await, snapshot_before);
    stack.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_cut_the_cli_refuses_is_a_409_with_its_reason_and_nothing_changes() {
    let root = Root::new("exit-trailing");
    let stack = Stack::boot(&root).await;
    let (card_id, _a, b) = two_turns(&root, &stack).await;
    let rows_before = rows(&stack, &card_id).await;
    let snapshot_before = stored_snapshot(&stack, &card_id).await;
    std::fs::write(
        root.fake_dir().join("check"),
        "Resume rejected by --resume-drops-turn: the range holds another turn",
    )
    .unwrap();

    let (status, body) = replace(&stack, &card_id, b["id"].as_str().unwrap(), "edited").await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(
        body["code"],
        json!("planner_turn_not_replaceable"),
        "{body}"
    );
    assert!(
        body["error"]
            .as_str()
            .unwrap()
            .contains("Resume rejected by --resume-drops-turn: the range holds another turn"),
        "{body}"
    );
    assert_eq!(root.read_fake("checks").unwrap().lines().count(), 1);
    assert_eq!(rows(&stack, &card_id).await, rows_before);
    assert_eq!(stored_snapshot(&stack, &card_id).await, snapshot_before);
    stack.shutdown().await;
}
