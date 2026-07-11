use super::*;
use crate::db::sqlite::begin_immediate_tx;
use crate::event::EventBus;
use crate::operation::workspace_lease::release_workspace_lease_for_card_repo;
use crate::operation::{OperationKey, OperationRepo, SqlxOperationRepo};
use sqlx::Row;
use std::path::Path;
use std::process::Command;
use std::sync::Arc;

struct WorkerLeaseHarness {
    repo: Arc<crate::db::sqlite::SqlxRepo>,
    adapter: CodexWorkerAdapter,
    wave_id: String,
    events: EventBus,
    repo_root: tempfile::TempDir,
}

async fn worker_lease_harness() -> WorkerLeaseHarness {
    let repo_root = tempfile::tempdir().unwrap();
    init_git_repo(repo_root.path());
    let repo = Arc::new(
        crate::db::sqlite::SqlxRepo::open("sqlite::memory:")
            .await
            .unwrap(),
    );
    let cove = crate::db::RepoSyncDomainRaw::cove_create(
        repo.as_ref(),
        crate::model::NewCove {
            name: "workspace leases".into(),
            color: "#101010".into(),
            sort: None,
        },
    )
    .await
    .unwrap();
    let wave = crate::db::RepoSyncDomainRaw::wave_create(
        repo.as_ref(),
        crate::model::NewWave {
            workflow_input: None,
            cove_id: cove.id,
            title: "workspace leases".into(),
            sort: None,
            cwd: repo_root.path().display().to_string(),
            workflow_id: None,
            attach_folder: false,
            theme: RequestTheme::default_dark(),
        },
    )
    .await
    .unwrap();
    let route_repo: Arc<dyn crate::db::RouteRepo> = repo.clone();
    let full_repo: Arc<dyn crate::db::Repo> = repo.clone();
    WorkerLeaseHarness {
        adapter: CodexWorkerAdapter::new(
            route_repo,
            Arc::new(CodexClient::new_stub()),
            SharedCodexAppServer::new_stub(full_repo),
            None,
            CardRoleCache::new(),
            WaveCoveCache::new(),
        ),
        repo,
        wave_id: wave.id.to_string(),
        events: EventBus::new(),
        repo_root,
    }
}

