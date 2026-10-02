//! #1933 T3 — a read-only claude task whose declared `head` is not the track checkout's HEAD is
//! refused before spawn, as a codex one is (`codex_adapter/tests/reader_head_tests.rs`), and so
//! is a Claude restart of its worker card.
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

/// A reader's worker exits, its checkout moves off the declared head, and the card is restarted
/// (`--resume`): the restart is refused before the provider is called.
#[cfg(feature = "fixtures")]
#[tokio::test]
async fn claude_reader_restart_refuses_a_checkout_that_left_the_head() {
    use crate::operation::claude_restart_adapter::{
        ClaudeRestartAdapter, ClaudeRestartOperationPayload,
    };
    let harness = claude_worker_harness().await;
    let head = git_rev(&harness.worktree, "HEAD");
    sqlx::query(
        "INSERT INTO tasks (id, track_id, key, kind, goal, context_json, depends_on_json, status, \
         created_at_ms, updated_at_ms, access, head) \
         VALUES (?1, ?2, 'resume', 'claude', 'review', 'null', '[]', 'dispatched', 1, 1, \
         'read_only', ?3)",
    )
    .bind(format!("{}:resume", harness.track_id))
    .bind(&harness.track_id)
    .bind(&head)
    .execute(harness.repo.pool())
    .await
    .unwrap();
    let (output, _, _) = prepare_claude_worker(&harness, "resume").await;
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
    assert!(
        std::process::Command::new("git")
            .args([
                "-c",
                "user.name=T",
                "-c",
                "user.email=t@example.invalid",
                "commit"
            ])
            .args(["--allow-empty", "-qm", "moved"])
            .current_dir(&harness.worktree)
            .status()
            .unwrap()
            .success()
    );
    let actual = git_rev(&harness.worktree, "HEAD");

    let spawns = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let counted = spawns.clone();
    let hook: SpawnHook = Arc::new(move |_terminal_id, _command_line, _cwd, _env| {
        counted.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Box::pin(async { Ok(SpawnHandle::NoOp) })
    });
    let route_repo: Arc<dyn crate::db::RouteRepo> = harness.repo.clone();
    let restart = ClaudeRestartAdapter::new_with_spawn_hook(
        route_repo.clone(),
        Arc::new(CodexClient::new_stub()),
        CardRoleCache::new(),
        TrackAreaCache::new(),
        hook,
    );
    let payload = serde_json::to_value(ClaudeRestartOperationPayload {
        actor: ActorId::KernelDispatcher,
        worker_session_id: None,
        card_id,
    })
    .unwrap();
    let op = claude_worker_op("op-restart", payload.clone());
    let mut tx = begin_immediate_tx(harness.repo.pool()).await.unwrap();
    let restart_output = restart.prepare_tx(&mut tx, &payload, &op).await.unwrap();
    tx.commit().await.unwrap();
    let ctx = SpawnCtx::new(
        route_repo,
        Arc::new(SqlxOperationRepo::new(harness.repo.pool().clone())),
        Arc::new(DaemonClient::new_stub()),
        TerminalRendererRegistry::new(),
        harness.events.clone(),
        OperationCompletionBus::new(),
    );
    let message = match restart.spawn_side_effect(&restart_output, &op, &ctx).await {
        Ok(_) => panic!("a restart whose checkout left the declared head must not spawn"),
        Err(error) => error.to_string(),
    };
    assert!(
        message.contains(&format!("refused: {TRACK_HEAD_MISMATCH}:"))
            && message.contains(&head)
            && message.contains(&actual),
        "{message}"
    );
    assert_eq!(
        spawns.load(std::sync::atomic::Ordering::SeqCst),
        0,
        "the provider was called"
    );
}
