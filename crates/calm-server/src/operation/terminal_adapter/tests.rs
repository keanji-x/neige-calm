use super::*;
use crate::db::sqlite::begin_immediate_tx;
use crate::operation::{OperationKey, OperationRepo, SqlxOperationRepo};
use std::sync::Arc;

struct TerminalWorkerHarness {
    repo: Arc<crate::db::sqlite::SqlxRepo>,
    adapter: TerminalWorkerAdapter,
    track_id: String,
    workspace: Option<tempfile::TempDir>,
}

async fn terminal_worker_harness() -> TerminalWorkerHarness {
    let workspace = tempfile::tempdir().unwrap();
    let mut harness =
        terminal_worker_harness_with_workspace(workspace.path().to_str().unwrap()).await;
    harness.workspace = Some(workspace);
    harness
}

async fn terminal_worker_harness_with_workspace(workspace: &str) -> TerminalWorkerHarness {
    let repo = Arc::new(
        crate::db::sqlite::SqlxRepo::open("sqlite::memory:")
            .await
            .unwrap(),
    );
    let area = crate::db::RepoSyncDomainRaw::area_create(
        repo.as_ref(),
        crate::model::NewArea {
            name: "terminal workers".into(),
            color: "#101010".into(),
            sort: None,
        },
    )
    .await
    .unwrap();
    let track = crate::db::RepoSyncDomainRaw::track_create(
        repo.as_ref(),
        crate::model::NewTrack {
            template_input: None,
            area_id: area.id,
            title: "terminal workers".into(),
            sort: None,
            cwd: workspace.to_string(),
            template_id: None,
            plugin_scope: None,
            attach_folder: false,
            theme: RequestTheme::default_dark(),
        },
    )
    .await
    .unwrap();
    let route_repo: Arc<dyn crate::db::RouteRepo> = repo.clone();
    TerminalWorkerHarness {
        adapter: TerminalWorkerAdapter::new(
            route_repo,
            CardRoleCache::new(),
            TrackAreaCache::new(),
        ),
        repo,
        track_id: track.id.to_string(),
        workspace: None,
    }
}

async fn prepare_terminal_worker(harness: &TerminalWorkerHarness, key: &str) -> TxOutput {
    prepare_terminal_worker_with_cwd(harness, key, Some("/tmp".into())).await
}

async fn prepare_terminal_worker_with_cwd(
    harness: &TerminalWorkerHarness,
    key: &str,
    cwd: Option<String>,
) -> TxOutput {
    let task_id = format!("{}:{key}", harness.track_id);
    let payload = serde_json::to_value(TerminalWorkerOperationPayload {
        actor: ActorId::KernelDispatcher,
        track_id: harness.track_id.clone(),
        idempotency_key: task_id.clone(),
        cmd: format!("printf {key}\n"),
        cwd,
    })
    .unwrap();
    sqlx::query(
        "INSERT OR IGNORE INTO tasks \
         (id, track_id, key, kind, goal, context_json, depends_on_json, status, created_at_ms, updated_at_ms) \
         VALUES (?1, ?2, ?3, 'terminal', 'test', 'null', '[]', 'dispatched', 1, 1)",
    )
    .bind(&task_id)
    .bind(&harness.track_id)
    .bind(key)
    .execute(harness.repo.pool())
    .await
    .unwrap();
    let op_repo = SqlxOperationRepo::new(harness.repo.pool().clone());
    let op_id = op_repo
        .insert_operation(
            "terminal-worker",
            OperationKey {
                operation_key: new_id(),
                idempotency_key: Some(format!("op-{key}")),
                payload_hash: format!("hash-{key}"),
            },
            payload.clone(),
        )
        .await
        .unwrap();
    let op = op_repo
        .claim_drive_batch(1)
        .await
        .unwrap()
        .into_iter()
        .find(|op| op.id == op_id)
        .unwrap();
    let mut tx = begin_immediate_tx(harness.repo.pool()).await.unwrap();
    let output = harness
        .adapter
        .prepare_tx(&mut tx, &payload, &op)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    output
}

