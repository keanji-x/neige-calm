use super::*;
use crate::db::sqlite::begin_immediate_tx;
use crate::operation::{OperationKey, OperationRepo, SqlxOperationRepo};
use std::sync::Arc;

struct TerminalWorkerHarness {
    repo: Arc<crate::db::sqlite::SqlxRepo>,
    adapter: TerminalWorkerAdapter,
    track_id: String,
}

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
    terminal_worker_harness_with_repo(repo, workspace).await
}

async fn terminal_worker_harness_with_repo(
    repo: Arc<crate::db::sqlite::SqlxRepo>,
    workspace: &str,
) -> TerminalWorkerHarness {
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

/// The task row carries `cwd: None` because that is the shape production sends. A track with no
/// worktree (managed, or attached before #1830) keeps its workspace path: `agent_cwd()` reads only
/// the worktree, not the kind.
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

/// An attached track with its #1830 track worktree, made from a fixture repo.
async fn worktree_harness(tmp: &std::path::Path) -> (TerminalWorkerHarness, String) {
    let checkout = tmp.join("checkout");
    crate::test_support::init_fixture_git_repo(&checkout);
    let harness = terminal_worker_harness_with_workspace(checkout.to_str().unwrap()).await;
    let worktree = crate::test_support::attach_track_worktree(
        harness.repo.pool(),
        &harness.track_id,
        &checkout,
    )
    .await;
    (harness, worktree.to_str().unwrap().to_string())
}

async fn terminal_cwd(harness: &TerminalWorkerHarness, output: &TxOutput) -> String {
    let card_id = output.output_string("card_id", "test").unwrap();
    sqlx::query_scalar("SELECT cwd FROM terminals WHERE card_id = ?1")
        .bind(&card_id)
        .fetch_one(harness.repo.pool())
        .await
        .unwrap()
}

/// #2139 R2: a terminal task with no cwd runs where the track's codex and claude tasks run, the
/// track worktree, never the user's checkout; a named cwd still wins.
#[tokio::test]
async fn terminal_worker_without_cwd_lands_in_the_track_worktree() {
    let tmp = tempfile::tempdir().unwrap();
    let (harness, worktree) = worktree_harness(tmp.path()).await;

    let output = prepare_terminal_worker_with_cwd(&harness, "no-cwd", None).await;
    assert_eq!(terminal_cwd(&harness, &output).await, worktree);
    assert_eq!(output.data["cwd"], worktree.as_str());

    let output = prepare_terminal_worker_with_cwd(&harness, "named", Some("/tmp".into())).await;
    assert_eq!(terminal_cwd(&harness, &output).await, "/tmp");
}

/// #2139 R2: `neige_terminal_open` and UI terminal cards (`terminal-create`) with no cwd open in
/// the track worktree too.
#[tokio::test]
async fn terminal_open_without_cwd_lands_in_the_track_worktree() {
    let tmp = tempfile::tempdir().unwrap();
    let (harness, worktree) = worktree_harness(tmp.path()).await;
    let route_repo: Arc<dyn crate::db::RouteRepo> = harness.repo.clone();
    let adapter = TerminalAdapter::new(route_repo, CardRoleCache::new(), TrackAreaCache::new());
    let payload = serde_json::to_value(TerminalCreateOperationPayload {
        actor: ActorId::KernelDispatcher,
        worker_session_id: None,
        planner_hooks: false,
        request: normalize_terminal_create_request(TerminalCreateRequestPayload {
            track_id: harness.track_id.clone(),
            title: None,
            sort: None,
            program: String::new(),
            cwd: String::new(),
            env: Value::Null,
            theme: RequestTheme::default_dark(),
        }),
    })
    .unwrap();
    let op_repo = SqlxOperationRepo::new(harness.repo.pool().clone());
    let op_id = op_repo
        .insert_operation(
            "terminal-create",
            OperationKey {
                operation_key: new_id(),
                idempotency_key: Some("op-open".into()),
                payload_hash: "hash-open".into(),
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
    let output = adapter.prepare_tx(&mut tx, &payload, &op).await.unwrap();
    tx.commit().await.unwrap();

    assert_eq!(terminal_cwd(&harness, &output).await, worktree);
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

#[cfg(test)]
mod pty_cwd_tests;

/// #2493: the terminal worker op's prepare binds its attempt to the session it creates and stamps
/// the card in the same write.
#[tokio::test]
async fn first_spawn_binds_attempt_in_prepare_tx() {
    let harness = terminal_worker_harness().await;
    let output = prepare_terminal_worker(&harness, "bind").await;
    let (session, card): (Option<String>, Option<String>) =
        sqlx::query_as("SELECT worker_session_id, worker_card_id FROM tasks WHERE id = ?1")
            .bind(format!("{}:bind", harness.track_id))
            .fetch_one(harness.repo.pool())
            .await
            .unwrap();
    assert_eq!(
        session.as_deref(),
        Some(output.output_string("runtime_id", "test").unwrap().as_str())
    );
    assert_eq!(
        card.as_deref(),
        Some(output.output_string("card_id", "test").unwrap().as_str())
    );
}
