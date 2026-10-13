//! `POST /api/cards/{id}/planner/compact` on a Claude Planner, through the production boot, routes,
//! harness and session against the fake `claude`: one `/compact` spawn that the harness runs as a
//! compaction and the transcript never shows, then a resumed turn and an edit across it.

use axum::http::StatusCode;
use serde_json::{Value, json};

use super::claude_planner_stack_fixture::{Root, Stack};

async fn compact(stack: &Stack, card_id: &str) -> (StatusCode, Value) {
    stack
        .send(
            "POST",
            &format!("/api/cards/{card_id}/planner/compact"),
            None,
        )
        .await
}

/// Every transcript row the card's read route serves, oldest first.
async fn rows(stack: &Stack, card_id: &str) -> Value {
    let (status, rows) = stack
        .send("GET", &format!("/api/cards/{card_id}/harness/items"), None)
        .await;
    assert_eq!(status, StatusCode::OK, "{rows}");
    assert!(rows.is_array(), "{rows}");
    rows
}

async fn replace(stack: &Stack, card_id: &str, turn_id: &str, text: &str) -> (StatusCode, Value) {
    stack
        .send_input(card_id, json!({ "text": text, "replaces_turn": turn_id }))
        .await
}

async fn stored_snapshot(stack: &Stack, card_id: &str) -> Value {
    stack
        .runtime(card_id)
        .await
        .handle_state_json
        .expect("a persisted snapshot")
}

fn spawns(root: &Root) -> usize {
    root.read_fake("spawns").unwrap_or_default().lines().count()
}

fn argv(root: &Root) -> Vec<String> {
    root.read_fake("argv")
        .expect("argv")
        .lines()
        .map(str::to_string)
        .collect()
}

fn has(argv: &[String], arg: &str) -> bool {
    argv.iter().any(|a| a == arg)
}

/// Two finished turns under `exit-trailing`, then a compaction the harness saw complete; returns
/// the card and both turns' outcomes.
async fn two_turns_then_compact(root: &Root, stack: &Stack) -> (String, Value, Value) {
    let (_track_id, card_id) = stack.create_claude_track().await;
    let (a, _) = stack
        .run_turn(root, &card_id, "exit-trailing", "first")
        .await;
    let (b, _) = stack
        .run_turn(root, &card_id, "exit-trailing", "second")
        .await;
    root.set_scenario("compact");
    let (status, body) = compact(stack, &card_id).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["started"], json!(true), "{body}");
    let runtime = stack.runtime(&card_id).await;
    stack.wait_phase(&runtime.id, "turn_completed").await;
    (card_id, a, b)
}

/// The compaction is one `--resume` spawn whose one line is `/compact`, run with the CLI's
/// commands; it completes through the harness and leaves the person's last turn the last one:
/// no outcome, no transcript row, the same `last_turn_id`.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn compact_runs_one_slash_compact_spawn_and_records_no_user_turn() {
    let root = Root::new("exit-trailing");
    let stack = Stack::boot(&root).await;
    let (_track_id, card_id) = stack.create_claude_track().await;
    stack
        .run_turn(&root, &card_id, "exit-trailing", "first")
        .await;
    let (b, _) = stack
        .run_turn(&root, &card_id, "exit-trailing", "second")
        .await;
    let runtime = stack.runtime(&card_id).await;
    let thread = runtime.thread_id.clone().expect("thread");
    let rows_before = rows(&stack, &card_id).await;
    let spawns_before = spawns(&root);
    root.set_scenario("compact");

    let (status, body) = compact(&stack, &card_id).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["started"], json!(true), "{body}");
    stack.wait_phase(&runtime.id, "turn_completed").await;

    assert_eq!(spawns(&root), spawns_before + 1, "one spawn");
    let stdin = root.read_fake("stdin").expect("stdin");
    let line: Value = serde_json::from_str(stdin.lines().last().unwrap()).unwrap();
    assert_eq!(
        line["message"]["content"],
        json!([{"type": "text", "text": "/compact"}])
    );
    assert_eq!(line["session_id"], json!(thread));
    let argv = argv(&root);
    let resume = argv.iter().position(|a| a == "--resume").expect("resumed");
    assert_eq!(argv[resume + 1], thread);
    assert!(
        !has(&argv, "--disable-slash-commands"),
        "/compact needs the commands: {argv:?}"
    );
    assert!(!argv.iter().any(|a| a.starts_with("--resume-session-at")));

    assert_eq!(
        rows(&stack, &card_id).await,
        rows_before,
        "the compaction is no turn of the transcript"
    );
    let snapshot = stored_snapshot(&stack, &card_id).await;
    assert_eq!(snapshot["last_turn_id"], b["id"], "{snapshot}");
    assert_eq!(snapshot["phase"], json!("turn_completed"));
    assert!(root.instructions_files().is_empty());
    stack.shutdown().await;
}

