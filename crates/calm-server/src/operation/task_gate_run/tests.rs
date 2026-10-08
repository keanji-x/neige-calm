use std::sync::Arc;

use serde_json::json;

use super::*;
use crate::db::RouteRepo;
use crate::db::sqlite::SqlxRepo;
use crate::event::EventBus;
use crate::operation::{
    OperationCompletionBus, Phase, SpawnCtx, SqlxOperationRepo, TxOutput as Output,
};
use crate::state::DaemonClient;
use crate::terminal_renderer::TerminalRendererRegistry;

#[test]
fn run_key_round_trip() {
    assert_eq!(gate_run_key("t:impl", 3), "t:impl#r3");
    assert_eq!(parse_run_key("t:impl#r3"), Some(("t:impl", 3)));
    assert_eq!(parse_run_key("t:impl#r0"), None, "run >= 1");
    assert_eq!(parse_run_key("t:impl#g3"), None);
    assert_eq!(parse_run_key("#r2"), None, "empty task id");
}

async fn spawn_ctx() -> SpawnCtx {
    let repo = Arc::new(
        SqlxRepo::open("sqlite::memory:")
            .await
            .expect("open in-memory sqlite"),
    );
    let route_repo: Arc<dyn RouteRepo> = repo.clone();
    let completion = OperationCompletionBus::new();
    SpawnCtx::new(
        route_repo.clone(),
        Arc::new(SqlxOperationRepo::new(repo.pool().clone())),
        Arc::new(DaemonClient::new_stub()),
        TerminalRendererRegistry::new_with_repo(route_repo),
        EventBus::new(),
        completion,
    )
}

fn parked_op(key: &str, artifacts: &SpawnArtifacts) -> Operation {
    Operation {
        id: "op-run".into(),
        operation_key: "op-key".into(),
        kind: TASK_GATE_RUN_KIND.into(),
        idempotency_key: Some(key.into()),
        payload_hash: String::new(),
        target_type: "track".into(),
        target_id: None,
        target: json!({}),
        payload: json!({}),
        tx_output: Some(Output::new("task", None, json!({}))),
        phase: Phase::Parked,
        phase_detail: None,
        attempt: 0,
        last_error: None,
        compensation_state: None,
        lease_owner: None,
        lease_until_ms: None,
        spawn_artifacts: Some(artifacts.clone()),
        parked_at_ms: Some(0),
        parked_deadline_ms: Some(i64::MAX),
    }
}

/// R11 (D9): the worker is alive and same-user, so it can write the run's exit file; recovery of
/// a run whose leader is gone never reads it. A forged `0` is still `gate-infra`, at boot and in
/// the steady-state probe alike.
#[tokio::test]
async fn run_recovery_never_reads_the_exit_file() {
    let dir = tempfile::tempdir().expect("tempdir");
    let exit_path = dir.path().join("t-r1.exit");
    std::fs::write(&exit_path, "0\n").expect("forge the exit file");
    let artifacts = SpawnArtifacts {
        pid: i32::MAX - 7,
        pgid: i32::MAX - 7,
        start_time: 1,
        boot_id: "another-boot".into(),
        log_path: Some(dir.path().join("t-r1.log").display().to_string()),
        extra: json!({
            "exit_path": exit_path.display().to_string(),
            "step_path": dir.path().join("t-r1.step").display().to_string(),
        }),
    };
    let adapter = TaskGateRunAdapter::new(dir.path().to_path_buf());
    let op = parked_op("t#r1", &artifacts);
    let ctx = spawn_ctx().await;
    for mode in [RecoveryMode::PreDeadlineProbe, RecoveryMode::Boot] {
        let recovery = adapter
            .recover_parked(&op, &artifacts, false, mode, &ctx)
            .await
            .expect("recovery decides");
        match recovery {
            ParkedRecovery::Fail { reason } => {
                assert!(reason.starts_with("gate-infra"), "{mode:?}: {reason}");
            }
            other => panic!("{mode:?}: a dead leader must fail the run as infra, got {other:?}"),
        }
    }
}
