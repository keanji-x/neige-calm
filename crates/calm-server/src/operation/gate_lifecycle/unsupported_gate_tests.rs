//! macOS must refuse both gate adapters before creating even a held wrapper.
use super::*;
use std::sync::Arc;

use crate::db::RouteRepo;
use crate::db::sqlite::SqlxRepo;
use crate::event::EventBus;
use crate::operation::task_gate_run::{FrozenRun, TaskGateRunAdapter, finalize};
use crate::operation::task_verify_adapter::target::{FrozenTarget, FrozenUnbound};
use crate::operation::task_verify_adapter::{FrozenVerify, TaskVerifyAdapter};
use crate::operation::{
    Operation, OperationCompletionBus, OperationOutcome, Phase, PhaseTag, ProviderAdapter,
    SpawnCtx, SqlxOperationRepo, TxOutput,
};
use crate::state::DaemonClient;
use crate::terminal_renderer::TerminalRendererRegistry;

async fn context() -> SpawnCtx {
    let repo = Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap());
    for sql in [
        "INSERT INTO areas(id,name,color,sort,kind,created_at,updated_at) VALUES('area','a','#000',0,'user',0,0)",
        "INSERT INTO tracks(id,area_id,title,sort,created_at,updated_at) VALUES('track','area','t',0,0,0)",
        "INSERT INTO tasks (
            id, track_id, key, kind, goal, context_json,
            status, gate_attempt, created_at_ms, updated_at_ms
        ) VALUES (
            'track:gate', 'track', 'gate', 'terminal', 'g', '{}',
            'verifying', 1, 0, 0
        )",
    ] {
        sqlx::query(sql).execute(repo.pool()).await.unwrap();
    }
    let route_repo: Arc<dyn RouteRepo> = repo.clone();
    SpawnCtx::new(
        route_repo.clone(),
        Arc::new(SqlxOperationRepo::new(repo.db().clone())),
        Arc::new(DaemonClient::new_stub()),
        TerminalRendererRegistry::new_with_repo(route_repo),
        EventBus::new(),
        OperationCompletionBus::new(),
    )
}

