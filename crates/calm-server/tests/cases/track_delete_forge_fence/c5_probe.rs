//! Real forge admissions shared by teardown and probe-race regressions.
use super::*;
use calm_server::operation::{
    OperationCompletionBus, ParkedObserver, Phase, ProviderAdapter, SpawnCtx, SpawnOutcome,
    forge_action_adapter::ForgeActionAdapter,
};
use workspace_runtime::execution::{Desired, ExecutionId};

fn context(boot: &Boot) -> SpawnCtx {
    SpawnCtx::new(
        boot.repo.clone(),
        Arc::new(SqlxOperationRepo::new(
            boot.repo.sqlite_db().unwrap().clone(),
        )),
        Arc::new(DaemonClient::new_stub()),
        calm_server::terminal_renderer::TerminalRendererRegistry::new_with_repo(boot.repo.clone()),
        EventBus::new(),
        OperationCompletionBus::new(),
    )
    .with_execution(boot.execution.lifecycle.clone())
    .with_location_admission(boot.state.location_admission_for_test())
}
async fn admitted(
    boot: &Boot,
    track: &str,
    idem: &str,
    probe: Option<calm_server::operation::forge_action_adapter::ProbeSpec>,
) -> (String, ParkedObserver) {
    let id = insert_forge_action(boot, track, idem, probe).await;
    let operations = SqlxOperationRepo::new(boot.repo.sqlite_db().unwrap().clone());
    let adapter = ForgeActionAdapter::new();
    let op = operations
        .claim_operation_for_recovery(&id)
        .await
        .unwrap()
        .unwrap();
    let (op, _) = operations
        .prepare_tx_and_advance(&op, &adapter, None)
        .await
        .unwrap()
        .committed()
        .unwrap();
    let op = operations
        .claim_operation_for_recovery(&op.id)
        .await
        .unwrap()
        .unwrap();
    operations
        .set_phase(&op, Phase::SpawnStarted, None)
        .await
        .unwrap()
        .applied()
        .unwrap();
    let op = operations
        .claim_operation_for_recovery(&id)
        .await
        .unwrap()
        .unwrap();
    let ctx = context(boot);
    std::fs::create_dir_all(op.payload["cwd_lease"].as_str().unwrap()).unwrap();
    let SpawnOutcome::Parked {
        deadline_ms,
        observer,
    } = adapter
        .spawn_side_effect(op.tx_output.as_ref().unwrap(), &op, &ctx)
        .await
        .unwrap()
    else {
        panic!("owned park")
    };
    operations
        .set_parked(&op, deadline_ms, Some("forge_execution"))
        .await
        .unwrap()
        .unwrap();
    (id, observer)
}
pub(super) async fn real_forge_action(boot: &Boot, track: &str, idem: &str) -> String {
    let (id, observer) = admitted(boot, track, idem, None).await;
    drop(observer); // Simulated observer loss: recovery must use the durable execution.
    id
}
async fn output(boot: &Boot, id: &str) -> Value {
    let operations = SqlxOperationRepo::new(boot.repo.sqlite_db().unwrap().clone());
    serde_json::to_value(
        operations
            .get_operation(id)
            .await
            .unwrap()
            .unwrap()
            .tx_output
            .unwrap(),
    )
    .unwrap()
}
pub(super) async fn settle_action(boot: &Boot, id: &str) {
    let mut output = output(boot, id).await;
    let execution: ExecutionId =
        serde_json::from_value(output["data"]["forge_execution"]["execution_id"].clone()).unwrap();
    let engine = boot.execution.lifecycle.engine().unwrap();
    let db = boot.repo.sqlite_db().unwrap();
    let mut tx = calm_server::db::sqlite::begin_immediate_tx(&db)
        .await
        .unwrap();
    let ticket = engine
        .request(&mut tx, &execution, Desired::Stop)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    engine.drive(ticket.committed());
    tokio::time::timeout(
        std::time::Duration::from_secs(10),
        engine.wait_settled(&execution),
    )
    .await
    .unwrap()
    .unwrap();
    // Crash checkpoint: execution settled, but op completion was not committed.
    output["data"]["forge_execution"]["release"] =
        json!({"Released":{"authorized_at_ms":now_ms()}});
    sqlx::query("UPDATE operations SET tx_output_json=?2 WHERE id=?1")
        .bind(id)
        .bind(output.to_string())
        .execute(db.writer_pool())
        .await
        .unwrap();
}

