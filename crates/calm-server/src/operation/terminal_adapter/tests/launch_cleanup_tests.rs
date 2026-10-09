use super::*;
use crate::operation::launch_cleanup_test_support::{
    AckProxy, NegativeFault, claimed_task, negative_ack_proxy, probe_running, spawn_sibling,
};
use crate::operation::{OperationCompletionBus, OperationRuntime, Phase};
use crate::state::{DaemonClient, WriteContext};
use crate::terminal_renderer::TerminalRendererRegistry;
use calm_session::control::{ControlMsg, ControlReply, ProbeRequest};
use calm_session::{read_frame, write_frame};
use calm_truth::db::RepoRead;
use std::sync::atomic::Ordering;
use std::time::Duration;

#[derive(Clone, Copy, PartialEq)]
enum Fault {
    AbortAndOpen,
    HealthyInitial,
    FastExit,
    LegacyPrestart,
    InvalidCwd,
    AckRestart,
    CompensationRestart,
    UnknownAck,
    Disconnect,
    LeaseLost,
    WrongSocket,
    PidConflict,
    WrongIdentity,
    ExpiredLease,
    NegativeWriteFailure,
    EvidenceReadFailure,
}

#[tokio::test]
async fn launch_abort_ui_and_boot_never_resend_ensure() {
    exercise(Fault::AbortAndOpen).await;
}

#[tokio::test]
async fn launch_healthy_initial_and_legacy_attach_do_not_resend() {
    exercise(Fault::HealthyInitial).await;
}

#[tokio::test]
async fn launch_legacy_prestart_remains_runnable() {
    exercise(Fault::LegacyPrestart).await;
}

#[tokio::test]
async fn spawn_failed_definite_ack_compensates_worker_rows() {
    exercise(Fault::InvalidCwd).await;
}

#[tokio::test]
async fn spawn_failed_ack_restart_consumes_rejection_before_boot_exit() {
    exercise(Fault::AckRestart).await;
}
#[tokio::test]
async fn spawn_failed_compensation_restart_is_idempotent() {
    exercise(Fault::CompensationRestart).await;
}
#[tokio::test]
async fn spawn_failed_uncertain_receipts_retain_ownership() {
    exercise(Fault::UnknownAck).await;
    exercise(Fault::Disconnect).await;
}
#[tokio::test]
async fn spawn_failed_negative_cas_fences_retain_ownership() {
    for fault in [
        Fault::LeaseLost,
        Fault::WrongSocket,
        Fault::PidConflict,
        Fault::NegativeWriteFailure,
        Fault::WrongIdentity,
        Fault::ExpiredLease,
    ] {
        exercise(fault).await;
    }
}

#[tokio::test]
async fn spawn_failed_evidence_read_failure_retains_compensation_for_retry() {
    exercise(Fault::EvidenceReadFailure).await;
}

#[tokio::test]
async fn launch_successful_fast_exit_preserves_worker_rows() {
    exercise(Fault::FastExit).await;
}

