use super::*;
use crate::db::sqlite::begin_immediate_tx;
use crate::operation::{OperationKey, OperationRepo, SqlxOperationRepo};
use std::sync::Arc;

struct TerminalWorkerHarness {
    repo: Arc<crate::db::sqlite::SqlxRepo>,
    adapter: TerminalWorkerAdapter,
    track_id: String,
}

/// #1147 S6 — a track's workspace is where its terminals land, so the harness
/// track carries one. An empty `workspace_path` is not a state any creation
/// route produces; `terminal_worker_refuses_to_default_to_an_empty_workspace`
/// builds that one deliberately.
const HARNESS_WORKSPACE: &str = "/neige-fixture-workspace";

async fn terminal_worker_harness() -> TerminalWorkerHarness {
    terminal_worker_harness_with_workspace(HARNESS_WORKSPACE).await
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

/// #1149 — the terminal worker card is titled after its task's plan key.
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
    // The pre-existing payload merge must survive untouched.
    assert_eq!(
        wire.payload.get("idempotency_key").and_then(Value::as_str),
        Some(format!("{}:slice-d", harness.track_id).as_str())
    );
    assert_eq!(
        wire.payload.get("role_request").and_then(Value::as_str),
        Some("terminal")
    );
}

/// #1147 S6 — a terminal worker whose task row names no cwd lands in the
/// **track's workspace**. It used to land in `$HOME` (`default_cwd()`), which is
/// how "each track is a repository" stopped being true the moment a worker
/// actually opened a shell.
///
/// The task row deliberately carries `cwd: None`: the dispatcher keeps it that
/// way on purpose (`scheduler::build_worker_payload` — materializing a default
/// into the payload would put the server's environment into
/// `stable_payload_hash`), so `None` is the shape production actually sends.
#[tokio::test]
async fn terminal_worker_without_cwd_lands_in_the_track_workspace() {
    let harness = terminal_worker_harness().await;
    let workspace = HARNESS_WORKSPACE;

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

    // No freeze assertion here, deliberately: this harness's track is
    // `attached`, and attached tracks are frozen the moment they are created
    // (design §数据模型), so asserting `frozen_at.is_some()` would pass with the
    // freeze deleted. Freeze point 2 is asserted where it can actually fail —
    // `track_workspace_repoint::a_terminal_card_lands_in_the_workspace_and_freezes_it`
    // (unfrozen managed track, user route) and
    // `claude_card_endpoint::post_claude_restart_does_not_deadlock_on_the_workspace_freeze`
    // (the call site that reaches `terminal_create_tx` directly).
}

/// A task row that names a cwd keeps it. The workspace is the *default*, not an
/// override — plan authors can still pin a worker to a directory.
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

/// Fail closed rather than inheriting the server's cwd.
///
/// A track with an empty `workspace_path` is not a state S2 can produce; if one
/// exists the row is broken. Writing `""` into `terminals.cwd` would make the
/// worker inherit whatever directory the kernel happens to be running in — the
/// unreadable-failure shape #1147 was opened on.
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