#[tokio::test]
async fn terminal_worker_prepare_titles_card_with_task_key() {
    let harness = terminal_worker_harness().await;
    let output = prepare_terminal_worker(&harness, "slice-d").await;
    let card_id = output.output_string("card_id", "test").unwrap();

    let stored: Option<String> = sqlx::query_scalar("SELECT title FROM cards WHERE id = ?1")
        .bind(&card_id)
        .fetch_one(harness.repo.pool())
        .await
        .unwrap();
    assert_eq!(stored, Some("slice-d".to_string()));

    let wire: crate::model::Card = serde_json::from_value(output.result.clone()).unwrap();
    assert_eq!(wire.title, Some("slice-d".to_string()));
    assert_eq!(
        wire.payload.get("idempotency_key").and_then(Value::as_str),
        Some(format!("{}:slice-d", harness.track_id).as_str())
    );
    assert_eq!(
        wire.payload.get("role_request").and_then(Value::as_str),
        Some("terminal")
    );
}

/// #1814: a Planner-dispatched terminal task may run `claude`; it gets auto-memory off in the env the
/// spawn side effect hands the PTY and the terminal row persists.
#[tokio::test]
async fn terminal_worker_env_disables_claude_auto_memory() {
    let harness = terminal_worker_harness().await;
    let output = prepare_terminal_worker(&harness, "memory").await;
    assert_eq!(
        output.data["env"]["CLAUDE_CODE_DISABLE_AUTO_MEMORY"], "1",
        "{}",
        output.data["env"]
    );
    let card_id = output.output_string("card_id", "test").unwrap();
    let stored: String = sqlx::query_scalar("SELECT env FROM terminals WHERE card_id = ?1")
        .bind(&card_id)
        .fetch_one(harness.repo.pool())
        .await
        .unwrap();
    let stored: Value = serde_json::from_str(&stored).unwrap();
    assert_eq!(stored["CLAUDE_CODE_DISABLE_AUTO_MEMORY"], "1", "{stored}");
}

/// The task row carries `cwd: None` because that is the shape production sends.
#[tokio::test]
async fn terminal_worker_without_cwd_lands_in_the_track_workspace() {
    let harness = terminal_worker_harness().await;
    let workspace = harness.workspace.as_ref().unwrap().path().to_str().unwrap();

    let output = prepare_terminal_worker_with_cwd(&harness, "no-cwd", None).await;
    let card_id = output.output_string("card_id", "test").unwrap();

    let stored: String = sqlx::query_scalar("SELECT cwd FROM terminals WHERE card_id = ?1")
        .bind(&card_id)
        .fetch_one(harness.repo.pool())
        .await
        .unwrap();
    assert_eq!(stored, workspace);

    let wire: crate::model::Card = serde_json::from_value(output.result.clone()).unwrap();
    assert_eq!(
        wire.payload.get("cwd").and_then(Value::as_str),
        Some(workspace),
        "the card payload's cwd is what the FE shows; it must agree with the row"
    );

    // No freeze assertion here: this harness's track is `attached`, which is frozen at creation, so `frozen_at.is_some()` would pass with the freeze deleted.
}

#[tokio::test]
async fn terminal_worker_with_an_explicit_cwd_keeps_it() {
    let harness = terminal_worker_harness().await;
    let output = prepare_terminal_worker_with_cwd(&harness, "explicit", Some("/tmp".into())).await;
    let card_id = output.output_string("card_id", "test").unwrap();
    let stored: String = sqlx::query_scalar("SELECT cwd FROM terminals WHERE card_id = ?1")
        .bind(&card_id)
        .fetch_one(harness.repo.pool())
        .await
        .unwrap();
    assert_eq!(stored, "/tmp");
}