use calm_server::operation::forge_action_adapter::ProbeSpec;
use calm_server::test_seams::{PausePoint, install_pause_for_test};
use std::time::Duration;
fn pause(point: &str, key: &str) -> PausePoint {
    let p = PausePoint::default();
    install_pause_for_test(point, key, p.clone());
    p
}
async fn entered(p: &PausePoint) {
    tokio::time::timeout(Duration::from_secs(10), p.entered.notified())
        .await
        .unwrap();
}
enum ObserverStart {
    Fresh(ParkedObserver),
    Paused(PausePoint),
}
async fn probe_ready(boot: &Boot, track: &str, output_probe: bool) -> (String, ObserverStart) {
    boot.state
        .operation_runtime
        .execution()
        .bind(Arc::new(boot.execution.lifecycle.engine().unwrap().clone()));
    let (id, observer) = admitted(
        boot,
        track,
        "probe-fence",
        Some(ProbeSpec {
            probe_argv: vec!["/bin/true".into()],
            output_probe_argv: Some(vec![
                "/bin/sh".into(),
                "-c".into(),
                "printf '{\"headRefOid\":\"h\",\"oid\":\"m\"}'".into(),
            ]),
        }),
    )
    .await;
    settle_action(boot, &id).await;
    // Keep the caller's alias in FrozenForge; deletion leaves this link dangling.
    let mut frozen = output(boot, &id).await;
    let alias = boot.tmp.path().join("probe-cwd");
    std::os::unix::fs::symlink(frozen["data"]["cwd_lease"].as_str().unwrap(), &alias).unwrap();
    frozen["data"]["cwd_lease"] = json!(alias);
    sqlx::query("UPDATE operations SET tx_output_json=?2 WHERE id=?1")
        .bind(&id)
        .bind(frozen.to_string())
        .execute(boot.repo.sqlite_db().unwrap().writer_pool())
        .await
        .unwrap();
    if !output_probe {
        return (id, ObserverStart::Fresh(observer));
    }
    drop(observer);
    // Admit a real verdict through the recovery entry, holding its accelerator
    // before its first read while staging the post-settlement crash checkpoint.
    let operations = SqlxOperationRepo::new(boot.repo.sqlite_db().unwrap().clone());
    let claimed = operations.claim_parked(&id).await.unwrap().unwrap();
    let paused = pause("forge-observer-before-read", &id);
    ForgeActionAdapter::new()
        .recover_owned_parked(
            &claimed,
            calm_server::operation::RecoveryMode::PreDeadlineProbe,
            &context(boot),
        )
        .await
        .unwrap();
    entered(&paused).await;
    let mut data = output(boot, &id).await;
    let execution: ExecutionId = serde_json::from_value(
        data["data"]["forge_execution"]["probes"]["verdict"]["execution_id"].clone(),
    )
    .unwrap();
    let engine = boot.execution.lifecycle.engine().unwrap();
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if matches!(
                engine.output(&execution, None).await,
                Ok(workspace_runtime::execution::ExecutionReply::Observation {
                    state: workspace_runtime::execution::ExecutionState::Spawned,
                    ..
                })
            ) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    let db = boot.repo.sqlite_db().unwrap();
    let mut tx = calm_server::db::sqlite::begin_immediate_tx(&db)
        .await
        .unwrap();
    let ticket = engine
        .request(&mut tx, &execution, Desired::Stop)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    engine.drive(ticket.committed());
    tokio::time::timeout(Duration::from_secs(10), engine.wait_settled(&execution))
        .await
        .unwrap()
        .unwrap();
    data["data"]["forge_execution"]["probes"]["verdict"]["release"] =
        json!({"Released":{"authorized_at_ms":now_ms()}});
    let result = format!(
        "{}.probe.{}",
        data["data"]["result_path"].as_str().unwrap(),
        execution.as_str()
    );
    std::fs::write(format!("{result}.code"), "0").unwrap();
    std::fs::write(format!("{result}.stdout"), "").unwrap();
    sqlx::query(
        "UPDATE operations SET tx_output_json=?2,lease_owner=NULL,lease_until_ms=NULL WHERE id=?1",
    )
    .bind(&id)
    .bind(data.to_string())
    .execute(db.writer_pool())
    .await
    .unwrap();
    (id, ObserverStart::Paused(paused))
}

