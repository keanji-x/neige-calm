//! #1933 T3 — a read-only claude task whose declared `head` is not the track checkout's HEAD is
//! refused before spawn, as a codex one is (`codex_adapter/tests/reader_head_tests.rs`). A Claude
//! restart of its worker card is refused outright (#2493).
use super::*;
use crate::operation::workspace_lease::worker::TRACK_HEAD_MISMATCH;

fn git_rev(dir: &std::path::Path, rev: &str) -> String {
    let output = std::process::Command::new("git")
        .args(["rev-parse", rev])
        .current_dir(dir)
        .output()
        .unwrap();
    assert!(output.status.success());
    String::from_utf8(output.stdout).unwrap().trim().to_string()
}

/// The harness checkout has two commits; the reader declares the first.
#[tokio::test]
async fn claude_reader_at_another_head_is_refused_before_spawn() {
    let harness = claude_worker_harness().await;
    let actual = git_rev(&harness.worktree, "HEAD");
    let declared = git_rev(&harness.worktree, "HEAD~1");
    sqlx::query(
        "INSERT INTO tasks (id, track_id, key, kind, goal, context_json, depends_on_json, status, \
         created_at_ms, updated_at_ms, access, head) \
         VALUES (?1, ?2, 'review', 'claude', 'review', 'null', '[]', 'dispatched', 1, 1, \
         'read_only', ?3)",
    )
    .bind(format!("{}:review", harness.track_id))
    .bind(&harness.track_id)
    .bind(&declared)
    .execute(harness.repo.pool())
    .await
    .unwrap();
    let message = try_prepare_claude_worker(&harness, "review")
        .await
        .expect_err("a reader at another head is refused")
        .to_string();
    assert!(
        message.contains(&format!("refused: {TRACK_HEAD_MISMATCH}:"))
            && message.contains(&declared)
            && message.contains(&actual),
        "{message}"
    );
}

/// #2493: a task worker's card is never restarted. A restart starts a new session for the card,
/// and a session serves one attempt, so a Claude restart of a card whose session is bound is
/// refused in prepare, naming the attempt, and writes nothing — found through the binding even
/// after the card payload's `idempotency_key` is edited away.
#[cfg(feature = "fixtures")]
#[tokio::test]
async fn claude_restart_refuses_a_bound_card() {
    use crate::operation::claude_restart_adapter::{
        ClaudeRestartAdapter, ClaudeRestartOperationPayload,
    };
    let harness = claude_worker_harness().await;
    let head = git_rev(&harness.worktree, "HEAD");
    let task_id = format!("{}:resume", harness.track_id);
    sqlx::query(
        "INSERT INTO tasks (id, track_id, key, kind, goal, context_json, depends_on_json, status, \
         created_at_ms, updated_at_ms, access, head) \
         VALUES (?1, ?2, 'resume', 'claude', 'review', 'null', '[]', 'dispatched', 1, 1, \
         'read_only', ?3)",
    )
    .bind(&task_id)
    .bind(&harness.track_id)
    .bind(&head)
    .execute(harness.repo.pool())
    .await
    .unwrap();
    let output = prepare_claude_worker_as_scheduled(&harness, "resume").await;
    let card_id = output.output_string("card_id", "test").unwrap();
    let terminal_id = output.output_string("terminal_id", "test").unwrap();
    crate::db::RepoOutOfDomain::terminal_set_exit(
        harness.repo.as_ref(),
        &terminal_id,
        Some(0),
        false,
    )
    .await
    .unwrap();
    let mut tx = begin_immediate_tx(harness.repo.pool()).await.unwrap();
    crate::db::sqlite::session_complete_for_card_tx(&mut tx, &card_id, WorkerSessionState::Exited)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    sqlx::query(
        "UPDATE cards SET payload = json_remove(payload, '$.idempotency_key') WHERE id = ?1",
    )
    .bind(&card_id)
    .execute(harness.repo.pool())
    .await
    .unwrap();

    let hook: SpawnHook = Arc::new(move |_terminal_id, _command_line, _cwd, _env| {
        Box::pin(async { panic!("a refused restart never spawns") })
    });
    let restart = ClaudeRestartAdapter::new_with_spawn_hook(
        harness.repo.clone(),
        Arc::new(CodexClient::new_stub()),
        CardRoleCache::new(),
        TrackAreaCache::new(),
        hook,
    );
    let payload = serde_json::to_value(ClaudeRestartOperationPayload {
        actor: ActorId::KernelDispatcher,
        worker_session_id: None,
        card_id: card_id.clone(),
    })
    .unwrap();
    let restart_op = claude_worker_op("op-restart", payload.clone());
    let rows = || async {
        let sessions: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM worker_sessions")
            .fetch_one(harness.repo.pool())
            .await
            .unwrap();
        let events: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM events")
            .fetch_one(harness.repo.pool())
            .await
            .unwrap();
        (sessions, events)
    };
    let before = rows().await;
    let mut tx = begin_immediate_tx(harness.repo.pool()).await.unwrap();
    let error = restart
        .prepare_tx(&mut tx, &payload, &restart_op)
        .await
        .unwrap_err();
    tx.rollback().await.unwrap();
    assert!(
        matches!(&error, CalmError::Conflict(message) if message == &format!(
            "claude-restart: card {card_id} runs task attempts (last {task_id}); declare a new \
             task for a fresh worker"
        )),
        "{error}"
    );
    assert_eq!(rows().await, before, "a refused restart writes nothing");
}
