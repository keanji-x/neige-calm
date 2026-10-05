use super::*;
use crate::db::sqlite::begin_immediate_tx;
use crate::event::EventBus;
use crate::git_candidate::delivery::AttemptOutcome;
use crate::operation::workspace_lease::{ReleaseDelivery, release_workspace_lease_for_card_repo};
use crate::operation::{OperationCompletionBus, OperationKey, OperationRepo, SqlxOperationRepo};
use crate::state::DaemonClient;
use crate::terminal_renderer::TerminalRendererRegistry;
use sqlx::Row;
use std::path::Path;
use std::process::Command;
use std::sync::Arc;

struct WorkerLeaseHarness {
    repo: Arc<crate::db::sqlite::SqlxRepo>,
    adapter: CodexWorkerAdapter,
    track_id: String,
    events: EventBus,
    repo_root: tempfile::TempDir,
    /// The track worktree (#1830 S2): where every worker of this track runs.
    worktree: std::path::PathBuf,
}

async fn worker_lease_harness() -> WorkerLeaseHarness {
    let repo_root = tempfile::tempdir().unwrap();
    init_git_repo(repo_root.path());
    let repo = Arc::new(
        crate::db::sqlite::SqlxRepo::open("sqlite::memory:")
            .await
            .unwrap(),
    );
    let area = crate::db::RepoSyncDomainRaw::area_create(
        repo.as_ref(),
        crate::model::NewArea {
            name: "workspace leases".into(),
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
            title: "workspace leases".into(),
            sort: None,
            cwd: repo_root.path().display().to_string(),
            template_id: None,
            plugin_scope: None,
            attach_folder: false,
            theme: RequestTheme::default_dark(),
        },
    )
    .await
    .unwrap();
    let worktree = crate::test_support::attach_track_worktree(
        repo.pool(),
        track.id.as_str(),
        repo_root.path(),
    )
    .await;
    let route_repo: Arc<dyn crate::db::RouteRepo> = repo.clone();
    let full_repo: Arc<dyn crate::db::Repo> = repo.clone();
    WorkerLeaseHarness {
        adapter: CodexWorkerAdapter::new(
            route_repo,
            Arc::new(CodexClient::new_stub()),
            SharedCodexAppServer::new_stub(full_repo),
            None,
            CardRoleCache::new(),
            TrackAreaCache::new(),
            std::env::temp_dir().join("neige-calm-test-unused-workspace-root"),
        ),
        repo,
        track_id: track.id.to_string(),
        events: EventBus::new(),
        repo_root,
        worktree,
    }
}

fn worker_payload(track_id: &str, key: &str) -> Value {
    serde_json::to_value(CodexWorkerOperationPayload {
        actor: ActorId::KernelDispatcher,
        track_id: track_id.to_string(),
        idempotency_key: format!("{track_id}:{key}"),
        goal: format!("do {key}"),
        cwd: None,
        context: Value::Null,
        acceptance_criteria: None,
    })
    .unwrap()
}

#[test]
fn codex_worker_payload_omits_none_cwd_for_hash_stability() {
    let payload = CodexWorkerOperationPayload {
        actor: ActorId::KernelDispatcher,
        track_id: "track-hash".into(),
        idempotency_key: "track-hash:task-a".into(),
        goal: "do task-a".into(),
        cwd: None,
        context: json!({ "from": "legacy" }),
        acceptance_criteria: None,
    };
    let serialized = serde_json::to_value(&payload).unwrap();
    assert!(
        !serialized.as_object().unwrap().contains_key("cwd"),
        "None cwd must serialize as absent for pre-upgrade hash parity"
    );

    let legacy_without_cwd = json!({
        "actor": serde_json::to_value(ActorId::KernelDispatcher).unwrap(),
        "track_id": "track-hash",
        "idempotency_key": "track-hash:task-a",
        "goal": "do task-a",
        "context": { "from": "legacy" },
    });
    assert_eq!(
        crate::routes::idempotency_key::stable_payload_hash(&payload).unwrap(),
        crate::routes::idempotency_key::stable_payload_hash(&legacy_without_cwd).unwrap()
    );

    let task_with_cwd = crate::model::Task {
        id: "track-hash:task-a".into(),
        track_id: "track-hash".into(),
        key: "task-a".into(),
        kind: crate::model::TaskKind::Codex,
        goal: "do task-a".into(),
        context_json: json!({ "from": "legacy" }).to_string(),
        acceptance_criteria: None,
        cwd: Some("/repo/from-plan-upsert".into()),
        depends_on_json: "[]".into(),
        priority: 0,
        gate_json: None,
        status: crate::model::TaskStatus::Pending,
        status_detail: None,
        worker_card_id: None,
        gate_result_json: None,
        gate_attempt: 0,
        gate_pid: None,
        gate_pid_starttime: None,
        gate_pid_boot_id: None,
        running_deadline_ms: None,
        context_stale_at_ms: None,
        declared_by: "spec".into(),
        spawn: "in-wave".into(),
        access: crate::model::TaskAccess::ReadWrite,
        start: crate::model::TaskStart::Checkout,
        created_at_ms: 1,
        updated_at_ms: 1,
        finished_at_ms: None,
    };
    let (kind, built) = crate::scheduler::build_worker_payload(&task_with_cwd).unwrap();
    assert_eq!(kind, "codex-worker");
    assert!(
        !built.as_object().unwrap().contains_key("cwd"),
        "build_worker_payload must not leak task.cwd into codex op identity"
    );
    assert_eq!(
        crate::routes::idempotency_key::stable_payload_hash(&built).unwrap(),
        crate::routes::idempotency_key::stable_payload_hash(&legacy_without_cwd).unwrap()
    );
}

