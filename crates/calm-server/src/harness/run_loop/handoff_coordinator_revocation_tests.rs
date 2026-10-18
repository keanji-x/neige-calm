//! Live original futures across a real second-runtime SQL lease takeover.
use super::completed_commit_tests::Fixture;
use super::handoff_activation_test_support::runtime_with_repo;
use super::handoff_coordinator_tests::{active, entered, progress, request, runtime, start_at};
use crate::harness::handoff_pause::PauseState;
use crate::model::CardRole;
use crate::operation::handoff_coordinator::{self as coordinator, CoordinatorResult};
use std::sync::Arc;

#[tokio::test]
async fn handoff_coordinator_live_source_intent_cannot_land_after_serial_absence() {
    live_intent_after_absence(true).await;
}

#[tokio::test]
async fn handoff_coordinator_live_target_intent_cannot_land_after_serial_absence() {
    live_intent_after_absence(false).await;
}

async fn live_intent_after_absence(source: bool) {
    let dir = tempfile::tempdir().unwrap();
    let url = format!("sqlite://{}?mode=rwc", dir.path().join("live.db").display());
    let mut a = Fixture::for_role_at(CardRole::Planner, &url).await;
    a.harness = crate::harness::PlannerHarness::run(super::handoff_pause_tests::params(
        &a,
        a.stored().await,
    ));
    let mut b = a.sibling(CardRole::Assistant).await;
    b.harness = crate::harness::PlannerHarness::run(super::handoff_pause_tests::params(
        &b,
        b.stored().await,
    ));
    let (original, registry) = runtime(&a, &b);
    let approval = request(&a, &b).await;
    let stage = if source {
        "SourceIntent"
    } else {
        "TargetIntent"
    };
    let (id, send, live) = start_at(&original, &approval, stage).await;
    entered(&send).await;
    let old_lease: String = sqlx::query_scalar("SELECT lease_owner FROM operations WHERE id=?1")
        .bind(&id)
        .fetch_one(a.repo.pool())
        .await
        .unwrap();
    // Only advance the persisted lease deadline, as in the repository claim tests.
    // No authored claim, business progress, phase, or cancellation replaces takeover.
    sqlx::query("UPDATE operations SET lease_until_ms=0 WHERE id=?1")
        .bind(&id)
        .execute(a.repo.pool())
        .await
        .unwrap();
    let reopened = Arc::new(crate::db::sqlite::SqlxRepo::open(&url).await.unwrap());
    let recovery = runtime_with_repo(&a, reopened, registry);
    let settling = crate::test_seams::PausePoint::default();
    crate::test_seams::install_pause_for_test(
        "handoff-coordinator-before-settlement",
        &approval.source_session_id,
        settling.clone(),
    );
    let takeover = tokio::spawn(async move {
        recovery.drive().await.unwrap();
        recovery
    });
    entered(&settling).await;
    assert!(
        !live.is_finished(),
        "original begin capability is still live"
    );
    let new_lease: String = sqlx::query_scalar("SELECT lease_owner FROM operations WHERE id=?1")
        .bind(&id)
        .fetch_one(a.repo.pool())
        .await
        .unwrap();
    assert_ne!(
        old_lease, new_lease,
        "production claim changed the lease owner"
    );
    let p = progress(&a, &id).await;
    assert_eq!(p["state"], "compensating");
    assert_eq!(
        p["claims"][if source { "source" } else { "target" }]["state"],
        "untouched"
    );
    assert_eq!(active(&a).await, 1);
    // Resolve has already answered Unknown, but Finished has not been persisted.
    send.release.notify_one();
    live.await.unwrap();
    let late = if source {
        a.stored().await
    } else {
        b.stored().await
    };
    settling.release.notify_one();
    let recovery = takeover.await.unwrap();
    assert!(matches!(
        coordinator::query(&recovery, &approval).await.unwrap(),
        CoordinatorResult::CancelledOrRefused
    ));
    assert_eq!(active(&a).await, 0);
    a.harness.shutdown().await.unwrap();
    b.harness.shutdown().await.unwrap();
    assert!(
        !late.handoff_pause.is_paused(),
        "late original pause committed after absence"
    );
    assert!(!a.stored().await.handoff_pause.is_paused());
    assert!(!b.stored().await.handoff_pause.is_paused());
    assert_eq!(a.daemon.started_thread_params_for_test().len(), 0);
    if source {
        assert_eq!(a.stored().await.handoff_pause, PauseState::Unpaused);
    }
}

