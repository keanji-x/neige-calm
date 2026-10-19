//! Real backend notification -> loop owner -> SQL -> durable system ACK.
use super::completed_commit_tests::Fixture;
use super::handoff_pause_tests::{request, running, system};
use super::*;
use crate::codex_appserver::Notification;
use crate::db::prelude::*;
use crate::harness::handoff_pause::{BackendFault, PauseReceipt, PauseState, ReleaseOutcome};
use crate::session_projection_repo::WorkerSessionState;

async fn notify(f: &Fixture, notification: Notification) {
    let hook = crate::test_seams::PausePoint::default();
    crate::test_seams::install_pause_for_test(
        "harness-notification-completed",
        f.harness.inner.card_id.as_str(),
        hook.clone(),
    );
    f.daemon.emit_notification_for_test(notification);
    tokio::time::timeout(Duration::from_secs(10), hook.entered.notified())
        .await
        .unwrap();
    // This hook is after the complete notification handler, including its durable write.
    hook.release.notify_one();
}

pub(super) async fn error(f: &Fixture) {
    notify(
        f,
        Notification::ThreadStatusChanged {
            thread_id: "thread-commit-wake".into(),
            status: serde_json::json!({"type":"systemError"}),
        },
    )
    .await;
}

fn faulted(receipt: &PauseReceipt) -> PauseState {
    PauseState::FaultedPaused {
        receipt: receipt.clone(),
        fault: BackendFault::ThreadSystemError,
    }
}

async fn status(f: &Fixture) -> WorkerSessionState {
    f.repo
        .session_projection_by_id(&f.harness.inner.worker_session_id)
        .await
        .unwrap()
        .unwrap()
        .status
}

pub(super) async fn restart(f: &mut Fixture) {
    // The real boot/lazy/replacement owner reloads SQL and CASes its complete source.
    f.harness.shutdown().await.unwrap();
    let runtime = f
        .repo
        .session_projection_by_id(&f.harness.inner.worker_session_id)
        .await
        .unwrap()
        .unwrap();
    let registry = crate::harness::HarnessRegistry::new();
    f.harness = crate::harness::spawn_recovered_harness(
        f.repo.clone(),
        f.harness.inner.events.clone(),
        f.harness.inner.card_role_cache.clone(),
        f.harness.inner.track_area_cache.clone(),
        f.daemon.clone(),
        f.daemon.thread_seals().clone(),
        &crate::harness::wiring::PlannerWiring::unconfigured_for_test(f.repo.clone()),
        &registry,
        &crate::per_card_lock::new_keyed_locks(),
        runtime,
        crate::harness::ClaimMode::Replace,
    )
    .await
    .unwrap()
    .installed()
    .expect("faulted pause remains recoverable");
}

pub(super) async fn refuse_snapshot(f: &Fixture) {
    sqlx::query(concat!(
        "CREATE TRIGGER refuse_fault BEFORE UPDATE OF handle_state_json ON worker_sessions BEGIN ",
        "SELECT RAISE(ABORT, 'fault db failure'); END",
    ))
    .execute(f.repo.db().writer_pool())
    .await
    .unwrap();
}

pub(super) async fn allow_snapshot(f: &Fixture) {
    sqlx::query("DROP TRIGGER refuse_fault")
        .execute(f.repo.db().writer_pool())
        .await
        .unwrap();
}

