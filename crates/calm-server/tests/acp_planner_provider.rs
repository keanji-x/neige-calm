//! Managed ACP acceptance through the production boot, REST routes and Harness.
#[allow(dead_code)]
#[path = "cases/claude_planner_session_fixture.rs"]
mod claude_planner_session_fixture;
#[allow(dead_code)]
#[path = "cases/claude_planner_stack_fixture.rs"]
mod stack_fixture;

use axum::http::StatusCode;
use serde_json::{Value, json};
use stack_fixture::{Root, Stack};
use std::time::Duration;

const PEER: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/fixtures/acp_planner/agent.py"
);
fn requests(root: &Root, method: &str) -> Vec<Value> {
    std::fs::read_to_string(root.path().join("requests.jsonl"))
        .unwrap_or_default()
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        .filter(|frame| frame["method"] == method)
        .collect()
}
async fn boot(root: &Root) -> Stack {
    std::fs::write(root.path().join("scenario"), "reply").unwrap();
    let config_path = root.path().join("acp.json");
    std::fs::write(&config_path,json!({"agents":[{"provider":"opencode","command":"/usr/bin/python3","args":["-u",PEER],"env":{"ACP_FIXTURE_ROOT":root.path()},"expected_agent_name":"Fixture ACP","expected_agent_version":"1"}]}).to_string()).unwrap();
    let mut config = root.config(false);
    config.acp_planner_config = Some(config_path);
    Stack::boot_config(&config).await
}
async fn create(stack: &Stack) -> (String, String) {
    stack
        .create_claude_track_with(json!({"planner_provider":"opencode","title":"Managed ACP"}))
        .await
}
async fn turn(stack: &Stack, card: &str, text: &str, n: usize) -> Value {
    let (status, body) = stack.post_input(card, text).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let outcome = stack.wait_outcomes(card, n).await[n - 1].clone();
    let runtime = stack.runtime(card).await;
    stack.wait_phase(&runtime.id, "turn_completed").await;
    outcome
}
async fn wait_file(root: &Root, name: &str) {
    wait_file_within(root, name, Duration::from_secs(20)).await;
}
async fn wait_file_within(root: &Root, name: &str, budget: Duration) {
    tokio::time::timeout(budget, async {
        while !root.path().join(name).exists() {
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .expect("peer file");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn acp_runs_two_turns_and_loads_the_same_native_session_after_restart() {
    let root = Root::new("unused");
    let stack = boot(&root).await;
    let (track, card) = create(&stack).await;
    let runtime = stack.runtime(&card).await;
    assert_eq!(
        runtime.agent_provider,
        Some(calm_server::session_projection_repo::AgentProvider::OpenCode)
    );
    assert!(runtime.session_id.is_none());
    assert!(requests(&root, "session/prompt").is_empty());
    assert_eq!(
        turn(&stack, &card, "first explicit input", 1).await["status"],
        "completed"
    );
    let native = stack
        .runtime(&card)
        .await
        .session_id
        .expect("native binding");
    let rows =
        claude_planner_session_fixture::card_rows(stack.repo(), &card, "item/completed").await;
    assert!(
        rows.iter()
            .any(|row| row["item"]["text"] == "reply: User says:\nfirst explicit input"),
        "{rows:?}"
    );
    assert!(
        rows.iter()
            .any(|row| row["item"]["result"]["content"][0]["text"] == "native tool output"),
        "{rows:?}"
    );
    assert_eq!(
        turn(&stack, &card, "second explicit input", 2).await["status"],
        "completed"
    );
    assert_eq!(requests(&root, "session/prompt").len(), 2);
    assert_eq!(requests(&root, "session/new").len(), 1);
    stack.shutdown().await;
    let stack = boot(&root).await;
    assert_eq!(
        stack.runtime(&card).await.session_id.as_deref(),
        Some(native.as_str())
    );
    assert_eq!(
        requests(&root, "session/prompt").len(),
        2,
        "boot and replay cannot submit"
    );
    assert_eq!(
        turn(&stack, &card, "after restart", 3).await["status"],
        "completed"
    );
    assert_eq!(requests(&root, "session/prompt").len(), 3);
    let rows =
        claude_planner_session_fixture::card_rows(stack.repo(), &card, "item/completed").await;
    assert!(
        rows.iter()
            .filter_map(|row| row["item"]["text"].as_str())
            .all(|text| !text.contains("historic reply")),
        "loaded history must not be attributed to a fresh turn: {rows:?}"
    );
    assert!(
        requests(&root, "session/load")
            .iter()
            .all(|request| request["params"]["sessionId"] == native)
    );
    let (status, body) = stack
        .send("DELETE", &format!("/api/tracks/{track}"), None)
        .await;
    assert!(status.is_success(), "{status}: {body}");
    stack.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn acp_permission_requests_use_never_without_creating_asks() {
    let root = Root::new("unused");
    let stack = boot(&root).await;
    let (track, card) = create(&stack).await;
    std::fs::write(root.path().join("scenario"), "permission").unwrap();
    assert_eq!(
        turn(&stack, &card, "request permission", 1).await["status"],
        "interrupted"
    );
    wait_file(&root, "permission-reply.json").await;
    let reply: Value = serde_json::from_str(
        &std::fs::read_to_string(root.path().join("permission-reply.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(reply["result"]["outcome"], json!({"outcome":"cancelled"}));
    assert!(
        stack
            .repo()
            .events_for_track(&track, &["ask.requested"], None)
            .await
            .unwrap()
            .is_empty()
    );
    assert_eq!(requests(&root, "session/prompt").len(), 1);
    stack.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn acp_lost_prompt_response_stays_fenced_after_restart_without_resend() {
    let root = Root::new("unused");
    let stack = boot(&root).await;
    let (_, card) = create(&stack).await;
    std::fs::write(root.path().join("scenario"), "lost").unwrap();
    let outcome = turn(&stack, &card, "perform once", 1).await;
    assert_eq!(outcome["status"], "failed");
    assert!(
        outcome["error"]["message"]
            .as_str()
            .unwrap()
            .contains("unknown")
    );
    assert_eq!(requests(&root, "session/prompt").len(), 1);
    stack.shutdown().await;
    let stack = boot(&root).await;
    let (status, body) = stack.post_input(&card, "later input").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let runtime = stack.runtime(&card).await;
    let handle = stack.harness(&runtime.id);
    tokio::time::timeout(Duration::from_secs(20), async {
        while handle.refused_issuances_for_test() == 0 {
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .expect("the production issuance path must actually hit the durable fence");
    assert_eq!(
        requests(&root, "session/prompt").len(),
        1,
        "unknown admission cannot be retried"
    );
    let runtime = stack.runtime(&card).await;
    let pool = stack.repo().sqlite_pool().unwrap();
    assert!(
        calm_server::db::sqlite::acp_submission_unresolved(&pool, &runtime.id)
            .await
            .unwrap()
    );
    stack.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn acp_uses_declared_configuration_keys_and_authenticates_mcp() {
    let root = Root::new("unused");
    let stack = boot(&root).await;
    let (_, card) = create(&stack).await;
    std::fs::write(root.path().join("scenario"), "mcp").unwrap();
    assert_eq!(turn(&stack, &card, "first", 1).await["status"], "completed");
    let reply: Value =
        serde_json::from_str(&std::fs::read_to_string(root.path().join("mcp-reply.json")).unwrap())
            .unwrap();
    assert!(reply.get("error").is_none(), "{reply}");
    let (status, catalog) = stack
        .send("GET", &format!("/api/models?card_id={card}"), None)
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(catalog["default_source"], "acp_session");
    assert!(
        catalog["models"]
            .as_array()
            .unwrap()
            .iter()
            .any(|model| model["model"] == "fixture/model-b")
    );
    let (status, body) = stack
        .send(
            "PUT",
            &format!("/api/cards/{card}/planner/model"),
            Some(json!({"model":"fixture/model-b","reasoning_effort":"deep"})),
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        turn(&stack, &card, "selected", 2).await["status"],
        "completed"
    );
    let changes = requests(&root, "session/set_config_option");
    assert_eq!(changes.len(), 2);
    assert_eq!(changes[0]["params"]["configId"], "declared-model-key");
    assert_eq!(changes[1]["params"]["configId"], "declared-effort-key");
    for line in std::fs::read_to_string(root.path().join("environment.jsonl"))
        .unwrap()
        .lines()
    {
        let presence: Value = serde_json::from_str(line).unwrap();
        assert_eq!(presence["NEIGE_MCP_DAEMON_TOKEN"], false);
        assert_eq!(presence["NEIGE_MCP_TOKEN"], false);
        assert_eq!(presence["ACP_AMBIENT_SENTINEL"], false);
    }
    stack.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn acp_registry_miss_recovers_without_codex_readiness() {
    let root = Root::new("unused");
    let stack = boot(&root).await;
    let (_, card) = create(&stack).await;
    assert_eq!(
        turn(&stack, &card, "before registry miss", 1).await["status"],
        "completed"
    );
    let runtime = stack.runtime(&card).await;
    let handle = stack.state.harness.remove(&runtime.id).expect("registered");
    handle.shutdown().await.unwrap();
    assert_eq!(
        turn(&stack, &card, "after registry miss", 2).await["status"],
        "completed"
    );
    assert_eq!(requests(&root, "session/prompt").len(), 2);
    stack.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn acp_recovery_records_unknown_for_a_dispatched_turn_with_no_pending_queue() {
    let root = Root::new("unused");
    let stack = boot(&root).await;
    let (_, card) = create(&stack).await;
    let outcome = turn(&stack, &card, "dispatched checkpoint", 1).await;
    let runtime = stack.runtime(&card).await;
    let pool = stack.repo().sqlite_pool().unwrap();
    let mut snapshot = stack.harness(&runtime.id).snapshot().await;
    stack.shutdown().await;
    // Restore the durable state at the real post-dispatch/pre-settlement crash checkpoint.
    snapshot.phase = calm_server::harness::HarnessPhaseTag::TurnRunning;
    assert!(snapshot.pending_entries().is_empty());
    let mut tx = calm_server::db::sqlite::begin_immediate_tx(&pool)
        .await
        .unwrap();
    sqlx::query(
        "UPDATE acp_submissions SET state='sending',outcome_json=NULL WHERE worker_session_id=?1",
    )
    .bind(&runtime.id)
    .execute(&mut *tx)
    .await
    .unwrap();
    delete_checkpoint_rows(&mut tx, &card, "turn/completed").await;
    sqlx::query("UPDATE worker_sessions SET handle_state_json=?2,active_turn_id=?3 WHERE id=?1")
        .bind(&runtime.id)
        .bind(serde_json::to_string(&snapshot).unwrap())
        .bind(outcome["id"].as_str().unwrap())
        .execute(&mut *tx)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    let stack = boot(&root).await;
    let outcomes = stack.wait_outcomes(&card, 1).await;
    assert_eq!(outcomes[0]["id"], outcome["id"]);
    assert_eq!(outcomes[0]["status"], "failed");
    assert!(
        outcomes[0]["error"]["message"]
            .as_str()
            .unwrap()
            .contains("unknown")
    );
    assert_eq!(
        requests(&root, "session/prompt").len(),
        1,
        "recovery cannot resend"
    );
    assert!(
        calm_server::db::sqlite::acp_submission_unresolved(&pool, &runtime.id)
            .await
            .unwrap()
    );
    stack.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn acp_reset_after_dispatch_checkpoint_transfers_only_later_input() {
    let root = Root::new("unused");
    let stack = boot(&root).await;
    let (track, card) = create(&stack).await;
    std::fs::write(root.path().join("scenario"), "checkpoint").unwrap();
    let (status, body) = stack.post_input(&card, "original operational input").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    wait_file(&root, "setup-checkpoint").await;
    let runtime = stack.runtime(&card).await;
    let mut checkpoint = calm_server::harness::HarnessSnapshot::from_value_strict(
        runtime.handle_state_json.clone().unwrap(),
    );
    assert_eq!(
        checkpoint.phase,
        calm_server::harness::HarnessPhaseTag::IssuingTurn
    );
    assert!(!checkpoint.pending_entries().is_empty());
    std::fs::write(root.path().join("release-setup"), "").unwrap();
    stack.wait_outcomes(&card, 1).await;
    let pool = stack.repo().sqlite_pool().unwrap();
    stack.shutdown().await;
    let mut entries = checkpoint.pending_entries();
    entries.push(calm_server::harness::QueueEntry::user_message(
        "later input".into(),
        None,
        Vec::new(),
    ));
    checkpoint.set_pending_entries(entries);
    sqlx::query("UPDATE worker_sessions SET handle_state_json=?2 WHERE id=?1")
        .bind(&runtime.id)
        .bind(serde_json::to_string(&checkpoint).unwrap())
        .execute(&pool)
        .await
        .unwrap();
    std::fs::write(root.path().join("scenario"), "reply").unwrap();
    let stack = boot(&root).await;
    let queued = stack
        .harness(&runtime.id)
        .snapshot()
        .await
        .pending_entries();
    assert!(queued.iter().all(|entry|!matches!(entry.observation(),calm_server::harness::Observation::UserMessage{text} if text=="original operational input")));
    assert!(queued.iter().any(|entry|matches!(entry.observation(),calm_server::harness::Observation::UserMessage{text} if text=="later input")));
    let (status, body) = stack
        .send(
            "POST",
            &format!("/api/cards/{card}/planner/reset"),
            Some(json!({})),
        )
        .await;
    assert!(status.is_success(), "{status}: {body}");
    stack.wait_outcomes(&card, 1).await;
    let prompts = requests(&root, "session/prompt");
    assert_eq!(prompts.len(), 2);
    let last = prompts.last().unwrap()["params"]["prompt"]
        .as_array()
        .unwrap();
    assert!(last.iter().any(|part| {
        part["text"]
            .as_str()
            .is_some_and(|text| text.contains("later input"))
    }));
    assert!(last.iter().all(|part| {
        part["text"]
            .as_str()
            .is_none_or(|text| !text.contains("original operational input"))
    }));
    assert!(stack.repo().track_get(&track).await.unwrap().is_some());
    stack.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn acp_failed_cleanup_revokes_the_live_mcp_credential() {
    let root = Root::new("unused");
    let stack = boot(&root).await;
    let (_, card) = create(&stack).await;
    std::fs::write(root.path().join("scenario"), "hold").unwrap();
    let (status, body) = stack.post_input(&card, "hold").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let runtime = stack.runtime(&card).await;
    stack.wait_phase(&runtime.id, "turn_running").await;
    let pool = stack.repo().sqlite_pool().unwrap();
    let before: Option<String> =
        sqlx::query_scalar("SELECT mcp_token_hash FROM worker_sessions WHERE id=?1")
            .bind(&runtime.id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert!(
        before.is_some(),
        "exercise a minted credential, not an unconfigured path"
    );
    calm_server::planner_process::fail_claude_planner_stop_for_test(&runtime.id);
    let (status, body) = stack
        .send(
            "POST",
            &format!("/api/cards/{card}/planner/interrupt"),
            Some(json!({})),
        )
        .await;
    assert!(status.is_success(), "{status}: {body}");
    let outcomes = stack.wait_outcomes(&card, 1).await;
    calm_server::planner_process::clear_claude_planner_stop_failure_for_test(&runtime.id);
    assert_eq!(outcomes[0]["status"], "failed");
    let after: Option<String> =
        sqlx::query_scalar("SELECT mcp_token_hash FROM worker_sessions WHERE id=?1")
            .bind(&runtime.id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert!(
        after.is_none(),
        "failed process cleanup must not retain MCP authority"
    );
    stack.shutdown().await;
}

async fn delete_checkpoint_rows(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    card: &str,
    method: &str,
) {
    sqlx::query("DELETE FROM harness_items WHERE card_id=?1 AND method=?2")
        .bind(card)
        .bind(method)
        .execute(&mut **tx)
        .await
        .unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn acp_completed_receipt_restores_reply_after_harness_projection_crash() {
    let root = Root::new("unused");
    let stack = boot(&root).await;
    let (_, card) = create(&stack).await;
    let outcome = turn(&stack, &card, "retain this reply", 1).await;
    let runtime = stack.runtime(&card).await;
    let pool = stack.repo().sqlite_pool().unwrap();
    let mut checkpoint = stack.harness(&runtime.id).snapshot().await;
    stack.shutdown().await;
    checkpoint.phase = calm_server::harness::HarnessPhaseTag::TurnRunning;
    let mut tx = calm_server::db::sqlite::begin_immediate_tx(&pool)
        .await
        .unwrap();
    delete_checkpoint_rows(&mut tx, &card, "item/completed").await;
    delete_checkpoint_rows(&mut tx, &card, "item/started").await;
    delete_checkpoint_rows(&mut tx, &card, "turn/completed").await;
    sqlx::query("UPDATE worker_sessions SET handle_state_json=?2 WHERE id=?1")
        .bind(&runtime.id)
        .bind(serde_json::to_string(&checkpoint).unwrap())
        .execute(&mut *tx)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    let stack = boot(&root).await;
    let rows = stack
        .repo()
        .transcript_rows_of_thread(&card, runtime.thread_id.as_deref().unwrap())
        .await
        .unwrap();
    assert!(
        rows.iter()
            .any(|row| row.params.contains("retain this reply")
                && row.item_type.as_deref() == Some("agentMessage")),
        "completed reply must recover from its receipt"
    );
    let completed: Vec<Value> = rows
        .iter()
        .filter(|row| row.method == "item/completed")
        .map(|row| serde_json::from_str::<Value>(&row.params).unwrap()["item"].clone())
        .collect();
    assert_eq!(completed.len(), 3);
    assert_eq!(completed[0]["text"], "before operation");
    assert_eq!(completed[1]["type"], "dynamicToolCall");
    assert!(
        completed[2]["text"]
            .as_str()
            .unwrap()
            .contains("retain this reply")
    );
    assert_eq!(
        completed
            .iter()
            .map(|item| item["id"].as_str().unwrap())
            .collect::<std::collections::BTreeSet<_>>()
            .len(),
        3
    );
    assert_eq!(stack.outcomes(&card).await[0]["id"], outcome["id"]);
    assert_eq!(requests(&root, "session/prompt").len(), 1);
    stack.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn acp_operation_replay_uses_the_quiesced_full_checkpoint() {
    let root = Root::new("unused");
    let stack = boot(&root).await;
    let (_, card) = create(&stack).await;
    let pool = stack.repo().sqlite_pool().unwrap();
    let operation: String = sqlx::query_scalar("SELECT id FROM operations WHERE target_id=?1 AND phase='succeeded' ORDER BY created_at_ms DESC LIMIT 1")
        .bind(&card).fetch_one(&pool).await.unwrap();
    std::fs::write(root.path().join("scenario"), "hold").unwrap();
    let (status, body) = stack.post_input(&card, "already dispatched").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let runtime = stack.runtime(&card).await;
    stack.wait_phase(&runtime.id, "turn_running").await;
    let issued = stack
        .harness(&runtime.id)
        .snapshot()
        .await
        .last_turn_id
        .unwrap();
    // Crash replay uses the operation's original Idle snapshot while boot has
    // independently recovered and dispatched this runtime's current input.
    sqlx::query("UPDATE operations SET phase='spawn_started',phase_detail_json=NULL,lease_owner=NULL,lease_until_ms=NULL WHERE id=?1")
        .bind(&operation).execute(&pool).await.unwrap();
    calm_server::recover_operations_on_boot(&stack.state)
        .await
        .unwrap();
    let restored = stack.harness(&runtime.id).snapshot().await;
    assert_eq!(restored.last_turn_id.as_deref(), Some(issued.as_str()));
    assert!(restored.pending_entries().is_empty());
    assert_eq!(requests(&root, "session/prompt").len(), 1);
    assert_eq!(stack.wait_outcomes(&card, 1).await[0]["id"], issued);
    stack.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn acp_receipt_write_failure_cannot_publish_success() {
    let root = Root::new("unused");
    let stack = boot(&root).await;
    let (_, card) = create(&stack).await;
    std::fs::write(root.path().join("scenario"), "settlement").unwrap();
    let (status, body) = stack.post_input(&card, "settle once").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    wait_file(&root, "before-settlement").await;
    let pool = stack.repo().sqlite_pool().unwrap();
    sqlx::query(concat!(
        "CREATE TRIGGER refuse_acp_settlement BEFORE UPDATE ON acp_submissions ",
        "WHEN NEW.state='completed' BEGIN ",
        "SELECT RAISE(ABORT,'fixture receipt write failure'); END"
    ))
    .execute(&pool)
    .await
    .unwrap();
    std::fs::write(root.path().join("release-settlement"), "").unwrap();
    let outcome = stack.wait_outcomes(&card, 1).await;
    assert_eq!(
        outcome[0]["status"], "failed",
        "undurable success cannot be published"
    );
    assert!(
        outcome[0]["error"]["message"]
            .as_str()
            .unwrap()
            .contains("unknown")
    );
    let runtime = stack.runtime(&card).await;
    assert!(
        calm_server::db::sqlite::acp_submission_unresolved(&pool, &runtime.id)
            .await
            .unwrap()
    );
    sqlx::query("DROP TRIGGER refuse_acp_settlement")
        .execute(&pool)
        .await
        .unwrap();
    stack.shutdown().await;
    let stack = boot(&root).await;
    assert_eq!(stack.outcomes(&card).await[0]["status"], "failed");
    assert_eq!(requests(&root, "session/prompt").len(), 1);
    stack.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn acp_recovery_retires_dispatch_before_folding_later_report_edits() {
    use calm_server::event::{EditAuthor, Event, EventScope};
    use calm_server::harness::Observation;
    use calm_server::ids::{ActorId, CardId, TrackId};
    let root = Root::new("unused");
    let stack = boot(&root).await;
    let (track, card) = create(&stack).await;
    let runtime = stack.runtime(&card).await;
    std::fs::write(root.path().join("scenario"), "checkpoint").unwrap();
    stack
        .harness(&runtime.id)
        .observe(Observation::ReportEdited {
            track_id: TrackId::from(track.clone()),
            body_sha256: "original".into(),
            body: "original report".into(),
            author: Some(EditAuthor::User),
            body_before: None,
            doc_rev_after: None,
            blocks_after: None,
        })
        .unwrap();
    wait_file_within(&root, "setup-checkpoint", Duration::from_secs(50)).await;
    let checkpoint = stack.harness(&runtime.id).snapshot().await;
    assert_eq!(
        checkpoint.phase,
        calm_server::harness::HarnessPhaseTag::IssuingTurn
    );
    assert!(matches!(
        checkpoint.pending_entries()[0].observation(),
        Observation::ReportEdited { .. }
    ));
    std::fs::write(root.path().join("release-setup"), "").unwrap();
    stack.wait_outcomes(&card, 1).await;
    let pool = stack.repo().sqlite_pool().unwrap();
    stack.shutdown().await;
    let mut tx = calm_server::db::sqlite::begin_immediate_tx(&pool)
        .await
        .unwrap();
    sqlx::query("UPDATE worker_sessions SET handle_state_json=?2 WHERE id=?1")
        .bind(&runtime.id)
        .bind(serde_json::to_string(&checkpoint).unwrap())
        .execute(&mut *tx)
        .await
        .unwrap();
    let area: String = sqlx::query_scalar("SELECT area_id FROM cards WHERE id=?1")
        .bind(&card)
        .fetch_one(&mut *tx)
        .await
        .unwrap();
    let later = calm_server::db::sqlite::append_decision_event_in_tx(
        &mut tx,
        &ActorId::User,
        &EventScope::Track {
            area: area.into(),
            track: TrackId::from(track.clone()),
        },
        None,
        &Event::TrackReportEdited {
            track_id: track.clone().into(),
            card_id: CardId::from(card.clone()),
            author: EditAuthor::User,
            author_plugin_id: None,
            edit_id: "later-edit".into(),
            summary_before: String::new(),
            summary_after: String::new(),
            body_before: "original report".into(),
            body_after: "later report".into(),
            agent_message: None,
        },
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    let stack = boot(&root).await;
    let recovered = stack.harness(&runtime.id).snapshot().await;
    let entries = recovered.pending_entries();
    assert_eq!(
        entries.len(),
        1,
        "only the undispatched report edit remains"
    );
    assert_eq!(entries[0].envelope_id(), Some(later));
    assert!(
        matches!(entries[0].observation(), Observation::ReportEdited { body, .. } if body == "later report")
    );
    assert_eq!(requests(&root, "session/prompt").len(), 1);
    stack.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn acp_reset_does_not_mirror_the_predecessors_revoked_hash() {
    let root = Root::new("unused");
    let stack = boot(&root).await;
    let (_, card) = create(&stack).await;
    std::fs::write(root.path().join("scenario"), "hold").unwrap();
    let (status, body) = stack.post_input(&card, "hold").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let runtime = stack.runtime(&card).await;
    stack.wait_phase(&runtime.id, "turn_running").await;
    let pool = stack.repo().sqlite_pool().unwrap();
    let old: Option<String> =
        sqlx::query_scalar("SELECT mcp_token_hash FROM worker_sessions WHERE id=?1")
            .bind(&runtime.id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert!(old.is_some());
    let (status, body) = stack
        .send(
            "POST",
            &format!("/api/cards/{card}/planner/interrupt"),
            None,
        )
        .await;
    assert!(status.is_success(), "{body}");
    stack.wait_outcomes(&card, 1).await;
    let (status, body) = stack
        .send(
            "POST",
            &format!("/api/cards/{card}/planner/reset"),
            Some(json!({})),
        )
        .await;
    assert!(status.is_success(), "{body}");
    let current = stack.runtime(&card).await;
    assert_ne!(current.id, runtime.id);
    let mirrored: Option<String> =
        sqlx::query_scalar("SELECT mcp_token_hash FROM worker_sessions WHERE id=?1")
            .bind(&current.id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert!(
        mirrored.is_none(),
        "idle successor cannot inherit an old credential"
    );
    let active:i64=sqlx::query_scalar("SELECT COUNT(*) FROM worker_sessions WHERE mcp_token_hash=?1 AND state IN ('starting','running','idle','turn_pending')").bind(old).fetch_one(&pool).await.unwrap();
    assert_eq!(active, 0);
    stack.shutdown().await;
}