#[tokio::test]
async fn handoff_coordinator_final_pause_and_revocation_transactions_serialize_both_orders() {
    use crate::harness::handoff_pause::ReleaseOutcome;
    for source in [true, false] {
        for pause_first in [true, false] {
            let a = Fixture::new().await;
            let b = a.sibling(CardRole::Assistant).await;
            let (original, registry) = runtime(&a, &b);
            let approval = request(&a, &b).await;
            let (id, send, live) = start_at(
                &original,
                &approval,
                if source {
                    "SourceIntent"
                } else {
                    "TargetIntent"
                },
            )
            .await;
            entered(&send).await;
            sqlx::query("UPDATE operations SET lease_until_ms=0 WHERE id=?1")
                .bind(&id)
                .execute(a.repo.pool())
                .await
                .unwrap();
            let before = super::handoff_coordinator_tests::hook(&id, "BeforeCompensation");
            let revoked = super::handoff_coordinator_tests::hook(&id, "Compensating");
            let recovery = runtime_with_repo(&a, a.repo.clone(), registry);
            let takeover = tokio::spawn(async move {
                recovery.drive().await.unwrap();
                recovery
            });
            entered(&before).await;
            let side = if source { &a } else { &b };
            let writing = crate::test_seams::PausePoint::default();
            crate::test_seams::install_pause_for_test(
                crate::test_seams::HANDOFF_PAUSE_BEFORE_WRITE,
                side.harness.inner.card_id.as_str(),
                writing.clone(),
            );
            send.release.notify_one();
            entered(&writing).await;
            let receipt = if pause_first {
                writing.release.notify_one();
                live.await.unwrap();
                let receipt = side.stored().await.handoff_pause.receipt().unwrap().clone();
                assert!(side.stored().await.handoff_pause.is_paused());
                before.release.notify_one();
                entered(&revoked).await;
                Some(receipt)
            } else {
                before.release.notify_one();
                entered(&revoked).await;
                writing.release.notify_one();
                live.await.unwrap();
                assert!(!side.stored().await.handoff_pause.is_paused());
                None
            };
            assert_eq!(progress(&a, &id).await["state"], "compensating");
            assert_eq!(active(&a).await, 1);
            // Neither changing technical output nor changing the exact binding can rearm it.
            assert!(sqlx::query("UPDATE operations SET tx_output_json=json_set(tx_output_json,'$.result.state','pausing') WHERE id=?1")
                .bind(&id).execute(a.repo.pool()).await.is_err());
            assert!(sqlx::query("UPDATE operations SET payload_json=json_set(payload_json,?1,'replacement') WHERE id=?2")
                .bind(if source { "$.source.claim_id" } else { "$.target.claim_id" })
                .bind(&id).execute(a.repo.pool()).await.is_err());
            revoked.release.notify_one();
            let recovery = takeover.await.unwrap();
            assert!(matches!(
                coordinator::query(&recovery, &approval).await.unwrap(),
                CoordinatorResult::CancelledOrRefused
            ));
            assert_eq!(active(&a).await, 0);
            assert!(!a.stored().await.handoff_pause.is_paused());
            assert!(!b.stored().await.handoff_pause.is_paused());
            if let Some(receipt) = receipt {
                assert_eq!(
                    side.stored().await.handoff_pause,
                    PauseState::Released {
                        receipt,
                        outcome: ReleaseOutcome::HealthyReleased,
                    }
                );
            }
            // A different new original claim remains available to the ordinary pause owner.
            let fresh = super::handoff_pause_tests::request(side);
            let fresh = side.harness.pause_for_handoff(fresh).await.unwrap();
            assert_eq!(
                side.harness.resume_from_handoff(fresh).await.unwrap(),
                ReleaseOutcome::HealthyReleased
            );
        }
    }
}