#[tokio::test]
async fn handoff_pause_system_error_keeps_durable_ack_and_original_compensation() {
    let mut f = running().await;
    let receipt = f.harness.pause_for_handoff(request(&f)).await.unwrap();
    error(&f).await;
    let memory = f.harness.snapshot().await;
    let ack = f
        .harness
        .observe_envelope(
            Observation::TrackGoal {
                text: "after fault".into(),
            },
            51,
        )
        .await;
    assert!(
        ack.is_ok(),
        "ACK={ack:?}; memory={:?}; SQL={:?}",
        memory.phase,
        f.stored().await.phase
    );
    assert_eq!(memory.phase, HarnessPhaseTag::Idle);
    assert_eq!(memory.handoff_pause, faulted(&receipt));
    assert_eq!(f.stored().await.push_watermark, 51);
    assert_eq!(f.harness.snapshot().await, f.stored().await);
    assert_eq!(status(&f).await, WorkerSessionState::Idle);
    assert_eq!(
        f.harness
            .query_handoff_pause(receipt.request().clone())
            .await
            .unwrap(),
        faulted(&receipt)
    );
    assert_eq!(
        f.harness
            .pause_for_handoff(receipt.request().clone())
            .await
            .unwrap(),
        receipt
    );
    let before = f.stored().await;
    error(&f).await;
    notify(
        &f,
        Notification::ThreadStatusChanged {
            thread_id: "thread-commit-wake".into(),
            status: serde_json::json!({"type":"idle"}),
        },
    )
    .await;
    assert_eq!(
        f.stored().await,
        before,
        "duplicate error and idle cannot clear fault"
    );
    restart(&mut f).await;
    assert_eq!(f.harness.snapshot().await, before);
    super::handoff_pause_interrupt_tests::tick_completed(&f).await;
    assert_eq!(f.daemon.turn_start_count_for_test(), 0);
    assert_eq!(
        f.harness
            .query_handoff_pause(receipt.request().clone())
            .await
            .unwrap(),
        faulted(&receipt)
    );
    system(&f, 52).await;
    let pending = f.stored().await.pending_entries();
    assert_eq!(
        f.harness
            .resume_from_handoff(receipt.clone())
            .await
            .unwrap(),
        ReleaseOutcome::ReleasedWithBackendFault {
            fault: BackendFault::ThreadSystemError
        }
    );
    let released = f.stored().await;
    assert_eq!(
        released.handoff_pause,
        PauseState::Released {
            receipt: receipt.clone(),
            outcome: ReleaseOutcome::ReleasedWithBackendFault {
                fault: BackendFault::ThreadSystemError
            }
        }
    );
    assert_eq!(released.phase, HarnessPhaseTag::Wedged);
    assert_eq!(
        released.wedged_reason.as_deref(),
        Some(HARNESS_SYSTEM_ERROR_REASON)
    );
    assert_eq!(released.pending_entries(), pending);
    assert_eq!(released.push_watermark, 52);
    assert_eq!(status(&f).await, WorkerSessionState::Failed);
    assert_eq!(f.harness.snapshot().await, released);
    super::handoff_pause_interrupt_tests::tick_completed(&f).await;
    assert_eq!(f.daemon.turn_start_count_for_test(), 0);
    // Existing recovery quiesce owns this state after release; no new automatic recovery.
    f.harness.quiesce_system_error_for_recovery().await.unwrap();
}

#[tokio::test]
async fn handoff_pause_fault_release_failure_restores_full_checkpoint_and_retries() {
    let f = running().await;
    let receipt = f.harness.pause_for_handoff(request(&f)).await.unwrap();
    system(&f, 31).await;
    error(&f).await;
    let before = f.stored().await;
    assert_eq!(before.handoff_pause, faulted(&receipt));
    // Abort the later runtime-status write, after handle_state_json was already updated
    // inside the transaction: marker clearance and Failed must roll back together.
    sqlx::query(concat!(
        "CREATE TRIGGER refuse_fault BEFORE UPDATE OF state ON worker_sessions ",
        "WHEN NEW.state='failed' BEGIN SELECT RAISE(ABORT, 'release status failure'); END",
    ))
    .execute(f.repo.db().writer_pool())
    .await
    .unwrap();
    assert!(
        f.harness
            .resume_from_handoff(receipt.clone())
            .await
            .is_err()
    );
    assert_eq!(f.harness.snapshot().await, before);
    assert_eq!(f.stored().await, before);
    assert_eq!(status(&f).await, WorkerSessionState::Idle);
    assert!(!f.harness.inner.shutting_down.load(Ordering::SeqCst));
    allow_snapshot(&f).await;
    system(&f, 32).await;
    assert_eq!(
        f.harness
            .resume_from_handoff(receipt.clone())
            .await
            .unwrap(),
        ReleaseOutcome::ReleasedWithBackendFault {
            fault: BackendFault::ThreadSystemError
        }
    );
    assert_eq!(status(&f).await, WorkerSessionState::Failed);
    assert_eq!(f.stored().await.phase, HarnessPhaseTag::Wedged);
    assert_eq!(
        f.stored().await.handoff_pause,
        PauseState::Released {
            receipt: receipt.clone(),
            outcome: ReleaseOutcome::ReleasedWithBackendFault {
                fault: BackendFault::ThreadSystemError
            }
        }
    );
    assert_eq!(f.stored().await.push_watermark, 32);
    assert_eq!(f.stored().await.pending_entries().len(), 2);
    f.harness.shutdown().await.unwrap();
}