/// After a compaction the next turn resumes the session as any turn does. Editing it cuts at the
/// last record of the person's turn before it, which precedes the boundary: the CLI's dry run
/// judges that cut, its refusal changes nothing, and an accepted cut waits for the replacing spawn.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn after_compact_a_turn_resumes_and_an_edit_across_it_is_judged_by_the_dry_run() {
    let root = Root::new("exit-trailing");
    let stack = Stack::boot(&root).await;
    let (card_id, _a, b) = two_turns_then_compact(&root, &stack).await;
    let thread = stack.runtime(&card_id).await.thread_id.expect("thread");

    let (c, _) = stack
        .run_turn(&root, &card_id, "exit-trailing", "third")
        .await;
    assert_eq!(c["status"], json!("completed"), "{c}");
    let argv = argv(&root);
    let resume = argv.iter().position(|a| a == "--resume").expect("resumed");
    assert_eq!(argv[resume + 1], thread);
    assert!(!argv.iter().any(|a| a.starts_with("--resume-session-at")));
    assert!(has(&argv, "--disable-slash-commands"), "a turn runs none");
    let outcomes = stack.outcomes(&card_id).await;
    assert_eq!(outcomes.len(), 3, "a, b and c: {outcomes:?}");
    assert_eq!(outcomes[2], c);

    let anchor = b["lastRecordUuid"]
        .as_str()
        .expect("b names its last record");
    let rows_before = rows(&stack, &card_id).await;
    let snapshot_before = stored_snapshot(&stack, &card_id).await;
    std::fs::write(
        root.fake_dir().join("check"),
        "Resume rejected by --resume-drops-turn: the range holds a compaction",
    )
    .unwrap();
    let turn_c = c["id"].as_str().unwrap();
    let (status, body) = replace(&stack, &card_id, turn_c, "edited").await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert!(
        body["error"]
            .as_str()
            .unwrap()
            .contains("the range holds a compaction"),
        "{body}"
    );
    let check = root.read_fake("check-argv").expect("the dry run ran");
    assert!(
        check
            .lines()
            .any(|arg| arg == format!("--resume-session-at={anchor}")),
        "the cut lands on b's last record: {check}"
    );
    assert_eq!(rows(&stack, &card_id).await, rows_before);
    assert_eq!(stored_snapshot(&stack, &card_id).await, snapshot_before);

    root.remove_fake("check");
    let runtime = stack.runtime(&card_id).await;
    stack.harness(&runtime.id).pause_issuance_for_dev();
    let (status, body) = replace(&stack, &card_id, turn_c, "edited again").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let stored = stored_snapshot(&stack, &card_id).await;
    assert_eq!(stored["pending_rewind"]["resume_at"], json!(anchor));
    assert_eq!(stored["last_turn_id"], b["id"]);
    stack.shutdown().await;
}

/// A `/compact` the CLI answered without a boundary did not compact: the compaction fails and the
/// harness wedges, as a failed Codex compaction does, and still nothing reaches the transcript.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_compact_without_a_boundary_wedges_the_conversation() {
    let root = Root::new("exit-trailing");
    let stack = Stack::boot(&root).await;
    let (_track_id, card_id) = stack.create_claude_track().await;
    stack
        .run_turn(&root, &card_id, "exit-trailing", "first")
        .await;
    let rows_before = rows(&stack, &card_id).await;
    root.set_scenario("compact-no-boundary");

    let (status, body) = compact(&stack, &card_id).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let runtime = stack.runtime(&card_id).await;
    stack.wait_phase(&runtime.id, "wedged").await;
    assert_eq!(rows(&stack, &card_id).await, rows_before);
    stack.shutdown().await;
}

/// A refusal before anything is spawned (here an unpinned CLI version) is a 400 that leaves the
/// conversation as it was, as Codex's refusal does.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_compact_refused_before_spawning_leaves_the_conversation_as_it_was() {
    let root = Root::new("exit-trailing");
    let stack = Stack::boot(&root).await;
    let (_track_id, card_id) = stack.create_claude_track().await;
    stack
        .run_turn(&root, &card_id, "exit-trailing", "first")
        .await;
    let snapshot_before = stored_snapshot(&stack, &card_id).await;
    let spawns_before = spawns(&root);
    std::fs::write(root.fake_dir().join("version"), "2.1.281").unwrap();

    let (status, body) = compact(&stack, &card_id).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert!(
        body["error"]
            .as_str()
            .unwrap()
            .contains("Claude will not compact this conversation"),
        "{body}"
    );
    assert_eq!(spawns(&root), spawns_before, "nothing spawned");
    let snapshot = stored_snapshot(&stack, &card_id).await;
    assert_eq!(snapshot["phase"], snapshot_before["phase"], "{snapshot}");
    assert_eq!(snapshot["last_turn_id"], snapshot_before["last_turn_id"]);
    stack.shutdown().await;
}