#[tokio::test]
async fn handoff_coordinator_ignored_revocation_retains_intent_until_file_reopen_recovery() {
    use crate::harness::handoff_pause::ReleaseOutcome;
    for source in [true, false] {
        let dir = tempfile::tempdir().unwrap();
        let url = format!(
            "sqlite://{}?mode=rwc",
            dir.path().join("ignored.db").display()
        );
        let a = Fixture::for_role_at(CardRole::Planner, &url).await;
        let b = a.sibling(CardRole::Assistant).await;
        let (original, registry) = runtime(&a, &b);
        let approval = request(&a, &b).await;
        let (id, send, live) = start_at(
            &original,
            &approval,
            if source {
                "SourceIntent"
            } else {
                "TargetIntent"
            },
        )
        .await;
        entered(&send).await;
        sqlx::query("UPDATE operations SET lease_until_ms=0 WHERE id=?1")
            .bind(&id)
            .execute(a.repo.pool())
            .await
            .unwrap();
        sqlx::query(concat!(
            "CREATE TRIGGER ignore_revocation BEFORE UPDATE OF tx_output_json ON operations ",
            "WHEN OLD.kind='handoff-workflow' AND json_extract(NEW.tx_output_json,'$.result.state')=",
            "'compensating' BEGIN SELECT RAISE(IGNORE); END",
        )).execute(a.repo.pool()).await.unwrap();
        let recovery = runtime_with_repo(&a, a.repo.clone(), registry.clone());
        recovery.drive().await.unwrap();
        let p = progress(&a, &id).await;
        assert_eq!(p["state"], "pausing");
        assert_eq!(
            p["claims"][if source { "source" } else { "target" }]["state"],
            "intent"
        );
        assert_eq!(active(&a).await, 1);
        assert!(matches!(
            coordinator::query(&recovery, &approval).await.unwrap(),
            CoordinatorResult::NeedsRecovery { .. }
        ));
        // A failed revocation has no authority to claim absence. The live original
        // can still land; exclusion must retain it for real reconciliation.
        send.release.notify_one();
        live.await.unwrap();
        let side = if source { &a } else { &b };
        let receipt = side.stored().await.handoff_pause.receipt().unwrap().clone();
        assert!(side.stored().await.handoff_pause.is_paused());
        assert_eq!(active(&a).await, 1);
        sqlx::query("DROP TRIGGER ignore_revocation")
            .execute(a.repo.pool())
            .await
            .unwrap();
        drop(recovery);
        let reopened = Arc::new(crate::db::sqlite::SqlxRepo::open(&url).await.unwrap());
        let recovery = runtime_with_repo(&a, reopened, registry);
        let op = recovery
            .find_by_kind_and_idempotency("handoff-workflow", &approval.key)
            .await
            .unwrap()
            .unwrap();
        coordinator::retry(&recovery, &approval, op.attempt)
            .await
            .unwrap();
        assert_eq!(
            side.stored().await.handoff_pause,
            PauseState::Released {
                receipt,
                outcome: ReleaseOutcome::HealthyReleased,
            }
        );
        assert_eq!(active(&a).await, 0);
    }
}

#[tokio::test]
async fn handoff_coordinator_revoked_existing_claim_keeps_fault_and_recovery_health_authority() {
    use crate::harness::handoff_pause::ReleaseOutcome;
    for source in [true, false] {
        for recovered in [false, true] {
            let mut a = super::handoff_pause_tests::running().await;
            let mut b = a.sibling(CardRole::Assistant).await;
            b.harness = crate::harness::PlannerHarness::run(super::handoff_pause_tests::params(
                &b,
                b.stored().await,
            ));
            let (runtime, registry) = runtime(&a, &b);
            let approval = request(&a, &b).await;
            let (id, paused, live) = start_at(
                &runtime,
                &approval,
                if source { "SourcePaused" } else { "BothPaused" },
            )
            .await;
            entered(&paused).await;
            let revoked = super::handoff_coordinator_tests::hook(&id, "Compensating");
            coordinator::cancel(&runtime, &approval).await.unwrap();
            paused.release.notify_one();
            entered(&revoked).await;
            let side = if source { &mut a } else { &mut b };
            let receipt = side.stored().await.handoff_pause.receipt().unwrap().clone();
            if recovered {
                super::handoff_pause_fault_tests::restart(side).await;
                registry.insert(
                    side.harness.inner.worker_session_id.clone(),
                    side.harness.clone(),
                );
            } else {
                super::handoff_pause_fault_tests::error(side).await;
            }
            assert!(matches!(
                side.stored().await.handoff_pause,
                PauseState::FaultedPaused { .. } | PauseState::RecoveryHealthUnconfirmed { .. }
            ));
            revoked.release.notify_one();
            live.await.unwrap();
            let side = if source { &a } else { &b };
            let outcome = if recovered {
                ReleaseOutcome::HealthUnconfirmedAfterRecovery
            } else {
                let PauseState::Released { outcome, .. } = side.stored().await.handoff_pause else {
                    panic!("must retain typed fault");
                };
                assert!(matches!(
                    outcome,
                    ReleaseOutcome::ReleasedWithBackendFault { .. }
                ));
                outcome
            };
            assert_eq!(
                side.stored().await.handoff_pause,
                PauseState::Released { receipt, outcome }
            );
            assert_eq!(active(&a).await, 1);
            assert!(matches!(
                coordinator::query(&runtime, &approval).await.unwrap(),
                CoordinatorResult::NeedsRecovery { .. }
            ));
            a.harness.shutdown().await.unwrap();
            b.harness.shutdown().await.unwrap();
        }
    }
}