fn worker_payload(wave_id: &str, key: &str) -> Value {
    serde_json::to_value(CodexWorkerOperationPayload {
        actor: ActorId::KernelDispatcher,
        wave_id: wave_id.to_string(),
        idempotency_key: format!("{wave_id}:{key}"),
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
        wave_id: "wave-hash".into(),
        idempotency_key: "wave-hash:task-a".into(),
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
        "wave_id": "wave-hash",
        "idempotency_key": "wave-hash:task-a",
        "goal": "do task-a",
        "context": { "from": "legacy" },
    });
    assert_eq!(
        crate::routes::terminal_cards::stable_payload_hash(&payload).unwrap(),
        crate::routes::terminal_cards::stable_payload_hash(&legacy_without_cwd).unwrap()
    );

    let task_with_cwd = crate::model::Task {
        id: "wave-hash:task-a".into(),
        wave_id: "wave-hash".into(),
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
        crate::routes::terminal_cards::stable_payload_hash(&built).unwrap(),
        crate::routes::terminal_cards::stable_payload_hash(&legacy_without_cwd).unwrap()
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
    let payload = worker_payload(&harness.wave_id, key);
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
    let output = harness
        .adapter
        .prepare_tx(&mut tx, &payload, &op)
        .await
        .unwrap();
    let events = output.post_commit_events.clone();
    tx.commit().await.unwrap();
    (output, events)
}

#[test]
fn render_worker_prompt_goal_only() {
    let out = render_worker_prompt("fix the bug", &Value::Null, None);
    assert_eq!(out, "Goal:\nfix the bug");
}

#[test]
fn render_worker_prompt_goal_plus_context() {
    let ctx = serde_json::json!({ "issue": 42, "title": "x" });
    let out = render_worker_prompt("fix it", &ctx, None);
    assert!(out.starts_with("Goal:\nfix it"));
    assert!(out.contains("\n\nContext:\n"));
    assert!(out.contains("\"issue\": 42"));
    assert!(out.contains("\"title\": \"x\""));
    assert!(!out.contains("Acceptance criteria"));
}

#[test]
fn render_worker_prompt_goal_plus_context_plus_ac() {
    let ctx = serde_json::json!({ "pr": 7 });
    let out = render_worker_prompt("ship", &ctx, Some("tests pass"));
    assert!(out.contains("Goal:\nship"));
    assert!(out.contains("\n\nContext:\n"));
    assert!(out.contains("\"pr\": 7"));
    assert!(out.ends_with("Acceptance criteria:\ntests pass"));
}

#[test]
fn render_worker_prompt_skips_empty_context_object() {
    let out = render_worker_prompt("g", &serde_json::json!({}), Some("ac"));
    assert!(
        !out.contains("Context"),
        "empty {{}} should be skipped: {out}"
    );
    assert!(out.contains("Acceptance criteria:\nac"));
}

#[test]
fn render_worker_prompt_skips_blank_ac() {
    let out = render_worker_prompt("g", &Value::Null, Some("   "));
    assert_eq!(out, "Goal:\ng");
}

#[tokio::test]
async fn codex_worker_prepare_acquires_held_workspace_lease_cwd() {
    let harness = worker_lease_harness().await;
    let (output, events) = prepare_worker(&harness, "a").await;
    let card_id = output.output_string("card_id", "test").unwrap();
    let lease_id = output.output_string("lease_id", "test").unwrap();
    let cwd = output.output_string("cwd", "test").unwrap();

    let cwd_path = std::path::Path::new(&cwd);
    assert!(cwd_path.is_absolute());
    assert!(cwd_path.starts_with(harness.repo_root.path()));
    assert!(
        cwd_path.parent().unwrap().is_dir(),
        "leased cwd parent exists"
    );
    assert!(
        !cwd_path.exists(),
        "leased cwd leaf is left for git worktree add"
    );
    let row = sqlx::query(
        "SELECT state, path, card_id, wave_id FROM workspace_leases WHERE lease_id = ?1",
    )
    .bind(&lease_id)
    .fetch_one(harness.repo.pool())
    .await
    .unwrap();
    assert_eq!(row.get::<String, _>("state"), "held");
    assert_eq!(row.get::<String, _>("path"), cwd);
    assert_eq!(row.get::<String, _>("card_id"), card_id);
    assert_eq!(row.get::<String, _>("wave_id"), harness.wave_id);
    assert_eq!(events.len(), 1);
    assert!(matches!(events[0].event, Event::WorkspaceLeased { .. }));

    assert!(
        release_workspace_lease_for_card_repo(harness.repo.as_ref(), &harness.events, &card_id)
            .await
            .unwrap()
    );
    assert!(
        !std::path::Path::new(&cwd).exists(),
        "lease acquisition leaves cwd leaf absent until provisioning"
    );
}

#[tokio::test]
async fn codex_worker_budget_parallelism_gets_disjoint_lease_paths() {
    let harness = worker_lease_harness().await;
    let (first, _) = prepare_worker(&harness, "a").await;
    let (second, _) = prepare_worker(&harness, "b").await;
    let first_card = first.output_string("card_id", "test").unwrap();
    let second_card = second.output_string("card_id", "test").unwrap();
    let first_cwd = first.output_string("cwd", "test").unwrap();
    let second_cwd = second.output_string("cwd", "test").unwrap();

    assert_ne!(first_card, second_card);
    assert_ne!(first_cwd, second_cwd);
    assert!(std::path::Path::new(&first_cwd).is_absolute());
    assert!(std::path::Path::new(&second_cwd).is_absolute());

    let held: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM workspace_leases WHERE state = 'held'")
            .fetch_one(harness.repo.pool())
            .await
            .unwrap();
    assert_eq!(held, 2);

    release_workspace_lease_for_card_repo(harness.repo.as_ref(), &harness.events, &first_card)
        .await
        .unwrap();
    release_workspace_lease_for_card_repo(harness.repo.as_ref(), &harness.events, &second_card)
        .await
        .unwrap();
}

#[tokio::test]
async fn workspace_lease_release_flips_row_and_persists_event() {
    let harness = worker_lease_harness().await;
    let (output, _) = prepare_worker(&harness, "a").await;
    let card_id = output.output_string("card_id", "test").unwrap();
    let lease_id = output.output_string("lease_id", "test").unwrap();

    assert!(
        release_workspace_lease_for_card_repo(harness.repo.as_ref(), &harness.events, &card_id)
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
        !release_workspace_lease_for_card_repo(harness.repo.as_ref(), &harness.events, &card_id)
            .await
            .unwrap(),
        "release is idempotent after the row is released"
    );
}

#[tokio::test]
async fn codex_worker_compensation_removes_workspace_before_row_release() {
    let harness = worker_lease_harness().await;
    let (output, _) = prepare_worker(&harness, "a").await;
    let op = worker_op("op-a", Value::Null);
    let state = harness
        .adapter
        .plan_compensation(PhaseTag::SpawnStarted, "boom", &output, &op)
        .await
        .unwrap();

    assert_eq!(state.steps[0].op, "remove_workspace_artifact");
    assert_eq!(state.steps[1].op, "release_workspace_lease");
    assert_eq!(state.steps[2].op, "cleanup_codex_worker");
    let lease_id = output.output_string("lease_id", "test").unwrap();
    assert_eq!(
        state.steps[1].arg_string("lease_id", "test").unwrap(),
        lease_id
    );
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