async fn exercise(fault: Fault) {
    let workspace = tempfile::tempdir().unwrap();
    let invalid = !matches!(
        fault,
        Fault::AbortAndOpen | Fault::HealthyInitial | Fault::LegacyPrestart | Fault::FastExit
    );
    let cwd = if invalid {
        workspace.path().join("absent")
    } else {
        workspace.path().to_path_buf()
    };
    let db_url = format!(
        "sqlite:{}",
        workspace.path().join("launch.sqlite").display()
    );
    let harness = terminal_worker_harness_with_repo(
        Arc::new(crate::db::sqlite::SqlxRepo::open(&db_url).await.unwrap()),
        cwd.to_str().unwrap(),
    )
    .await;
    let events = crate::event::EventBus::new();
    let write = WriteContext::new(
        harness.adapter.card_role_cache.clone(),
        harness.adapter.track_area_cache.clone(),
    );
    let mut declaration = json!({"key":"launch", "kind":"terminal", "command":"printf running > launched; sleep 30", "ready":true, "declared_by":"user"});
    if fault == Fault::FastExit {
        declaration["command"] = json!("printf running > launched; exit 0");
    }
    let task = claimed_task(
        harness.repo.clone(),
        events.clone(),
        write,
        &harness.track_id,
        declaration,
    )
    .await;
    let supervisor = calm_proc_supervisor::test_support::InProcessProcSupervisor::start()
        .await
        .unwrap();
    let sibling = spawn_sibling(supervisor.sock(), workspace.path()).await;
    let proxy = if invalid {
        Some(
            negative_ack_proxy(
                supervisor.sock(),
                harness.repo.pool().clone(),
                match fault {
                    Fault::UnknownAck => NegativeFault::Unknown,
                    Fault::Disconnect => NegativeFault::Disconnect,
                    Fault::LeaseLost => NegativeFault::LeaseLost,
                    Fault::WrongSocket => NegativeFault::WrongSocket,
                    Fault::PidConflict => NegativeFault::PidConflict,
                    Fault::WrongIdentity => NegativeFault::WrongIdentity,
                    Fault::ExpiredLease => NegativeFault::ExpiredLease,
                    _ => NegativeFault::Forward,
                },
            )
            .await,
        )
    } else {
        Some(
            AckProxy::start(
                supervisor.sock(),
                workspace.path().join("launched"),
                fault == Fault::AbortAndOpen,
            )
            .await,
        )
    };
    let op_repo = Arc::new(SqlxOperationRepo::new(harness.repo.pool().clone()));
    let renderer = TerminalRendererRegistry::new_with_repo(harness.repo.clone());
    let mut daemon = DaemonClient::new_stub();
    daemon.proc_supervisor_sock = Some(
        proxy
            .as_ref()
            .map_or_else(|| supervisor.sock().into(), |p| p.sock.clone()),
    );
    let daemon = Arc::new(daemon);
    let completion = OperationCompletionBus::new();
    let adapter = Arc::new(harness.adapter);
    let runtime = Arc::new(
        OperationRuntime::new(
            op_repo.clone(),
            vec![adapter.clone()],
            events.clone(),
            completion.clone(),
            SpawnCtx::new(
                harness.repo.clone(),
                op_repo.clone(),
                daemon.clone(),
                renderer.clone(),
                events,
                completion,
            ),
        )
        .await
        .unwrap(),
    );
    let (kind, payload) = crate::scheduler::build_worker_payload(&task).unwrap();
    let key = OperationKey {
        operation_key: new_id(),
        idempotency_key: Some(task.id.clone()),
        payload_hash: crate::routes::idempotency_key::stable_payload_hash(&payload).unwrap(),
    };
    let legacy_id = if fault == Fault::LegacyPrestart {
        let id = op_repo
            .insert_operation(kind, key.clone(), payload.clone())
            .await
            .unwrap();
        let claimed = op_repo
            .claim_drive_batch(1)
            .await
            .unwrap()
            .into_iter()
            .find(|op| op.id == id)
            .unwrap();
        op_repo
            .prepare_tx_and_advance(&claimed, adapter.as_ref())
            .await
            .unwrap()
            .unwrap();
        let prepared = op_repo.get_operation(&id).await.unwrap().unwrap();
        assert_eq!(prepared.phase, Phase::TxCommitted);
        sqlx::query("UPDATE operations SET tx_output_json=json_remove(tx_output_json,'$.data.terminal_launch') WHERE id=?1")
            .bind(&id).execute(harness.repo.pool()).await.unwrap();
        Some(id)
    } else {
        None
    };
    let interruption = match fault {
        Fault::AckRestart => Some(
            "CREATE TRIGGER interrupt_launch BEFORE UPDATE OF phase ON operations WHEN NEW.phase IN \
            ('compensating','stuck') BEGIN SELECT RAISE(ABORT,'ack interruption'); END",
        ),
        Fault::CompensationRestart => Some(
            "CREATE TRIGGER interrupt_launch BEFORE UPDATE OF phase,compensation_state ON operations WHEN \
            OLD.phase='compensating' AND (NEW.phase<>'compensating' OR \
            NEW.compensation_state<>OLD.compensation_state) BEGIN SELECT RAISE(ABORT,'compensation interruption'); \
            END",
        ),
        Fault::NegativeWriteFailure => Some(
            "CREATE TRIGGER interrupt_launch BEFORE UPDATE OF tx_output_json ON operations WHEN \
            json_extract(NEW.tx_output_json,'$.data.terminal_launch.state')='rejected' BEGIN SELECT \
            RAISE(ABORT,'negative persistence failure'); END",
        ),
        _ => None,
    };
    if let Some(sql) = interruption {
        sqlx::query(sql).execute(harness.repo.pool()).await.unwrap();
    }
    if fault == Fault::EvidenceReadFailure {
        sqlx::query("CREATE TRIGGER conflict_evidence AFTER UPDATE OF phase ON operations WHEN NEW.phase='compensating' \
            BEGIN INSERT INTO operations (id,operation_key,kind,payload_hash,target_type,target_id,target_json, \
            payload_json,tx_output_json,phase,created_at_ms,updated_at_ms) VALUES ('conflict','conflict',NEW.kind, \
            NEW.payload_hash,NEW.target_type,NEW.target_id,NEW.target_json,NEW.payload_json,NEW.tx_output_json, \
            'failed',1,1); END").execute(harness.repo.pool()).await.unwrap();
        sqlx::query("CREATE TRIGGER block_stuck BEFORE UPDATE OF phase ON operations WHEN NEW.phase='stuck' \
            BEGIN SELECT RAISE(ABORT,'hold failed cleanup for reboot'); END")
            .execute(harness.repo.pool()).await.unwrap();
    }
    let submitted_key = key.clone();
    let drive = runtime.clone();
    let run = tokio::spawn(async move {
        if let Some(id) = legacy_id {
            drive.drive().await?;
            Ok(id)
        } else {
            drive.submit(kind, submitted_key, payload).await
        }
    });
    struct Abort(tokio::task::AbortHandle);
    impl Drop for Abort {
        fn drop(&mut self) {
            self.0.abort();
        }
    }
    let _abort = Abort(run.abort_handle());
    let op_id = if fault == Fault::AbortAndOpen {
        let proxy = proxy.as_ref().unwrap();
        tokio::time::timeout(Duration::from_secs(3), proxy.spawned.notified())
            .await
            .unwrap();
        run.abort();
        assert!(run.await.unwrap_err().is_cancelled());
        let op = op_repo
            .find_by_idempotency_key(kind, &key)
            .await
            .unwrap()
            .unwrap();
        let output = op.tx_output.as_ref().unwrap();
        let term_id = output.output_string("terminal_id", "test").unwrap();
        let term = harness.repo.terminal_get(&term_id).await.unwrap().unwrap();
        assert!(
            term.pid.is_none(),
            "abort is before renderer PID persistence"
        );
        // Ordinary viewer/WS entry: no TaskLaunch argument is available here.
        let _view = tokio::time::timeout(
            Duration::from_secs(3),
            crate::routes::terminal::spawn_terminal_with_parts(
                daemon.as_ref(),
                renderer.as_ref(),
                harness.repo.as_ref(),
                &term,
                &output.output_string("cmd", "test").unwrap(),
                &output.output_string("cwd", "test").unwrap(),
                &output.data["env"],
            ),
        )
        .await
        .unwrap();
        let plan = runtime.recover_on_boot().await.unwrap();
        tokio::time::timeout(Duration::from_secs(5), runtime.apply_recovery(plan))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            proxy.ensures.load(Ordering::SeqCst),
            1,
            "UI and boot must attach/reconcile, never resend an uncertain EnsureProc"
        );
        op.id
    } else {
        let result = tokio::time::timeout(Duration::from_secs(10), run)
            .await
            .unwrap()
            .unwrap();
        if fault == Fault::EvidenceReadFailure {
            let op = op_repo
                .find_by_idempotency_key(kind, &key)
                .await
                .unwrap()
                .unwrap();
            assert_eq!(
                op.phase,
                Phase::Compensating,
                "unavailable evidence must not complete cleanup: {result:?}"
            );
            let state: crate::operation::CompensationStateVersioned =
                serde_json::from_value(op.compensation_state.clone().unwrap()).unwrap();
            assert!(!state.steps[0].completed);
            assert!(
                state.steps[0]
                    .last_error
                    .as_deref()
                    .unwrap()
                    .contains("conflicting launch ownership")
            );
            assert_eq!(state.steps[0].attempts, 1);
            assert!(result.is_err(), "blocked fallback must expose the failure");
            assert!(workspace.path().is_dir());
            let output = op.tx_output.as_ref().unwrap();
            assert!(
                harness
                    .repo
                    .card_get(&output.output_string("card_id", "test").unwrap())
                    .await
                    .unwrap()
                    .is_some()
            );
            assert!(
                harness
                    .repo
                    .terminal_get(&output.output_string("terminal_id", "test").unwrap())
                    .await
                    .unwrap()
                    .is_some()
            );
            sqlx::query("DROP TRIGGER conflict_evidence")
                .execute(harness.repo.pool())
                .await
                .unwrap();
            sqlx::query("DROP TRIGGER block_stuck")
                .execute(harness.repo.pool())
                .await
                .unwrap();
            sqlx::query("DELETE FROM operations WHERE id='conflict'")
                .execute(harness.repo.pool())
                .await
                .unwrap();
            runtime
                .apply_recovery(runtime.recover_on_boot().await.unwrap())
                .await
                .unwrap();
            op.id
        } else if matches!(fault, Fault::AckRestart | Fault::CompensationRestart) {
            // Block both the interrupted write and drive's fallback Stuck write:
            // only the durable phase/checkpoint define the crash boundary.
            let _ = result;
            let op = op_repo
                .find_by_idempotency_key(kind, &key)
                .await
                .unwrap()
                .unwrap();
            assert_eq!(
                op.tx_output.as_ref().unwrap().data["terminal_launch"]["state"],
                "rejected"
            );
            assert_eq!(
                op.phase,
                if fault == Fault::AckRestart {
                    Phase::SpawnStarted
                } else {
                    Phase::Compensating
                }
            );
            if fault == Fault::CompensationRestart {
                let state: crate::operation::CompensationStateVersioned =
                    serde_json::from_value(op.compensation_state.clone().unwrap()).unwrap();
                assert!(
                    !state.steps[0].completed,
                    "interruption must precede cleanup completion persistence"
                );
                let terminal_id = op
                    .tx_output
                    .as_ref()
                    .unwrap()
                    .output_string("terminal_id", "test")
                    .unwrap();
                assert!(
                    harness
                        .repo
                        .terminal_get(&terminal_id)
                        .await
                        .unwrap()
                        .is_none(),
                    "interruption follows real row cleanup"
                );
            }
            if fault == Fault::AckRestart {
                let output = op.tx_output.as_ref().unwrap();
                let term_id = output.output_string("terminal_id", "test").unwrap();
                let term = harness.repo.terminal_get(&term_id).await.unwrap().unwrap();
                let bus = OperationCompletionBus::new();
                let ctx = SpawnCtx::new(
                    harness.repo.clone(),
                    op_repo.clone(),
                    daemon.clone(),
                    renderer.clone(),
                    crate::event::EventBus::new(),
                    bus,
                );
                assert!(
                    crate::operation::worker_cleanup::require_cleanup_safe(&ctx, &op, output, true)
                        .await
                        .is_err(),
                    "negative viewer receipt must not bypass business-session veto"
                );
                let view = crate::routes::terminal::spawn_terminal_with_parts(
                    daemon.as_ref(),
                    renderer.as_ref(),
                    harness.repo.as_ref(),
                    &term,
                    &output.output_string("cmd", "test").unwrap(),
                    &output.output_string("cwd", "test").unwrap(),
                    &output.data["env"],
                )
                .await;
                assert!(view.is_err(), "UI cannot reopen a rejected one-use launch");
                assert_eq!(proxy.as_ref().unwrap().ensures.load(Ordering::SeqCst), 1);
            }
            sqlx::query("DROP TRIGGER interrupt_launch")
                .execute(harness.repo.pool())
                .await
                .unwrap();
            // Reopen the on-disk database and reconstruct runtime/state, using real boot ordering.
            let reboot_repo = Arc::new(crate::db::sqlite::SqlxRepo::open(&db_url).await.unwrap());
            let reboot_op_repo = Arc::new(SqlxOperationRepo::new(reboot_repo.pool().clone()));
            let reboot_adapter = Arc::new(TerminalWorkerAdapter::new(
                reboot_repo.clone(),
                adapter.card_role_cache.clone(),
                adapter.track_area_cache.clone(),
            ));
            let state = crate::state::AppState::from_parts(
                reboot_repo.clone(),
                crate::event::EventBus::new(),
                daemon.clone(),
                Arc::new(crate::plugin_host::PluginHost::new_full(
                    Arc::new(crate::plugin_host::PluginRegistry::empty()),
                    reboot_repo.clone(),
                    std::path::PathBuf::new(),
                    workspace.path().join("plugins"),
                    vec![],
                    crate::event::EventBus::new(),
                    WriteContext::new(
                        adapter.card_role_cache.clone(),
                        adapter.track_area_cache.clone(),
                    ),
                )),
                Arc::new(crate::state::CodexClient::new_stub()),
                None,
                None,
            );
            crate::reconcile_supervisor_on_boot(&state).await;
            if fault == Fault::AckRestart {
                let terminal_id = op
                    .tx_output
                    .as_ref()
                    .unwrap()
                    .output_string("terminal_id", "test")
                    .unwrap();
                assert_eq!(
                    harness
                        .repo
                        .terminal_get(&terminal_id)
                        .await
                        .unwrap()
                        .unwrap()
                        .exit_code,
                    Some(-1)
                );
            }
            let bus = OperationCompletionBus::new();
            let restarted = OperationRuntime::new(
                reboot_op_repo.clone(),
                vec![reboot_adapter],
                crate::event::EventBus::new(),
                bus.clone(),
                SpawnCtx::new(
                    reboot_repo.clone(),
                    reboot_op_repo.clone(),
                    daemon.clone(),
                    renderer.clone(),
                    crate::event::EventBus::new(),
                    bus,
                ),
            )
            .await
            .unwrap();
            restarted
                .apply_recovery(restarted.recover_on_boot().await.unwrap())
                .await
                .unwrap();
            restarted.drive().await.unwrap();
            restarted
                .apply_recovery(restarted.recover_on_boot().await.unwrap())
                .await
                .unwrap();
            op.id
        } else {
            result.unwrap()
        }
    };
    let op = op_repo.get_operation(&op_id).await.unwrap().unwrap();
    if matches!(fault, Fault::HealthyInitial | Fault::LegacyPrestart) {
        assert_eq!(
            op.phase,
            Phase::Succeeded,
            "a proven pre-spawn operation remains runnable: {:?}",
            op.last_error
        );
    }
    let output = op.tx_output.as_ref().unwrap();
    let terminal_id = output.output_string("terminal_id", "test").unwrap();
    if fault == Fault::AbortAndOpen {
        assert_eq!(
            output.data["terminal_launch"]["state"], "requested",
            "a read-only attachment never publishes handoff"
        );
    }

    let card_id = output.output_string("card_id", "test").unwrap();
    if fault == Fault::FastExit {
        assert_eq!(op.phase, Phase::Succeeded);
        assert!(harness.repo.card_get(&card_id).await.unwrap().is_some());
        tokio::time::timeout(Duration::from_secs(3), async {
            while harness
                .repo
                .terminal_get(&terminal_id)
                .await
                .unwrap()
                .unwrap()
                .exit_code
                .is_none()
            {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        assert!(probe_running(supervisor.sock(), &sibling).await);
        assert_eq!(proxy.as_ref().unwrap().ensures.load(Ordering::SeqCst), 1);
        return;
    }

    if invalid
        && !matches!(
            fault,
            Fault::InvalidCwd
                | Fault::AckRestart
                | Fault::CompensationRestart
                | Fault::EvidenceReadFailure
        )
    {
        assert_eq!(output.data["terminal_launch"]["state"], "requested");
        assert!(harness.repo.card_get(&card_id).await.unwrap().is_some());
        assert!(
            harness
                .repo
                .terminal_get(&terminal_id)
                .await
                .unwrap()
                .is_some()
        );
        assert_ne!(op.phase, Phase::Failed);
        let term = harness
            .repo
            .terminal_get(&terminal_id)
            .await
            .unwrap()
            .unwrap();
        let _view = crate::routes::terminal::spawn_terminal_with_parts(
            daemon.as_ref(),
            renderer.as_ref(),
            harness.repo.as_ref(),
            &term,
            &output.output_string("cmd", "test").unwrap(),
            &output.output_string("cwd", "test").unwrap(),
            &output.data["env"],
        )
        .await;
        runtime
            .apply_recovery(runtime.recover_on_boot().await.unwrap())
            .await
            .unwrap();
        assert!(harness.repo.card_get(&card_id).await.unwrap().is_some());
        assert!(
            harness
                .repo
                .terminal_get(&terminal_id)
                .await
                .unwrap()
                .is_some()
        );
        assert!(probe_running(supervisor.sock(), &sibling).await);
        assert_eq!(proxy.as_ref().unwrap().ensures.load(Ordering::SeqCst), 1);
        return;
    }
    if invalid {
        assert_eq!(output.data["terminal_launch"]["state"], "rejected");
        assert_eq!(
            runtime
                .submit(kind, key.clone(), op.payload.clone())
                .await
                .unwrap(),
            op_id
        );
        assert_eq!(
            op.phase,
            Phase::Failed,
            "definite spawn rejection must compensate: {:?}",
            op.last_error
        );
        assert!(harness.repo.card_get(&card_id).await.unwrap().is_none());
        assert!(
            harness
                .repo
                .terminal_get(&terminal_id)
                .await
                .unwrap()
                .is_none()
        );
        let remaining_sessions: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM worker_sessions WHERE card_id=?1")
                .bind(&card_id)
                .fetch_one(harness.repo.pool())
                .await
                .unwrap();
        assert_eq!(remaining_sessions, 0);
        assert!(renderer.is_empty());
        assert_eq!(proxy.as_ref().unwrap().ensures.load(Ordering::SeqCst), 1);
        assert!(!workspace.path().join("launched").exists());
        assert!(workspace.path().is_dir());
        assert!(!probe_running(supervisor.sock(), &format!("term:{terminal_id}")).await);
        assert!(probe_running(supervisor.sock(), &sibling).await);
        return;
    }

    tokio::time::timeout(Duration::from_secs(3), async {
        while !workspace.path().join("launched").exists() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    let mut control = tokio::net::UnixStream::connect(supervisor.sock())
        .await
        .unwrap();
    write_frame(
        &mut control,
        &ControlMsg::Probe(ProbeRequest {
            proc_id: format!("term:{terminal_id}"),
        }),
    )
    .await
    .unwrap();
    let reply: ControlReply = read_frame(&mut control).await.unwrap();
    let running = matches!(
        reply,
        ControlReply::ProbeOk {
            proc_running: true,
            ..
        }
    );
    if matches!(fault, Fault::HealthyInitial | Fault::LegacyPrestart) {
        assert_eq!(op.phase, Phase::Succeeded);
        let term = harness
            .repo
            .terminal_get(&terminal_id)
            .await
            .unwrap()
            .unwrap();
        assert!(term.pid.is_some());
        for legacy in [false, true] {
            if legacy {
                // Released/pre-upgrade output had no launch checkpoint. Its
                // absence is unknown, never permission to run the command again.
                sqlx::query("UPDATE operations SET tx_output_json=json_remove(tx_output_json,'$.data.terminal_launch') WHERE id=?1")
                    .bind(&op_id).execute(harness.repo.pool()).await.unwrap();
            }
            let attached_registry = TerminalRendererRegistry::new_with_repo(harness.repo.clone());
            let attached = tokio::time::timeout(
                Duration::from_secs(3),
                crate::routes::terminal::spawn_terminal_with_parts(
                    daemon.as_ref(),
                    attached_registry.as_ref(),
                    harness.repo.as_ref(),
                    &term,
                    &output.output_string("cmd", "test").unwrap(),
                    &output.output_string("cwd", "test").unwrap(),
                    &output.data["env"],
                ),
            )
            .await
            .unwrap()
            .unwrap();
            assert_eq!(attached.terminal_id, terminal_id);
        }
        assert_eq!(proxy.as_ref().unwrap().ensures.load(Ordering::SeqCst), 1);
        assert!(probe_running(supervisor.sock(), &sibling).await);
        return;
    }
    assert!(
        probe_running(supervisor.sock(), &sibling).await,
        "cleanup must preserve an unrelated supervisor process"
    );
    let retained = harness.repo.card_get(&card_id).await.unwrap().is_some()
        && harness
            .repo
            .terminal_get(&terminal_id)
            .await
            .unwrap()
            .is_some();
    assert!(
        retained,
        "uncertain launch discarded worker ownership; supervisor_running={running}, renderer_present={}, phase={:?}",
        renderer.get(&terminal_id).is_some(),
        op.phase
    );
    assert!(
        matches!(op.phase, Phase::Stuck { .. }),
        "unproven cleanup remains actionable, not disposable"
    );
}