#[tokio::test]
async fn terminal_worker_refuses_to_default_to_an_empty_workspace() {
    let harness = terminal_worker_harness_with_workspace("").await;
    let stored: String = sqlx::query_scalar("SELECT workspace_path FROM tracks WHERE id = ?1")
        .bind(&harness.track_id)
        .fetch_one(harness.repo.pool())
        .await
        .unwrap();
    assert_eq!(stored, "", "premise: this harness's track has no workspace");

    let task_id = format!("{}:empty", harness.track_id);
    let payload = serde_json::to_value(TerminalWorkerOperationPayload {
        actor: ActorId::KernelDispatcher,
        track_id: harness.track_id.clone(),
        idempotency_key: task_id.clone(),
        cmd: "printf hi\n".into(),
        cwd: None,
    })
    .unwrap();
    sqlx::query(
        "INSERT OR IGNORE INTO tasks \
         (id, track_id, key, kind, goal, context_json, depends_on_json, status, created_at_ms, updated_at_ms) \
         VALUES (?1, ?2, 'empty', 'terminal', 'test', 'null', '[]', 'dispatched', 1, 1)",
    )
    .bind(&task_id)
    .bind(&harness.track_id)
    .execute(harness.repo.pool())
    .await
    .unwrap();
    let op_repo = SqlxOperationRepo::new(harness.repo.pool().clone());
    let op_id = op_repo
        .insert_operation(
            "terminal-worker",
            OperationKey {
                operation_key: new_id(),
                idempotency_key: Some("op-empty".into()),
                payload_hash: "hash-empty".into(),
            },
            payload.clone(),
        )
        .await
        .unwrap();
    let op = op_repo
        .claim_drive_batch(1)
        .await
        .unwrap()
        .into_iter()
        .find(|op| op.id == op_id)
        .unwrap();
    let mut tx = begin_immediate_tx(harness.repo.pool()).await.unwrap();
    let err = harness
        .adapter
        .prepare_tx(&mut tx, &payload, &op)
        .await
        .expect_err("an empty workspace path must not become an empty cwd");
    assert!(
        err.to_string().contains("no workspace path"),
        "unexpected error: {err}"
    );
}

#[cfg(test)]
mod launch_cleanup_tests;

#[cfg(test)]
mod disposal_tests;

#[tokio::test]
async fn terminal_writer_releases_only_after_sealed_stop() {
    let harness = terminal_worker_harness().await;
    let output = prepare_terminal_worker(&harness, "writer-stop").await;
    let id = output.output_string("terminal_id", "test").unwrap();
    let held:i64=sqlx::query_scalar("SELECT count(*) FROM workspace_leases WHERE holder_kind='terminal' AND holder_id=?1 AND state='held'")
        .bind(&id).fetch_one(harness.repo.pool()).await.unwrap();
    assert_eq!(held, 1, "prepare must reserve the actual terminal cwd");
    let absent = harness
        .workspace
        .as_ref()
        .unwrap()
        .path()
        .join("missing-supervisor.sock");
    assert!(
        crate::terminal_renderer::stop_and_release_terminal(harness.repo.as_ref(), &absent, &id)
            .await
            .is_err()
    );
    let state: String = sqlx::query_scalar(
        "SELECT state FROM workspace_leases WHERE holder_kind='terminal' AND holder_id=?1",
    )
    .bind(&id)
    .fetch_one(harness.repo.pool())
    .await
    .unwrap();
    assert_eq!(state, "held", "uncertain stop retains writer");
    let supervisor = calm_proc_supervisor::test_support::InProcessProcSupervisor::start()
        .await
        .unwrap();
    crate::terminal_renderer::stop_and_release_terminal(
        harness.repo.as_ref(),
        supervisor.sock(),
        &id,
    )
    .await
    .unwrap();
    let state: String = sqlx::query_scalar(
        "SELECT state FROM workspace_leases WHERE holder_kind='terminal' AND holder_id=?1",
    )
    .bind(&id)
    .fetch_one(harness.repo.pool())
    .await
    .unwrap();
    assert_eq!(state, "released", "sealed provider proof permits release");
}

