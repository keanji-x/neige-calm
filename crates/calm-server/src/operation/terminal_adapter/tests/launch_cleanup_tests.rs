use super::*;
use crate::operation::launch_cleanup_test_support::{
    AckProxy, install_commit_fault, probe_running, spawn_sibling,
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
    Commit,
    Deadline,
    AbortAndOpen,
    HealthyInitial,
    LegacyPrestart,
}

#[tokio::test]
async fn recovery_launch_commit_failure_retains_supervisor_ownership_before_compensation() {
    exercise(Fault::Commit).await;
}
#[tokio::test]
async fn recovery_launch_deadline_retains_supervisor_ownership_before_compensation() {
    exercise(Fault::Deadline).await;
}
#[tokio::test]
async fn recovery_launch_abort_ui_and_boot_never_resend_ensure() {
    exercise(Fault::AbortAndOpen).await;
}

#[tokio::test]
async fn recovery_launch_healthy_initial_and_legacy_attach_do_not_resend() {
    exercise(Fault::HealthyInitial).await;
}

#[tokio::test]
async fn recovery_launch_legacy_prestart_remains_runnable() {
    exercise(Fault::LegacyPrestart).await;
}

async fn exercise(fault: Fault) {
    let workspace = tempfile::tempdir().unwrap();
    let harness = terminal_worker_harness_with_workspace(workspace.path().to_str().unwrap()).await;
    let events = crate::event::EventBus::new();
    let write = WriteContext::new(
        harness.adapter.card_role_cache.clone(),
        harness.adapter.track_area_cache.clone(),
    );
    let declaration = json!({"key":"launch", "kind":"terminal", "command":"printf running > launched; sleep 30", "ready":true, "declared_by":"user"});
    let fixture = if matches!(fault, Fault::HealthyInitial | Fault::LegacyPrestart) {
        crate::task_recovery::launch_test_support::initial_claimed_task(
            harness.repo.clone(),
            events.clone(),
            write,
            &harness.track_id,
            declaration,
        )
        .await
    } else {
        crate::task_recovery::launch_test_support::recovered_claimed_task(
            harness.repo.clone(),
            events.clone(),
            write,
            &harness.track_id,
            declaration,
        )
        .await
    };
    let supervisor = calm_proc_supervisor::test_support::InProcessProcSupervisor::start()
        .await
        .unwrap();
    let sibling = spawn_sibling(supervisor.sock(), workspace.path()).await;
    if fault == Fault::Commit {
        install_commit_fault(harness.repo.pool()).await;
    }
    let proxy = Some(
        AckProxy::start(
            supervisor.sock(),
            workspace.path().join("launched"),
            matches!(fault, Fault::Deadline | Fault::AbortAndOpen),
        )
        .await,
    );
    let _short_deadline = if fault == Fault::Deadline {
        Some(crate::operation::task_launch::test_timeout::install(
            &fixture.task.id,
            Duration::from_millis(250),
        ))
    } else {
        None
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
    let (kind, payload) = crate::scheduler::build_worker_payload(&fixture.task).unwrap();
    let key = OperationKey {
        operation_key: new_id(),
        idempotency_key: Some(fixture.task.id.clone()),
        payload_hash: crate::routes::terminal_cards::stable_payload_hash(&payload).unwrap(),
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
        tokio::time::timeout(Duration::from_secs(10), run)
            .await
            .unwrap()
            .unwrap()
            .unwrap()
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
    if matches!(fault, Fault::Commit | Fault::Deadline | Fault::AbortAndOpen) {
        assert_eq!(
            output.data["terminal_launch"]["state"], "requested",
            "failed commit, timeout and read-only attachment never publish handoff"
        );
    }

    let card_id = output.output_string("card_id", "test").unwrap();
    if matches!(fault, Fault::Commit | Fault::Deadline) {
        let original = op.compensation_state.as_ref().unwrap()["reason"]
            .as_str()
            .unwrap();
        let expected = if fault == Fault::Commit {
            "FOREIGN KEY"
        } else {
            "control exchange timed out"
        };
        assert!(
            original.contains(expected),
            "fault must reach its intended boundary: {original}"
        );
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
    if fault == Fault::Commit {
        assert!(
            harness
                .repo
                .terminal_get(&terminal_id)
                .await
                .unwrap()
                .unwrap()
                .pid
                .is_some(),
            "acknowledged PID survives an enclosing COMMIT failure"
        );
    }
    assert!(
        matches!(op.phase, Phase::Stuck { .. }),
        "unproven cleanup remains actionable, not disposable"
    );
}