fn worker_op(id: &str, payload: Value) -> Operation {
    Operation {
        id: id.to_string(),
        operation_key: format!("op-key-{id}"),
        kind: "codex-worker".into(),
        idempotency_key: Some(id.to_string()),
        payload_hash: "hash".into(),
        target_type: "unknown".into(),
        target_id: None,
        target: json!({ "type": "unknown", "id": null }),
        payload,
        tx_output: None,
        phase: Phase::Pending,
        phase_detail: None,
        attempt: 0,
        last_error: None,
        compensation_state: None,
        lease_owner: None,
        lease_until_ms: None,
        spawn_artifacts: None,
        parked_at_ms: None,
        parked_deadline_ms: None,
    }
}

async fn prepare_worker(
    harness: &WorkerLeaseHarness,
    key: &str,
) -> (TxOutput, Vec<BroadcastEnvelope>) {
    prepare_worker_with_task_key(harness, key, key).await
}

/// Same flow as [`prepare_worker`] but lets the test choose the `tasks.key` column independently of the operation key.
async fn prepare_worker_with_task_key(
    harness: &WorkerLeaseHarness,
    key: &str,
    task_key: &str,
) -> (TxOutput, Vec<BroadcastEnvelope>) {
    let (output, events, _op) = prepare_worker_and_op(harness, key, task_key).await;
    (output, events)
}

/// [`prepare_worker_with_task_key`] plus the claimed operation row, for a
/// test that carries the op past prepare the way the driver does.
async fn prepare_worker_and_op(
    harness: &WorkerLeaseHarness,
    key: &str,
    task_key: &str,
) -> (TxOutput, Vec<BroadcastEnvelope>, Operation) {
    let (output, op) = try_prepare_worker_and_op(harness, key, task_key)
        .await
        .unwrap();
    let events = output.post_commit_events.clone();
    (output, events, op)
}