async fn count(boot: &Boot) -> i64 {
    sqlx::query_scalar("SELECT COUNT(*) FROM workspace_executions")
        .fetch_one(boot.repo.sqlite_db().unwrap().read_pool())
        .await
        .unwrap()
}
async fn op(boot: &Boot, id: &str) -> calm_server::operation::Operation {
    SqlxOperationRepo::new(boot.repo.sqlite_db().unwrap().clone())
        .get_operation(id)
        .await
        .unwrap()
        .unwrap()
}
async fn blocked(boot: &Boot, id: &str, observer: ObserverStart) -> PausePoint {
    let before = op(boot, id).await;
    let executions = count(boot).await;
    let step = pause("forge-observer-after-step", id);
    match observer {
        ObserverStart::Fresh(observer) => {
            tokio::spawn(observer);
        }
        ObserverStart::Paused(paused) => paused.release.notify_one(),
    }
    entered(&step).await;
    assert_eq!(
        count(boot).await,
        executions,
        "observer must not admit behind the effect fence"
    );
    let current = op(boot, id).await;
    assert_eq!(current.phase, Phase::Parked);
    assert_eq!(
        current.tx_output.as_ref().map(|o| &o.data),
        before.tx_output.as_ref().map(|o| &o.data),
        "Busy must not CAS the probe record"
    );
    assert_eq!(current.parked_deadline_ms, before.parked_deadline_ms);
    // Boot's recovery plan and final sweep share the production recovery entry.
    let operations = SqlxOperationRepo::new(boot.repo.sqlite_db().unwrap().clone());
    let claimed = operations.claim_parked_for_boot(id).await.unwrap().unwrap();
    assert!(matches!(
        ForgeActionAdapter::new()
            .recover_owned_parked(
                &claimed,
                calm_server::operation::RecoveryMode::Boot,
                &context(boot)
            )
            .await
            .unwrap(),
        calm_server::operation::ParkedRecovery::LeaveParked
    ));
    sqlx::query("UPDATE operations SET lease_owner=NULL,lease_until_ms=NULL WHERE id=?")
        .bind(id)
        .execute(boot.repo.sqlite_db().unwrap().writer_pool())
        .await
        .unwrap();
    boot.state.operation_runtime.sweep_parked().await.unwrap();
    assert_eq!(
        count(boot).await,
        executions,
        "boot/sweep must not admit behind the effect fence"
    );
    let current = op(boot, id).await;
    assert_eq!(current.phase, Phase::Parked);
    assert_eq!(
        current.tx_output.as_ref().map(|o| &o.data),
        before.tx_output.as_ref().map(|o| &o.data)
    );
    assert_eq!(current.parked_deadline_ms, before.parked_deadline_ms);
    let next = pause("forge-observer-before-read", id);
    step.release.notify_one();
    entered(&next).await;
    next
}
async fn retry(boot: &Boot, id: &str, survives: bool, paused: PausePoint) {
    let before = count(boot).await;
    boot.state.operation_runtime.sweep_parked().await.unwrap();
    let current = op(boot, id).await;
    if survives {
        assert_eq!(current.phase, Phase::Parked);
        assert_eq!(count(boot).await, before + 1);
    } else {
        assert_eq!(current.phase, Phase::Failed);
        assert_eq!(
            current.phase_detail.as_ref().unwrap()["last_error_class"],
            "gate-infra"
        );
        assert_eq!(count(boot).await, before);
    }
    paused.release.notify_one();
}

#[tokio::test]
async fn c5_probe_delete_proof_to_recycle_fences_observer_and_boot() {
    for area_delete in [false, true] {
        for output_probe in [false, true] {
            let boot = boot().await;
            let track = create_track(&boot, "probe-delete").await;
            let (id, observer) = probe_ready(&boot, &track, output_probe).await;
            let proof = if area_delete {
                pause("c5-area-after-proof", &boot.area_id)
            } else {
                pause("c5-track-after-proof", &track)
            };
            let race = async {
                entered(&proof).await;
                let observer = blocked(&boot, &id, observer).await;
                proof.release.notify_one();
                observer
            };
            let url = if area_delete {
                format!("/api/areas/{}", boot.area_id)
            } else {
                format!("/api/tracks/{track}")
            };
            let (status, observer) = tokio::join!(delete(boot.app.clone(), &url), race);
            assert_eq!(status, StatusCode::NO_CONTENT);
            retry(&boot, &id, false, observer).await;
        }
    }
}