#[tokio::test]
async fn terminal_create_ui_consumes_launch_once_and_recovery_only_attaches() {
    use crate::operation::{
        Phase,
        terminal_launch::{self, Launch, TerminalStart},
    };
    let harness = terminal_worker_harness().await;
    let adapter = TerminalAdapter::new(
        harness.repo.clone(),
        CardRoleCache::new(),
        TrackAreaCache::new(),
    );
    let payload = json!({"actor":ActorId::Kernel,"track_id":harness.track_id,"program":"/bin/sh","cwd":harness.workspace.as_ref().unwrap().path(),"theme":RequestTheme::default_dark()});
    let ops = SqlxOperationRepo::new(harness.repo.pool().clone());
    let id = ops
        .insert_operation(
            "terminal-create",
            OperationKey {
                operation_key: new_id(),
                idempotency_key: None,
                payload_hash: "create-ui".into(),
            },
            payload,
        )
        .await
        .unwrap();
    let op = ops
        .claim_drive_batch(1)
        .await
        .unwrap()
        .into_iter()
        .find(|o| o.id == id)
        .unwrap();
    ops.prepare_tx_and_advance(&op, &adapter)
        .await
        .unwrap()
        .unwrap();
    let op = ops
        .claim_drive_batch(1)
        .await
        .unwrap()
        .into_iter()
        .find(|o| o.id == id)
        .unwrap();
    ops.set_phase(&op, Phase::SpawnStarted)
        .await
        .unwrap()
        .unwrap();
    let op = ops
        .claim_drive_batch(1)
        .await
        .unwrap()
        .into_iter()
        .find(|o| o.id == id)
        .unwrap();
    let output = op.tx_output.as_ref().unwrap();
    let terminal = output.output_string("terminal_id", "test").unwrap();
    let sock = harness
        .workspace
        .as_ref()
        .unwrap()
        .path()
        .join("supervisor.sock");
    let first = terminal_launch::resolve(harness.repo.as_ref(), &terminal, &sock, None)
        .await
        .unwrap();
    assert!(matches!(first,TerminalStart::Fresh(launch) if matches!(*launch,Launch::Terminal(_))));
    let second = terminal_launch::resolve(harness.repo.as_ref(), &terminal, &sock, None)
        .await
        .unwrap();
    assert!(
        matches!(second, TerminalStart::AttachOnly(_)),
        "UI/recovery cannot reissue EnsureProc after recorded request"
    );
    ops.set_phase(&op, Phase::Succeeded).await.unwrap().unwrap();
    let supervisor = calm_proc_supervisor::test_support::InProcessProcSupervisor::start()
        .await
        .unwrap();
    crate::terminal_sweeper::reconcile_terminal_writers(harness.repo.as_ref(), supervisor.sock())
        .await
        .unwrap();
    let state: String = sqlx::query_scalar(
        "SELECT state FROM workspace_leases WHERE holder_kind='terminal' AND holder_id=?1",
    )
    .bind(&terminal)
    .fetch_one(harness.repo.pool())
    .await
    .unwrap();
    assert_eq!(
        state, "released",
        "restart reconciliation seals a missing execution before releasing its guard"
    );
}

#[tokio::test]
async fn legacy_terminal_without_persisted_launch_owner_only_attaches() {
    let harness = terminal_worker_harness().await;
    let output = prepare_terminal_worker(&harness, "legacy-no-owner").await;
    // prepare_tx alone has not committed an Operation output/target. That
    // historical terminal row cannot authorize a new process from a UI attach.
    let id = output.output_string("terminal_id", "test").unwrap();
    let sock = harness
        .workspace
        .as_ref()
        .unwrap()
        .path()
        .join("supervisor.sock");
    let start = super::super::terminal_launch::resolve(harness.repo.as_ref(), &id, &sock, None)
        .await
        .unwrap();
    assert!(matches!(
        start,
        super::super::terminal_launch::TerminalStart::AttachOnly(_)
    ));
}