#[tokio::test]
async fn handoff_pause_fault_notification_storage_failure_keeps_sticky_fault_and_unchanged_sql() {
    let f = running().await;
    let receipt = f.harness.pause_for_handoff(request(&f)).await.unwrap();
    system(&f, 41).await;
    let before = f.stored().await;
    refuse_snapshot(&f).await;
    error(&f).await;
    assert!(f.harness.inner.shutting_down.load(Ordering::SeqCst));
    let mut sticky = before.clone();
    sticky.handoff_pause = faulted(&receipt);
    assert_eq!(f.harness.snapshot().await, sticky);
    assert_eq!(f.stored().await, before);
    assert_eq!(status(&f).await, WorkerSessionState::Idle);
    assert!(f.harness.resume_from_handoff(receipt).await.is_err());
    assert!(
        f.harness
            .observe_envelope(
                Observation::TrackGoal {
                    text: "must refuse".into()
                },
                42
            )
            .await
            .is_err()
    );
    assert_eq!(f.stored().await, before);
    allow_snapshot(&f).await;
}

#[tokio::test]
async fn handoff_pause_fault_release_requires_exact_claim_and_current_binding() {
    let f = running().await;
    let receipt = f.harness.pause_for_handoff(request(&f)).await.unwrap();
    error(&f).await;
    let before = f.stored().await;
    assert_eq!(before.handoff_pause, faulted(&receipt));
    assert!(
        f.harness
            .resume_from_handoff(PauseReceipt::from_request(request(&f)))
            .await
            .is_err()
    );
    assert_eq!(f.harness.snapshot().await, before);
    sqlx::query("UPDATE cards SET session_id=NULL WHERE id=?1")
        .bind(f.harness.inner.card_id.as_str())
        .execute(f.repo.db().writer_pool())
        .await
        .unwrap();
    assert!(f.harness.resume_from_handoff(receipt).await.is_err());
    let raw = f
        .repo
        .session_projection_handle_state_by_id(&f.harness.inner.worker_session_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(HarnessSnapshot::parse_known(raw).unwrap(), before);
    assert_eq!(f.harness.snapshot().await, before);
    assert!(f.harness.inner.shutting_down.load(Ordering::SeqCst));
}

#[tokio::test]
async fn handoff_pause_late_lifecycle_notifications_cannot_create_work_or_clear_fault() {
    let f = running().await;
    let receipt = f.harness.pause_for_handoff(request(&f)).await.unwrap();
    for fault in [false, true] {
        if fault {
            error(&f).await;
        }
        let before = f.stored().await;
        for notification in [
            Notification::TurnStarted {
                thread_id: "thread-commit-wake".into(),
                turn: serde_json::json!({"id":"late"}),
            },
            Notification::TurnCompleted {
                thread_id: "thread-commit-wake".into(),
                turn: serde_json::json!({"id":"late","status":"failed"}),
            },
            Notification::ThreadStarted {
                params: serde_json::json!({"thread":{"id":"thread-commit-wake"}}),
            },
            Notification::ThreadStatusChanged {
                thread_id: "thread-commit-wake".into(),
                status: serde_json::json!({"type":"idle"}),
            },
        ] {
            notify(&f, notification).await;
        }
        super::handoff_pause_interrupt_tests::tick_completed(&f).await;
        assert_eq!(f.harness.snapshot().await, before);
        assert_eq!(f.stored().await, before);
        assert_eq!(f.daemon.turn_start_count_for_test(), 0);
    }
    assert_eq!(f.stored().await.handoff_pause, faulted(&receipt));
    f.harness.shutdown().await.unwrap();
}

#[tokio::test]
async fn handoff_pause_unpaused_system_error_retains_original_failed_contract() {
    let f = running().await;
    error(&f).await;
    assert_eq!(f.harness.snapshot().await.phase, HarnessPhaseTag::Wedged);
    assert_eq!(f.stored().await.handoff_pause, PauseState::Unpaused);
    assert_eq!(
        f.stored().await.wedged_reason.as_deref(),
        Some(HARNESS_SYSTEM_ERROR_REASON)
    );
    assert_eq!(status(&f).await, WorkerSessionState::Failed);
    notify(
        &f,
        Notification::ThreadStatusChanged {
            thread_id: "thread-commit-wake".into(),
            status: serde_json::json!({"type":"idle"}),
        },
    )
    .await;
    assert_eq!(f.harness.snapshot().await.phase, HarnessPhaseTag::Wedged);
    f.harness.quiesce_system_error_for_recovery().await.unwrap();
}

#[tokio::test]
async fn handoff_pause_fault_decoder_remains_fail_closed() {
    let f = Fixture::new().await;
    let receipt = f.harness.pause_for_handoff(request(&f)).await.unwrap();
    let mut snapshot = f.stored().await;
    // The old healthy paused shape has an explicit decoder; no fault field is required there.
    assert!(HarnessSnapshot::parse_known(serde_json::to_value(&snapshot).unwrap()).is_some());
    snapshot.handoff_pause = faulted(&receipt);
    let good = serde_json::to_value(&snapshot).unwrap();
    assert_eq!(
        HarnessSnapshot::parse_known(good.clone()).unwrap(),
        snapshot
    );
    for bad in [
        serde_json::json!({"state":"faulted_paused","receipt":receipt}),
        serde_json::json!({"state":"faulted_paused","receipt":receipt,"fault":null}),
        serde_json::json!({"state":"faulted_paused","receipt":receipt,"fault":{"kind":"other"}}),
        serde_json::json!({"state":"faulted_paused","receipt":null,"fault":{"kind":"thread_system_error"}}),
        serde_json::json!({"state":"paused","receipt":receipt,"fault":{"kind":"thread_system_error"}}),
    ] {
        let mut raw = good.clone();
        raw["handoff_pause"] = bad;
        assert!(HarnessSnapshot::parse_known(raw).is_none());
    }
    for phase in [
        HarnessPhaseTag::Wedged,
        HarnessPhaseTag::Resumed,
        HarnessPhaseTag::TurnRunning,
        HarnessPhaseTag::Compacting,
    ] {
        snapshot.phase = phase;
        assert!(HarnessSnapshot::parse_known(serde_json::to_value(&snapshot).unwrap()).is_none());
    }
}

#[tokio::test]
async fn handoff_pause_fault_acp_reconciliation_preserves_checkpoint_or_refuses() {
    let f = Fixture::new().await;
    let receipt = f.harness.pause_for_handoff(request(&f)).await.unwrap();
    let mut snapshot = f.stored().await;
    snapshot.handoff_pause = faulted(&receipt);
    snapshot.last_turn_id = Some("settled".into());
    let before = snapshot.clone();
    sqlx::query(concat!(
        "INSERT INTO acp_submissions(worker_session_id,client_id,turn_id,thread_id,",
        "native_session_id,input_json,state,outcome_json,created_at_ms) VALUES(?1,'c','settled',",
        "'thread-commit-wake','native','{\"queue\":[]}','completed','{}',0)",
    ))
    .bind(&f.harness.inner.worker_session_id)
    .execute(f.repo.db().writer_pool())
    .await
    .unwrap();
    crate::acp_planner::recovery::recover(
        f.repo.as_ref(),
        &f.harness.inner.worker_session_id,
        f.harness.inner.card_id.as_str(),
        f.harness.inner.track_id.as_str(),
        &mut snapshot,
    )
    .await
    .unwrap();
    assert_eq!(snapshot, before);
    for statement in [
        "UPDATE acp_submissions SET thread_id='foreign'",
        "UPDATE acp_submissions SET thread_id='thread-commit-wake',state='unknown',outcome_json=NULL",
    ] {
        sqlx::query(statement)
            .execute(f.repo.db().writer_pool())
            .await
            .unwrap();
        assert!(
            crate::acp_planner::recovery::recover(
                f.repo.as_ref(),
                &f.harness.inner.worker_session_id,
                f.harness.inner.card_id.as_str(),
                f.harness.inner.track_id.as_str(),
                &mut snapshot
            )
            .await
            .is_err()
        );
        assert_eq!(snapshot, before);
    }
}

#[tokio::test]
async fn handoff_pause_fault_release_unknown_storage_stops_without_partial_state() {
    let f = running().await;
    let receipt = f.harness.pause_for_handoff(request(&f)).await.unwrap();
    error(&f).await;
    let before = f.harness.snapshot().await;
    assert_eq!(before.handoff_pause, faulted(&receipt));
    f.repo.pool().close().await;
    assert!(f.harness.resume_from_handoff(receipt).await.is_err());
    assert!(f.harness.inner.shutting_down.load(Ordering::SeqCst));
    assert_eq!(f.harness.snapshot().await, before);
    assert_eq!(f.daemon.turn_start_count_for_test(), 0);
}