#[tokio::test]
async fn handoff_coordinator_concurrent_final_writers_cannot_finish_with_paused_session() {
    for source in [true, false] {
        let dir = tempfile::tempdir().unwrap();
        let url = format!(
            "sqlite://{}?mode=rwc",
            dir.path().join("contending.db").display()
        );
        let a = Fixture::for_role_at(CardRole::Planner, &url).await;
        let b = a.sibling(CardRole::Assistant).await;
        let (original, registry) = runtime(&a, &b);
        let approval = request(&a, &b).await;
        let (id, send, live) = start_at(
            &original,
            &approval,
            if source {
                "SourceIntent"
            } else {
                "TargetIntent"
            },
        )
        .await;
        entered(&send).await;
        sqlx::query("UPDATE operations SET lease_until_ms=0 WHERE id=?1")
            .bind(&id)
            .execute(a.repo.pool())
            .await
            .unwrap();
        let before = super::handoff_coordinator_tests::hook(&id, "BeforeCompensation");
        let reopened = Arc::new(crate::db::sqlite::SqlxRepo::open(&url).await.unwrap());
        let recovery = runtime_with_repo(&a, reopened, registry);
        let takeover = tokio::spawn(async move {
            recovery.drive().await.unwrap();
            recovery
        });
        entered(&before).await;
        let writing = crate::test_seams::PausePoint::default();
        let side = if source { &a } else { &b };
        crate::test_seams::install_pause_for_test(
            crate::test_seams::HANDOFF_PAUSE_BEFORE_WRITE,
            side.harness.inner.card_id.as_str(),
            writing.clone(),
        );
        send.release.notify_one();
        entered(&writing).await;
        // Let both real final writers compete behind a third SQLite writer. The
        // deterministic order test above covers both winners; this test allows either.
        let blocker = crate::db::sqlite::begin_immediate_tx(a.repo.pool())
            .await
            .unwrap();
        before.release.notify_one();
        writing.release.notify_one();
        tokio::task::yield_now().await;
        blocker.rollback().await.unwrap();
        live.await.unwrap();
        let recovery = takeover.await.unwrap();
        assert!(matches!(
            coordinator::query(&recovery, &approval).await.unwrap(),
            CoordinatorResult::CancelledOrRefused
        ));
        assert_eq!(active(&a).await, 0);
        assert!(!a.stored().await.handoff_pause.is_paused());
        assert!(!b.stored().await.handoff_pause.is_paused());
        assert_eq!(a.daemon.started_thread_params_for_test().len(), 0);
    }
}

#[tokio::test]
async fn handoff_coordinator_revocation_survives_owner_loss_and_file_reopen() {
    for source in [true, false] {
        let dir = tempfile::tempdir().unwrap();
        let url = format!(
            "sqlite://{}?mode=rwc",
            dir.path().join("revoked.db").display()
        );
        let a = Fixture::for_role_at(CardRole::Planner, &url).await;
        let b = a.sibling(CardRole::Assistant).await;
        let (original, registry) = runtime(&a, &b);
        let approval = request(&a, &b).await;
        let (id, send, live) = start_at(
            &original,
            &approval,
            if source {
                "SourceIntent"
            } else {
                "TargetIntent"
            },
        )
        .await;
        entered(&send).await;
        sqlx::query("UPDATE operations SET lease_until_ms=0 WHERE id=?1")
            .bind(&id)
            .execute(a.repo.pool())
            .await
            .unwrap();
        let revoked = super::handoff_coordinator_tests::hook(&id, "Compensating");
        let recovery = runtime_with_repo(&a, a.repo.clone(), registry.clone());
        let takeover = tokio::spawn(async move {
            recovery.drive().await.unwrap();
        });
        entered(&revoked).await;
        assert_eq!(progress(&a, &id).await["state"], "compensating");
        // Lose only the compensator after commit and before Resolve. The original
        // begin future is never cancelled and still owns its live capability.
        takeover.abort();
        assert!(takeover.await.unwrap_err().is_cancelled());
        revoked.release.notify_one();
        let reopened = Arc::new(crate::db::sqlite::SqlxRepo::open(&url).await.unwrap());
        let recovery = runtime_with_repo(&a, reopened, registry);
        assert!(matches!(
            coordinator::query(&recovery, &approval).await.unwrap(),
            CoordinatorResult::NeedsRecovery { .. }
        ));
        send.release.notify_one();
        live.await.unwrap();
        let side = if source { &a } else { &b };
        assert!(
            !side.stored().await.handoff_pause.is_paused(),
            "persisted revocation survives loss of its owner"
        );
        assert_eq!(active(&a).await, 1);
        sqlx::query("UPDATE operations SET lease_until_ms=0 WHERE id=?1")
            .bind(&id)
            .execute(a.repo.pool())
            .await
            .unwrap();
        recovery.drive().await.unwrap();
        assert!(matches!(
            coordinator::query(&recovery, &approval).await.unwrap(),
            CoordinatorResult::CancelledOrRefused
        ));
        assert_eq!(active(&a).await, 0);
        assert!(!a.stored().await.handoff_pause.is_paused());
        assert!(!b.stored().await.handoff_pause.is_paused());
    }
}
