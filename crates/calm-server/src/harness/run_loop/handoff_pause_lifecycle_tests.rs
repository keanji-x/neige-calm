//! Production notification, accepted release cancellation, real replacement and file reopen.
use super::completed_commit_tests::Fixture;
use super::handoff_pause_fault_tests::{allow_snapshot, error, refuse_snapshot};
use super::handoff_pause_tests::{params, request};
use super::*;
use crate::db::prelude::*;
use crate::harness::handoff_pause::{PauseState, ReleaseOutcome};
use crate::session_projection_repo::WorkerSessionState;

#[tokio::test]
async fn handoff_pause_fault_release_lost_reply_survives_owner_exit() {
    let dir = tempfile::tempdir().unwrap();
    let url = format!(
        "sqlite://{}?mode=rwc",
        dir.path().join("released.sqlite").display()
    );
    let mut f = Fixture::for_role_at(crate::model::CardRole::Planner, &url).await;
    f.harness = PlannerHarness::run(params(&f, f.stored().await));
    let receipt = f.harness.pause_for_handoff(request(&f)).await.unwrap();
    error(&f).await;
    let hook = crate::test_seams::PausePoint::default();
    crate::test_seams::install_pause_for_test(
        crate::test_seams::HANDOFF_PAUSE_BEFORE_WRITE,
        f.harness.inner.card_id.as_str(),
        hook.clone(),
    );
    let harness = f.harness.clone();
    let submitted = receipt.clone();
    let call = tokio::spawn(async move { harness.resume_from_handoff(submitted).await });
    hook.entered.notified().await;
    call.abort();
    let _ = call.await;
    hook.release.notify_one();
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if f.repo
                .session_projection_by_id(&f.harness.inner.worker_session_id)
                .await
                .unwrap()
                .unwrap()
                .status
                == WorkerSessionState::Failed
            {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("accepted release commits before owner exit");
    f.harness.shutdown().await.unwrap();
    assert_eq!(
        f.repo
            .session_projection_by_id(&f.harness.inner.worker_session_id)
            .await
            .unwrap()
            .unwrap()
            .status,
        WorkerSessionState::Failed
    );
    let expected = ReleaseOutcome::ReleasedWithBackendFault {
        fault: crate::harness::handoff_pause::BackendFault::ThreadSystemError,
    };
    assert_eq!(
        f.harness
            .resume_from_handoff(receipt.clone())
            .await
            .unwrap(),
        expected
    );
    assert!(f.harness.pause_for_handoff(request(&f)).await.is_err());
    f.repo.pool().close().await;
    let repo = crate::db::sqlite::SqlxRepo::open(&url).await.unwrap();
    let original = receipt.request().clone();
    drop(f); // No live handle or registry participates in this read.
    assert_eq!(
        crate::db::handoff_pause::query_handoff_pause(&repo, original)
            .await
            .unwrap(),
        crate::harness::handoff_pause::PauseQuery::Recorded(PauseState::Released {
            receipt,
            outcome: expected,
        })
    );
}

#[tokio::test]
async fn handoff_pause_aborted_fault_replacement_never_releases_healthy() {
    aborted_fault_recovery(false).await;
}

#[tokio::test]
async fn handoff_pause_aborted_fault_file_reopen_never_releases_healthy() {
    aborted_fault_recovery(true).await;
}

async fn aborted_fault_recovery(reopen: bool) {
    let dir = tempfile::tempdir().unwrap();
    let url = format!(
        "sqlite://{}?mode=rwc",
        dir.path().join("pause.sqlite").display()
    );
    let mut f = Fixture::for_role_at(crate::model::CardRole::Planner, &url).await;
    f.harness = PlannerHarness::run(params(&f, f.stored().await));
    let registry = crate::harness::HarnessRegistry::new();
    assert!(
        registry
            .try_reserve(f.harness.inner.worker_session_id.clone())
            .unwrap()
            .install(f.harness.clone())
    );
    let receipt = f.harness.pause_for_handoff(request(&f)).await.unwrap();
    let before = f.stored().await;
    refuse_snapshot(&f).await;
    error(&f).await;
    assert!(f.harness.inner.shutting_down.load(Ordering::SeqCst));
    assert_eq!(
        f.stored().await,
        before,
        "ABORT leaves original SQL checkpoint"
    );
    f.harness.shutdown().await.unwrap();
    allow_snapshot(&f).await;
    if reopen {
        f.repo.pool().close().await;
        f.repo = Arc::new(crate::db::sqlite::SqlxRepo::open(&url).await.unwrap());
        f.daemon =
            crate::shared_codex_appserver::SharedCodexAppServer::new_fake_running_with_pending(
                f.repo.clone(),
                None,
            );
    }
    let runtime = f
        .repo
        .session_projection_by_id(&f.harness.inner.worker_session_id)
        .await
        .unwrap()
        .unwrap();
    f.harness = crate::harness::spawn_recovered_harness(
        f.repo.clone(),
        f.harness.inner.events.clone(),
        f.harness.inner.card_role_cache.clone(),
        f.harness.inner.track_area_cache.clone(),
        f.daemon.clone(),
        f.daemon.thread_seals().clone(),
        &crate::claude_planner::wiring::ClaudePlannerWiring::unconfigured_for_test(f.repo.clone()),
        &registry,
        &crate::per_card_lock::new_keyed_locks(),
        runtime,
        crate::harness::ClaimMode::Replace,
    )
    .await
    .unwrap()
    .installed()
    .expect("actual recovery installs paused owner");
    assert_eq!(
        f.stored().await.handoff_pause,
        PauseState::RecoveryHealthUnconfirmed {
            receipt: receipt.clone()
        }
    );
    let outcome = f
        .harness
        .resume_from_handoff(receipt.clone())
        .await
        .unwrap();
    assert_ne!(
        outcome,
        ReleaseOutcome::HealthyReleased,
        "consumed fault cannot become health proof (file reopen={reopen})"
    );
    assert_eq!(outcome, ReleaseOutcome::HealthUnconfirmedAfterRecovery);
    let row = f
        .repo
        .session_projection_by_id(&f.harness.inner.worker_session_id)
        .await
        .unwrap()
        .unwrap();
    let raw = row.handle_state_json.clone().unwrap();
    assert_eq!(row.status, WorkerSessionState::Failed);
    assert!(
        f.repo
            .session_projection_system_error_recovery_matches(&row, "thread-commit-wake")
            .await
            .unwrap()
    );
    f.harness.quiesce_system_error_for_recovery().await.unwrap();
    assert_eq!(
        f.repo
            .session_projection_handle_state_by_id(&row.id)
            .await
            .unwrap()
            .unwrap(),
        raw
    );
    if reopen {
        f.repo.pool().close().await;
        f.repo = Arc::new(crate::db::sqlite::SqlxRepo::open(&url).await.unwrap());
    }
    assert_eq!(
        crate::db::handoff_pause::query_handoff_pause(f.repo.as_ref(), receipt.request().clone())
            .await
            .unwrap(),
        crate::harness::handoff_pause::PauseQuery::Recorded(PauseState::Released {
            receipt,
            outcome
        })
    );
    f.harness.shutdown().await.unwrap();
}

#[tokio::test]
async fn handoff_pause_terminal_replay_new_claim_and_unknown_matrix() {
    use crate::harness::handoff_pause::{PauseQuery, PauseReceipt, PauseRequest};
    let f = Fixture::new().await;
    let claim = request(&f);
    assert_eq!(
        crate::db::handoff_pause::query_handoff_pause(f.repo.as_ref(), claim.clone())
            .await
            .unwrap(),
        PauseQuery::Unknown
    );
    assert!(
        f.harness
            .resume_from_handoff(PauseReceipt::from_request(claim.clone()))
            .await
            .is_err()
    );
    let receipt = f.harness.pause_for_handoff(claim.clone()).await.unwrap();
    assert_eq!(
        f.harness
            .resume_from_handoff(receipt.clone())
            .await
            .unwrap(),
        ReleaseOutcome::HealthyReleased
    );
    let terminal = f.stored().await;
    // These operations are factual replay: a rejecting write trigger cannot affect them.
    refuse_snapshot(&f).await;
    assert_eq!(
        f.harness
            .resume_from_handoff(receipt.clone())
            .await
            .unwrap(),
        ReleaseOutcome::HealthyReleased
    );
    assert_eq!(
        crate::db::handoff_pause::query_handoff_pause(f.repo.as_ref(), claim.clone())
            .await
            .unwrap(),
        PauseQuery::Recorded(PauseState::Released {
            receipt: receipt.clone(),
            outcome: ReleaseOutcome::HealthyReleased
        })
    );
    assert!(f.harness.pause_for_handoff(claim.clone()).await.is_err());
    assert_eq!(f.stored().await, terminal);
    allow_snapshot(&f).await;
    // A terminal result is not an active pause: ordinary system writers still work.
    super::handoff_pause_tests::system(&f, 81).await;
    let new_claim = request(&f);
    let second = f.harness.pause_for_handoff(new_claim).await.unwrap();
    assert!(
        crate::db::handoff_pause::query_handoff_pause(f.repo.as_ref(), claim.clone())
            .await
            .is_err()
    );
    assert!(f.harness.resume_from_handoff(receipt).await.is_err());
    assert!(
        crate::db::handoff_pause::query_handoff_pause(
            f.repo.as_ref(),
            PauseRequest::new(
                f.harness.inner.card_id.clone(),
                "wrong-session".into(),
                f.harness.inner.track_id.clone()
            )
        )
        .await
        .is_err()
    );
    assert_eq!(f.stored().await.handoff_pause.receipt(), Some(&second));
    f.repo.pool().close().await;
    assert!(
        crate::db::handoff_pause::query_handoff_pause(f.repo.as_ref(), second.request().clone())
            .await
            .is_err()
    );
}

#[tokio::test]
async fn handoff_pause_durable_query_rejects_forged_storage_binding() {
    use crate::harness::handoff_pause::{PauseReceipt, PauseRequest};
    let f = Fixture::new().await;
    let other = f.sibling(crate::model::CardRole::Assistant).await;
    // Valid receipt syntax and internally consistent request/snapshot are insufficient:
    // SQL's immutable original session/card relationship must agree as well.
    let forged = PauseRequest::new(
        other.harness.inner.card_id.clone(),
        f.harness.inner.worker_session_id.clone(),
        f.harness.inner.track_id.clone(),
    );
    let mut snapshot = f.stored().await;
    snapshot.handoff_pause = PauseState::Paused {
        receipt: PauseReceipt::from_request(forged.clone()),
    };
    sqlx::query("UPDATE worker_sessions SET handle_state_json=?1 WHERE id=?2")
        .bind(serde_json::to_string(&snapshot).unwrap())
        .bind(&f.harness.inner.worker_session_id)
        .execute(f.repo.pool())
        .await
        .unwrap();
    assert!(
        crate::db::handoff_pause::query_handoff_pause(f.repo.as_ref(), forged)
            .await
            .is_err()
    );
}

#[tokio::test]
async fn handoff_pause_terminal_query_uses_original_session_after_pointer_moves() {
    use crate::harness::handoff_pause::PauseQuery;
    let f = Fixture::new().await;
    let receipt = f.harness.pause_for_handoff(request(&f)).await.unwrap();
    assert_eq!(
        f.harness
            .resume_from_handoff(receipt.clone())
            .await
            .unwrap(),
        ReleaseOutcome::HealthyReleased
    );
    let terminal = f.stored().await;
    use crate::db::sqlite::{session_mark_superseded_runtime_tx, session_start_runtime_tx};
    use crate::session_projection_repo::{AgentProvider, WorkerSessionInit, WorkerSessionKind};
    let replacement_id = crate::model::new_id();
    let fresh = HarnessSnapshot::initial(0, vec![]);
    let fresh_raw = serde_json::to_value(&fresh).unwrap();
    let mut tx = f.repo.pool().begin().await.unwrap();
    session_mark_superseded_runtime_tx(&mut tx, &f.harness.inner.worker_session_id)
        .await
        .unwrap();
    session_start_runtime_tx(
        &mut tx,
        WorkerSessionInit {
            id: replacement_id.clone(),
            card_id: f.harness.inner.card_id.to_string(),
            kind: WorkerSessionKind::SharedPlanner,
            agent_provider: Some(AgentProvider::Codex),
            status: WorkerSessionState::Idle,
            terminal_run_id: None,
            thread_id: None,
            session_id: None,
            active_turn_id: None,
            handle_state_json: Some(fresh_raw.clone()),
            spawn_op_id: None,
            now_ms: crate::model::now_ms(),
        },
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    refuse_snapshot(&f).await;
    assert_eq!(
        crate::db::handoff_pause::query_handoff_pause(f.repo.as_ref(), receipt.request().clone())
            .await
            .unwrap(),
        PauseQuery::Recorded(terminal.handoff_pause)
    );
    assert_eq!(
        f.harness.resume_from_handoff(receipt).await.unwrap(),
        ReleaseOutcome::HealthyReleased
    );
    assert!(f.harness.pause_for_handoff(request(&f)).await.is_err());
    assert_eq!(
        f.repo
            .session_projection_handle_state_by_id(&replacement_id)
            .await
            .unwrap()
            .unwrap(),
        fresh_raw
    );
    allow_snapshot(&f).await;
}

#[tokio::test]
async fn handoff_pause_terminal_decoder_rejects_invalid_data() {
    let f = Fixture::new().await;
    let receipt = f.harness.pause_for_handoff(request(&f)).await.unwrap();
    let mut snapshot = f.stored().await;
    for outcome in [
        ReleaseOutcome::HealthyReleased,
        ReleaseOutcome::HealthUnconfirmedAfterRecovery,
        ReleaseOutcome::ReleasedWithBackendFault {
            fault: crate::harness::handoff_pause::BackendFault::ThreadSystemError,
        },
    ] {
        snapshot.handoff_pause = PauseState::Released {
            receipt: receipt.clone(),
            outcome,
        };
        assert_eq!(
            HarnessSnapshot::parse_known(serde_json::to_value(&snapshot).unwrap()),
            Some(snapshot.clone())
        );
    }
    for bad in [
        serde_json::json!({"state":"released","receipt":receipt}),
        serde_json::json!({"state":"released","receipt":receipt,"outcome":null}),
        serde_json::json!({"state":"released","receipt":receipt,"outcome":{"kind":"other"}}),
        serde_json::json!({"state":"recovery_health_unconfirmed","receipt":null}),
    ] {
        let mut raw = serde_json::to_value(&snapshot).unwrap();
        raw["handoff_pause"] = bad;
        assert!(HarnessSnapshot::parse_known(raw).is_none());
    }
}