#[tokio::test]
async fn c5_probe_clean_track_fence_observer_and_boot_then_retry() {
    use calm_server::test_seams::clean_closed_worktrees_with_history_for_test as clean;
    for output_probe in [false, true] {
        let boot = boot().await;
        let track = create_track(&boot, "probe-clean").await;
        // Remove the unrelated Legacy planner session guard from this clean fixture.
        let db = boot.repo.sqlite_db().unwrap();
        let sessions: Vec<String> =
            sqlx::query_scalar("SELECT id FROM worker_sessions WHERE track_id=?")
                .bind(&track)
                .fetch_all(db.read_pool())
                .await
                .unwrap();
        for session in sessions {
            if let Some(handle) = boot.state.harness.remove(&session) {
                let _ = handle.shutdown().await;
            }
        }
        sqlx::query("UPDATE tracks SET closed_at=123 WHERE id=?")
            .bind(&track)
            .execute(db.writer_pool())
            .await
            .unwrap();
        let (id, observer) = probe_ready(&boot, &track, output_probe).await;
        let fenced = pause("c1-clean-fenced", &track);
        let race = async {
            entered(&fenced).await;
            let observer = blocked(&boot, &id, observer).await;
            fenced.release.notify_one();
            observer
        };
        let mut history = std::collections::HashSet::new();
        let (result, observer) = tokio::join!(
            clean(&db, &boot.state.operation_runtime, &mut history),
            race
        );
        assert_eq!(result.unwrap(), 1);
        retry(&boot, &id, true, observer).await;
    }
}

#[tokio::test]
async fn c5_probe_own_track_and_canonical_location_owners_busy_leave_parked() {
    use calm_server::operation::{ParkedRecovery, RecoveryMode};
    use calm_server::per_card_lock::{lock_key, try_lock_key};
    for output_probe in [false, true] {
        for owner in ["own", "track", "area", "same-track"] {
            let boot = boot().await;
            let track = create_track(&boot, "probe-owners").await;
            let (id, observer) = probe_ready(&boot, &track, output_probe).await;
            let config = boot.state.location_admission_for_test();
            let root = config
                .managed_root
                .join(&boot.area_id)
                .join(if owner == "same-track" {
                    &track
                } else {
                    "foreign-track"
                });
            std::fs::create_dir_all(&root).unwrap();
            let alias = boot.tmp.path().join("cwd-alias");
            std::os::unix::fs::symlink(&root, &alias).unwrap();
            let mut data = output(&boot, &id).await;
            data["data"]["cwd_lease"] = json!(alias);
            sqlx::query("UPDATE operations SET tx_output_json=?2,parked_deadline_ms=1 WHERE id=?1")
                .bind(&id)
                .bind(data.to_string())
                .execute(boot.repo.sqlite_db().unwrap().writer_pool())
                .await
                .unwrap();
            let (locks, key) = match owner {
                "own" | "same-track" => (&config.track_locks, track.as_str()),
                "track" => (&config.track_locks, "foreign-track"),
                _ => (&config.area_locks, boot.area_id.as_str()),
            };
            let guard = lock_key(locks, key).await;
            let (paused, wait_for_observer) = match observer {
                ObserverStart::Fresh(observer) => {
                    drop(observer);
                    (pause("forge-observer-before-read", &id), true)
                }
                ObserverStart::Paused(paused) => (paused, false),
            };
            let operations = SqlxOperationRepo::new(boot.repo.sqlite_db().unwrap().clone());
            let claimed = operations.claim_parked(&id).await.unwrap().unwrap();
            let before = count(&boot).await;
            for mode in [
                RecoveryMode::Boot,
                RecoveryMode::PreDeadlineProbe,
                RecoveryMode::PastDeadline,
            ] {
                let recovery = ForgeActionAdapter::new()
                    .recover_owned_parked(&claimed, mode, &context(&boot))
                    .await
                    .unwrap();
                assert!(matches!(recovery, ParkedRecovery::LeaveParked));
                assert_eq!(count(&boot).await, before);
                let current = op(&boot, &id).await;
                assert_eq!(
                    current.tx_output.as_ref().map(|o| &o.data),
                    claimed.tx_output.as_ref().map(|o| &o.data)
                );
                assert_eq!(current.parked_deadline_ms, Some(1));
            }
            if wait_for_observer {
                entered(&paused).await;
            }
            if !matches!(owner, "own" | "same-track") {
                assert!(
                    try_lock_key(&config.track_locks, &track).is_some(),
                    "failed owner fence must release the owning track guard"
                );
            }
            drop(guard);
            let recovery = ForgeActionAdapter::new()
                .recover_owned_parked(&claimed, RecoveryMode::PastDeadline, &context(&boot))
                .await
                .unwrap();
            assert!(matches!(recovery, ParkedRecovery::LeaveParked));
            assert_eq!(count(&boot).await, before + 1);
            paused.release.notify_one();
        }
    }
}