/// [`prepare_worker_and_op`] that returns the prepare's refusal; a refused prepare commits
/// nothing. A `tasks` row the test inserted first is kept.
async fn try_prepare_worker_and_op(
    harness: &WorkerLeaseHarness,
    key: &str,
    task_key: &str,
) -> Result<(TxOutput, Operation)> {
    let payload = worker_payload(&harness.track_id, key);
    let task_id = format!("{}:{key}", harness.track_id);
    sqlx::query(
        "INSERT OR IGNORE INTO tasks \
         (id, track_id, key, kind, goal, context_json, depends_on_json, status, created_at_ms, updated_at_ms) \
         VALUES (?1, ?2, ?3, 'codex', 'test', 'null', '[]', 'dispatched', 1, 1)",
    )
    .bind(&task_id)
    .bind(&harness.track_id)
    .bind(task_key)
    .execute(harness.repo.pool())
    .await
    .unwrap();
    let op_repo = SqlxOperationRepo::new(harness.repo.pool().clone());
    let op_id = op_repo
        .insert_operation(
            "codex-worker",
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
    let output = harness.adapter.prepare_tx(&mut tx, &payload, &op).await?;
    tx.commit().await.unwrap();
    Ok((output, op))
}

#[tokio::test]
async fn codex_worker_prepare_acquires_held_workspace_lease_cwd() {
    let harness = worker_lease_harness().await;
    let (output, events) = prepare_worker(&harness, "a").await;
    let card_id = output.output_string("card_id", "test").unwrap();
    let lease_id = output.output_string("lease_id", "test").unwrap();
    let cwd = output.output_string("cwd", "test").unwrap();

    assert_eq!(
        std::path::Path::new(&cwd),
        harness.worktree,
        "the worker runs in the track worktree (#1830 S2)"
    );
    assert_eq!(
        output.output_string("branch", "test").unwrap(),
        format!("neige/track-{}", harness.track_id)
    );
    let row = sqlx::query(
        "SELECT state, path, card_id, track_id FROM workspace_leases WHERE lease_id = ?1",
    )
    .bind(&lease_id)
    .fetch_one(harness.repo.pool())
    .await
    .unwrap();
    assert_eq!(row.get::<String, _>("state"), "held");
    assert_eq!(row.get::<String, _>("path"), cwd);
    assert_eq!(row.get::<String, _>("card_id"), card_id);
    assert_eq!(row.get::<String, _>("track_id"), harness.track_id);
    assert_eq!(events.len(), 1);
    assert!(matches!(events[0].event, Event::WorkspaceLeased { .. }));

    assert!(
        release_workspace_lease_for_card_repo(
            harness.repo.as_ref(),
            &harness.events,
            &card_id,
            ReleaseDelivery::Commit(AttemptOutcome::Completed),
        )
        .await
        .unwrap()
    );
    assert!(
        std::path::Path::new(&cwd).join("README.md").is_file(),
        "releasing a lease leaves the track's checkout in place"
    );
}

#[tokio::test]
async fn workspace_lease_release_flips_row_and_persists_event() {
    let harness = worker_lease_harness().await;
    let (output, _) = prepare_worker(&harness, "a").await;
    let card_id = output.output_string("card_id", "test").unwrap();
    let lease_id = output.output_string("lease_id", "test").unwrap();

    assert!(
        release_workspace_lease_for_card_repo(
            harness.repo.as_ref(),
            &harness.events,
            &card_id,
            ReleaseDelivery::Commit(AttemptOutcome::Completed),
        )
        .await
        .unwrap()
    );
    let state: String =
        sqlx::query_scalar("SELECT state FROM workspace_leases WHERE lease_id = ?1")
            .bind(&lease_id)
            .fetch_one(harness.repo.pool())
            .await
            .unwrap();
    assert_eq!(state, "released");
    let released_events: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM events WHERE kind = 'workspace.released'")
            .fetch_one(harness.repo.pool())
            .await
            .unwrap();
    assert_eq!(released_events, 1);
    let removed_events: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM events WHERE kind = 'worktree.removed'")
            .fetch_one(harness.repo.pool())
            .await
            .unwrap();
    assert_eq!(removed_events, 0);

    assert!(
        !release_workspace_lease_for_card_repo(
            harness.repo.as_ref(),
            &harness.events,
            &card_id,
            ReleaseDelivery::Commit(AttemptOutcome::Completed),
        )
        .await
        .unwrap(),
        "release is idempotent after the row is released"
    );
}

#[tokio::test]
async fn codex_worker_compensation_releases_the_lease_last() {
    let harness = worker_lease_harness().await;
    let (output, _) = prepare_worker(&harness, "a").await;
    let op = worker_op("op-a", Value::Null);
    let state = harness
        .adapter
        .plan_compensation(PhaseTag::SpawnStarted, "boom", &output, &op)
        .await
        .unwrap();

    // After the worker rows are gone (#1830 S2 D7); nothing on disk is removed.
    assert_eq!(state.steps.len(), 2);
    assert_eq!(state.steps[0].op, "cleanup_codex_worker");
    assert_eq!(state.steps[1].op, "release_workspace_lease");
    let lease_id = output.output_string("lease_id", "test").unwrap();
    assert_eq!(
        state.steps[1].arg_string("lease_id", "test").unwrap(),
        lease_id
    );
}

#[tokio::test]
async fn codex_worker_prepare_titles_card_with_task_key() {
    let harness = worker_lease_harness().await;
    let (output, _) = prepare_worker(&harness, "slice-b").await;
    let card_id = output.output_string("card_id", "test").unwrap();

    let stored: Option<String> = sqlx::query_scalar("SELECT title FROM cards WHERE id = ?1")
        .bind(&card_id)
        .fetch_one(harness.repo.pool())
        .await
        .unwrap();
    assert_eq!(stored, Some("slice-b".to_string()));

    let wire: crate::model::Card = serde_json::from_value(output.result.clone()).unwrap();
    assert_eq!(wire.title, Some("slice-b".to_string()));
}

#[tokio::test]
async fn codex_worker_prepare_leaves_title_none_for_blank_task_key() {
    let harness = worker_lease_harness().await;
    let (output, _) = prepare_worker_with_task_key(&harness, "blank", "").await;
    let card_id = output.output_string("card_id", "test").unwrap();

    let stored: Option<String> = sqlx::query_scalar("SELECT title FROM cards WHERE id = ?1")
        .bind(&card_id)
        .fetch_one(harness.repo.pool())
        .await
        .unwrap();
    assert_eq!(stored, None, "blank task key must not title the card");

    let wire: crate::model::Card = serde_json::from_value(output.result.clone()).unwrap();
    assert_eq!(wire.title, None);
}

/// Exercised directly because every worker adapter runs `refuse_if_context_stale` first, which already refuses a missing row.
#[tokio::test]
async fn task_key_for_card_title_is_fail_soft() {
    let harness = worker_lease_harness().await;
    let mut tx = begin_immediate_tx(harness.repo.pool()).await.unwrap();
    assert_eq!(
        crate::operation::task_key_for_card_title(&mut tx, "no-such-task").await,
        None
    );
    assert_eq!(
        crate::operation::task_key_for_card_title(&mut tx, "").await,
        None
    );
    tx.commit().await.unwrap();
}

/// Dropping the table inside the transaction is the cheapest way to make the statement itself fail for real.
#[tokio::test]
async fn task_key_for_card_title_swallows_a_failing_select() {
    let harness = worker_lease_harness().await;
    let mut tx = begin_immediate_tx(harness.repo.pool()).await.unwrap();
    sqlx::query("DROP TABLE tasks")
        .execute(&mut *tx)
        .await
        .unwrap();
    // The statement really is broken now — otherwise this test would pass for the wrong reason (a missing row).
    assert!(
        sqlx::query_scalar::<_, String>("SELECT key FROM tasks WHERE id = ?1")
            .bind("any")
            .fetch_optional(&mut *tx)
            .await
            .is_err()
    );
    assert_eq!(
        crate::operation::task_key_for_card_title(&mut tx, "any-task").await,
        None
    );
    tx.rollback().await.unwrap();
}

/// D3 — the spawn only verifies: the codex spawn entry (`verify_codex_worker_workspace`, carried
/// to `app_server_interact` with the repo's own transitions so its checkpoint lands) refuses a
/// checkout whose HEAD moved after the prepare tx recorded its base, naming both commits.
#[tokio::test]
async fn codex_spawn_refuses_a_checkout_that_moved_after_prepare() {
    let harness = worker_lease_harness().await;
    let (mut output, _, op) = prepare_worker_and_op(&harness, "pinned", "pinned").await;
    let card_id = output.output_string("card_id", "test").unwrap();
    let cwd = output.output_string("cwd", "test").unwrap();
    let recorded_base = output.output_string("base_sha", "test").unwrap();
    assert_eq!(git_head(Path::new(&cwd)), recorded_base);

    let op_repo = Arc::new(SqlxOperationRepo::new(harness.repo.pool().clone()));
    let kind = harness
        .adapter
        .app_server_interact_kind(&output, &op)
        .unwrap();
    op_repo
        .set_phase(&op, Phase::AppServerInteract { kind })
        .await
        .unwrap()
        .expect("the claimed op moves to app_server_interact");
    let op = op_repo
        .claim_drive_batch(1)
        .await
        .unwrap()
        .into_iter()
        .find(|claimed| claimed.id == op.id)
        .expect("the op is re-claimed in app_server_interact");
    let route_repo: Arc<dyn crate::db::RouteRepo> = harness.repo.clone();
    let ctx = SpawnCtx::new(
        route_repo,
        op_repo,
        Arc::new(DaemonClient::new_stub()),
        TerminalRendererRegistry::new(),
        harness.events.clone(),
        OperationCompletionBus::new(),
    );
    verify_codex_worker_workspace(
        &ctx,
        &harness.adapter.card_role_cache,
        &harness.adapter.track_area_cache,
        &op,
        &mut output,
    )
    .await
    .expect("the unmoved checkout verifies");

    // The checkout moves on; a second spawn attempt is refused.
    run_git(
        Path::new(&cwd),
        ["commit", "--allow-empty", "-m", "moved after prepare"],
    );
    let moved_head = git_head(Path::new(&cwd));
    assert_ne!(moved_head, recorded_base, "test setup moved HEAD");
    let err = verify_codex_worker_workspace(
        &ctx,
        &harness.adapter.card_role_cache,
        &harness.adapter.track_area_cache,
        &op,
        &mut output,
    )
    .await
    .expect_err("a moved checkout fails the spawn");
    let message = err.to_string();
    assert!(
        message.contains(&recorded_base) && message.contains(&moved_head),
        "{message}"
    );

    release_workspace_lease_for_card_repo(
        harness.repo.as_ref(),
        &harness.events,
        &card_id,
        ReleaseDelivery::Commit(AttemptOutcome::Completed),
    )
    .await
    .unwrap();
}

fn git_head(dir: &Path) -> String {
    let output = Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(dir)
        .output()
        .unwrap();
    assert!(output.status.success());
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

fn init_git_repo(path: &Path) {
    std::fs::create_dir_all(path).unwrap();
    run_git(path, ["init"]);
    run_git(path, ["config", "user.email", "codex-worker@example.test"]);
    run_git(path, ["config", "user.name", "Codex Worker Test"]);
    std::fs::write(path.join("README.md"), "initial\n").unwrap();
    run_git(path, ["add", "README.md"]);
    run_git(path, ["commit", "-m", "initial"]);
}

fn run_git<const N: usize>(repo: &Path, args: [&str; N]) {
    let output = Command::new("git")
        .args(args)
        .current_dir(repo)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {:?} failed in {}\nstdout:\n{}\nstderr:\n{}",
        args,
        repo.display(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[tokio::test]
async fn codex_worker_prompt_includes_completion_task_id() {
    let harness = worker_lease_harness().await;
    let (output, _) = prepare_worker(&harness, "identity").await;
    assert!(
        output
            .output_string("prompt", "test")
            .unwrap()
            .contains(&format!("{}:identity", harness.track_id)),
        "Codex must receive the exact task id it is required to report"
    );
}

#[cfg(test)]
mod viewer_cleanup_tests;

#[cfg(test)]
mod reader_head_tests;

/// #1727 S4 slice 2 — the lease the codex worker's `prepare_tx` takes is a kernel-delivery
/// lease (`delivery_policy = 'kernel'`, written in the same INSERT as its base); the
/// fixtures-only plain lease stays NULL (legacy).
#[tokio::test]
async fn worker_lease_is_kernel_policy() {
    let harness = worker_lease_harness().await;
    let (output, _) = prepare_worker(&harness, "kernel").await;
    let card_id = output.output_string("card_id", "test").unwrap();
    let policy: Option<String> =
        sqlx::query_scalar("SELECT delivery_policy FROM workspace_leases WHERE card_id = ?1")
            .bind(&card_id)
            .fetch_one(harness.repo.pool())
            .await
            .unwrap();
    assert_eq!(policy.as_deref(), Some("kernel"));
    let mut tx = begin_immediate_tx(harness.repo.pool()).await.unwrap();
    let lease = crate::operation::workspace_lease::facts::workspace_lease_by_id_tx(
        &mut tx,
        &sqlx::query_scalar::<_, String>(
            "SELECT lease_id FROM workspace_leases WHERE card_id = ?1",
        )
        .bind(&card_id)
        .fetch_one(harness.repo.pool())
        .await
        .unwrap(),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(
        lease.delivery_policy,
        Some(crate::operation::workspace_lease::DeliveryPolicy::Kernel)
    );
    assert!(lease.base.is_some());

    let plain_card = format!("{card_id}-plain");
    let plain_path = harness.repo_root.path().join("plain-lease");
    let (plain, _event) = crate::operation::workspace_lease::acquire_plain_workspace_lease_tx(
        &mut tx,
        &plain_card,
        &harness.track_id,
        "op-plain",
        &plain_path,
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    assert_eq!(plain.delivery_policy, None);
    let policy: Option<String> =
        sqlx::query_scalar("SELECT delivery_policy FROM workspace_leases WHERE card_id = ?1")
            .bind(&plain_card)
            .fetch_one(harness.repo.pool())
            .await
            .unwrap();
    assert_eq!(policy, None, "the plain fixture lease is legacy");
}