fn operation(output: &TxOutput, kind: &str) -> Operation {
    Operation {
        id: "unsupported-gate-op".into(),
        operation_key: "unsupported-gate-key".into(),
        kind: kind.into(),
        idempotency_key: None,
        payload_hash: String::new(),
        target_type: "task".into(),
        target_id: Some("track:gate".into()),
        target: Value::Null,
        payload: Value::Null,
        tx_output: Some(output.clone()),
        phase: Phase::SpawnStarted,
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

fn gate() -> GateSpec {
    GateSpec {
        cwd: None,
        timeout_secs: Some(1),
        steps: vec![GateStep {
            name: "sentinel".into(),
            cmd: "printf executed > step-executed".into(),
        }],
    }
}

// Each test owns its files. The invalid preflight input also prevents wrapper creation
// when the production platform refusal is removed; no child cleanup protocol is needed.
fn private_dir() -> tempfile::TempDir {
    let dir = tempfile::Builder::new()
        .prefix("unsupported-gate-")
        .tempdir()
        .unwrap();
    assert!(dir.path().is_dir());
    eprintln!(
        "unsupported_gate private directory: {}",
        dir.path().display()
    );
    dir
}

fn adapter_paths(dir: &Path) -> (PathBuf, PathBuf) {
    let cwd = dir.join("cwd-file");
    std::fs::write(&cwd, b"not a directory").unwrap();
    let logs = dir.join("logs");
    std::fs::create_dir(&logs).unwrap();
    assert!(cwd.is_file());
    assert!(!cwd.is_dir());
    assert!(logs.is_dir());
    (cwd, logs)
}

fn assert_no_artifacts(dir: &Path, paths: &GatePaths) {
    for path in [
        &paths.script,
        &paths.log,
        &paths.step,
        &paths.exit,
        &PathBuf::from(format!("{}.tmp", paths.exit.display())),
        &dir.join("step-executed"),
        &dir.join("cwd-file/step-executed"),
    ] {
        assert!(!path.exists(), "gate created {}", path.display());
    }
}

fn assert_pre_spawn_refusal<T>(result: Result<T>, dir: &Path, paths: &GatePaths) -> String {
    let error = match result {
        Err(error) => error,
        Ok(_) => panic!("unsupported gate was allowed to spawn"),
    };
    assert_no_artifacts(dir, paths);
    let CalmError::Internal(message) = &error else {
        panic!("expected unsupported platform refusal, got {error:?}");
    };
    assert_eq!(
        message,
        "gate-infra: gate execution unsupported on macos: descendant cleanup requires Linux /proc",
        "expected unsupported platform refusal"
    );
    error.to_string()
}

#[tokio::test]
async fn unsupported_gate_shared_entry_refuses_before_wrapper() {
    let dir = private_dir();
    let ctx = context().await;
    let output_parent = dir.path().join("output-file");
    std::fs::write(&output_parent, b"not a directory").unwrap();
    assert!(dir.path().is_dir());
    assert!(output_parent.is_file());
    let paths = GatePaths::new(&output_parent, "direct");
    assert_no_artifacts(dir.path(), &paths);
    // With a valid cwd, the platform refusal must precede the script/log writes.
    // If it is removed, the ordinary-file parent rejects the write before cmd.spawn.
    let result = spawn_held_identified(
        ctx.repo.as_ref(),
        dir.path(),
        &gate().steps,
        &paths,
        "direct",
    )
    .await;
    assert_pre_spawn_refusal(result, dir.path(), &paths);
    assert_eq!(std::fs::read(&output_parent).unwrap(), b"not a directory");
}

async fn verify_unbound(reason: Option<FrozenUnbound>) {
    let dir = private_dir();
    let (cwd, logs) = adapter_paths(dir.path());
    let ctx = context().await;
    let adapter = TaskVerifyAdapter::new(logs.clone());
    let mut output = TxOutput::new("task", Some("track:gate".into()), json!({}));
    output.data = serde_json::to_value(FrozenVerify {
        task_id: "track:gate".into(),
        track_id: "track".into(),
        area_id: "area".into(),
        key: "gate".into(),
        attempt: 1,
        cwd: cwd.display().to_string(),
        gate: gate(),
        target: FrozenTarget::Unbound {
            reason: reason.unwrap_or(FrozenUnbound::LegacyFrozen),
        },
    })
    .unwrap();
    if reason.is_none() {
        output.data.as_object_mut().unwrap().remove("target");
    }
    let op = operation(&output, "task-verify");
    let paths = GatePaths::new(&logs, "track:gate-g1");
    assert_no_artifacts(dir.path(), &paths);
    // The real adapter reaches the platform refusal; without it, spawn_held's
    // cwd.is_dir check rejects this still-existing file before writing or spawning.
    let result = adapter.spawn_side_effect(&output, &op, &ctx).await;
    let error = assert_pre_spawn_refusal(result, dir.path(), &paths);
    let plan = adapter
        .plan_compensation(PhaseTag::SpawnStarted, &error, &output, &op)
        .await
        .unwrap();
    let fail = plan
        .steps
        .iter()
        .find(|step| step.op == "fail_task_gate_infra")
        .unwrap();
    adapter
        .compensate_step(fail, &output, &op, &ctx)
        .await
        .unwrap();
    let (status, verdict): (String, String) =
        sqlx::query_as("SELECT status, gate_result_json FROM tasks WHERE id='track:gate'")
            .fetch_one(ctx.operation_repo.sqlite_db().read_pool())
            .await
            .unwrap();
    let verdict: Value = serde_json::from_str(&verdict).unwrap();
    assert_eq!(status, "failed");
    assert_eq!(verdict["passed"], false);
    assert_eq!(verdict["status_detail"], "gate-infra");
    assert_eq!(verdict["log_tail"], error);
    assert_no_artifacts(dir.path(), &paths);
    assert_eq!(std::fs::read(&cwd).unwrap(), b"not a directory");
}

#[tokio::test]
async fn unsupported_gate_terminal_unbound_refuses_before_wrapper() {
    verify_unbound(Some(FrozenUnbound::Terminal)).await;
}

#[tokio::test]
async fn unsupported_gate_legacy_lease_unbound_refuses_before_wrapper() {
    verify_unbound(Some(FrozenUnbound::LegacyLease)).await;
}

#[tokio::test]
async fn unsupported_gate_legacy_frozen_unbound_refuses_before_wrapper() {
    verify_unbound(None).await;
}

#[tokio::test]
async fn unsupported_gate_task_gate_run_refuses_before_wrapper() {
    let dir = private_dir();
    let (cwd, logs) = adapter_paths(dir.path());
    let ctx = context().await;
    let adapter = TaskGateRunAdapter::new(logs.clone());
    let mut output = TxOutput::new("task", Some("track:gate".into()), json!({}));
    output.data = serde_json::to_value(FrozenRun {
        task_id: "track:gate".into(),
        track_id: "track".into(),
        card_id: "worker".into(),
        run: 1,
        cwd: cwd.display().to_string(),
        branch: "test-branch".into(),
        base_sha: "unused-before-release".into(),
        canonical_path: dir.path().display().to_string(),
        git_common_dir: dir.path().join(".git").display().to_string(),
        ref_name: "refs/neige/gate-runs/track/worker/r1".into(),
        message: "must not checkpoint".into(),
        gate: gate(),
    })
    .unwrap();
    let op = operation(&output, "task-gate-run");
    let paths = GatePaths::new(&logs, "track:gate-r1");
    assert_no_artifacts(dir.path(), &paths);
    // refs_digest's git spawn fails for this cwd and returns None; the adapter
    // still reaches spawn_held_identified with the same safe preflight input.
    let result = adapter.spawn_side_effect(&output, &op, &ctx).await;
    let error = assert_pre_spawn_refusal(result, dir.path(), &paths);
    let result = finalize::terminal_result(
        1,
        &paths.log,
        OperationOutcome::Failed {
            last_error: error.clone(),
            from_phase: PhaseTag::SpawnStarted,
            last_error_class: None,
        },
    );
    assert!(!result.verdict.passed);
    assert_eq!(result.verdict.status_detail.as_deref(), Some("gate-infra"));
    assert_eq!(result.verdict.log_tail, error);
    assert!(result.commit.is_none());
    assert_no_artifacts(dir.path(), &paths);
    assert_eq!(std::fs::read(&cwd).unwrap(), b"not a directory");
}
