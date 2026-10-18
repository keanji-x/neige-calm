//! Whole production begin/restart regressions, without seeded success receipts.
use super::completed_commit_tests::Fixture;
use super::handoff_activation_test_support::runtime_with_repo;
use super::handoff_coordinator_tests::{active, entered, request, runtime};
use super::*;
use crate::db::prelude::*;
use crate::harness::handoff_pause::PauseState;
use crate::model::CardRole;
use crate::operation::handoff_activation::ActivationResult;
use crate::operation::handoff_coordinator::{self as coordinator, CoordinatorResult};

fn file_url(dir: &tempfile::TempDir) -> String {
    format!(
        "sqlite://{}?mode=rwc",
        dir.path().join("workflow.db").display()
    )
}

async fn snapshot(f: &Fixture) -> HarnessSnapshot {
    super::handoff_activation_test_support::stored(f, &f.harness.inner.worker_session_id).await
}

async fn roles(a: &Fixture, b: &Fixture) -> (String, String) {
    sqlx::query_as("SELECT s.role,t.role FROM cards s,cards t WHERE s.id=?1 AND t.id=?2")
        .bind(a.harness.inner.card_id.as_str())
        .bind(b.harness.inner.card_id.as_str())
        .fetch_one(a.repo.pool())
        .await
        .unwrap()
}

async fn reserved_track_cannot_close_or_delete(a: &Fixture) {
    assert_eq!(active(a).await, 1);
    assert!(
        sqlx::query("UPDATE tracks SET closed_at=1 WHERE id=?1")
            .bind(a.harness.inner.track_id.as_str())
            .execute(a.repo.pool())
            .await
            .is_err()
    );
    assert!(
        sqlx::query("DELETE FROM tracks WHERE id=?1")
            .bind(a.harness.inner.track_id.as_str())
            .execute(a.repo.pool())
            .await
            .is_err()
    );
}

async fn terminal_track_can_close_and_delete(a: &Fixture) {
    assert_eq!(active(a).await, 0);
    sqlx::query("UPDATE tracks SET closed_at=1 WHERE id=?1")
        .bind(a.harness.inner.track_id.as_str())
        .execute(a.repo.pool())
        .await
        .unwrap();
    a.repo
        .track_delete(a.harness.inner.track_id.as_str())
        .await
        .unwrap();
    assert!(
        a.repo
            .track_get(a.harness.inner.track_id.as_str())
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn handoff_workflow_reserved_restart_without_cancel_never_reacquires_live_approval() {
    let dir = tempfile::tempdir().unwrap();
    let url = file_url(&dir);
    let a = Fixture::for_role_at(CardRole::Planner, &url).await;
    let b = a.sibling(CardRole::Assistant).await;
    let (original, registry) = runtime(&a, &b);
    let request = request(&a, &b).await;
    let hook = crate::test_seams::PausePoint::default();
    crate::test_seams::install_pause_for_test("handoff-begin-admitted", &request.key, hook.clone());
    let begin = {
        let runtime = original.clone();
        let request = request.clone();
        tokio::spawn(async move { coordinator::begin(&runtime, request).await })
    };
    entered(&hook).await;
    let op = original
        .find_by_kind_and_idempotency("handoff-coordinator", &request.key)
        .await
        .unwrap()
        .unwrap();
    assert!(op.tx_output.is_none());
    let input = op.payload.clone();
    assert!(input["source"]["claim_id"].as_str().is_some());
    assert!(input["target"]["claim_id"].as_str().is_some());
    reserved_track_cannot_close_or_delete(&a).await;
    begin.abort();
    assert!(begin.await.unwrap_err().is_cancelled());
    hook.release.notify_one();
    drop(original);

    // The same session becomes busy, then idle, through real durable queue commands.
    let entry = QueueEntry::user_message("intervening user input".into(), None, vec![]);
    let entry_id = entry.id().unwrap().clone();
    b.enqueue(vec![entry]).await;
    b.harness
        .mutate_pending_entry(
            crate::harness::queue::QueueMutation::Delete {
                entry_id,
                if_entry_rev: 0,
            },
            ActorId::User,
        )
        .await
        .unwrap()
        .unwrap();
    assert!(snapshot(&b).await.pending_entries().is_empty());
    let reopened = Arc::new(crate::db::sqlite::SqlxRepo::open(&url).await.unwrap());
    let restored = runtime_with_repo(&a, reopened, registry);
    restored
        .apply_recovery(restored.recover_on_boot().await.unwrap())
        .await
        .unwrap();
    restored.drive().await.unwrap();
    let after = restored
        .find_by_kind_and_idempotency("handoff-coordinator", &request.key)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(after.id, op.id);
    assert_eq!(
        after.payload, input,
        "original claims cannot be replaced on recovery"
    );
    assert_eq!(roles(&a, &b).await, ("planner".into(), "assistant".into()));
    assert_eq!(snapshot(&a).await.handoff_pause, PauseState::Unpaused);
    assert_eq!(snapshot(&b).await.handoff_pause, PauseState::Unpaused);
    assert_eq!(a.daemon.started_thread_params_for_test().len(), 0);
    assert!(matches!(
        coordinator::query(&restored, &request).await.unwrap(),
        CoordinatorResult::CancelledOrRefused
    ));
    terminal_track_can_close_and_delete(&a).await;
}

#[tokio::test]
async fn handoff_workflow_committed_prepare_abort_reopens_and_explicitly_forwards_original_key() {
    let dir = tempfile::tempdir().unwrap();
    let url = file_url(&dir);
    let a = Fixture::for_role_at(CardRole::Planner, &url).await;
    let b = a.sibling(CardRole::Assistant).await;
    let (original, registry) = runtime(&a, &b);
    let request = request(&a, &b).await;
    sqlx::query("CREATE TRIGGER fail_activation_prepare BEFORE UPDATE OF tx_output_json ON operations WHEN OLD.kind='handoff-activation' AND OLD.phase='pending' BEGIN SELECT RAISE(ABORT,'activation prepare storage unavailable'); END")
        .execute(a.repo.pool()).await.unwrap();
    let id = coordinator::begin(&original, request.clone())
        .await
        .unwrap();
    assert_eq!(
        roles(&a, &b).await,
        ("planner_history".into(), "planner".into())
    );
    let mut claims = vec![];
    for f in [&a, &b] {
        let pause = snapshot(f).await.handoff_pause;
        assert!(matches!(pause, PauseState::ConsumedByHandoff { .. }));
        claims.push(pause);
    }
    assert_eq!(a.daemon.started_thread_params_for_test().len(), 0);
    let CoordinatorResult::Forward { cutover, .. } =
        coordinator::query(&original, &request).await.unwrap()
    else {
        panic!("identity commit must be durable before activation preparation");
    };
    let crate::operation::handoff_cutover::CutoverResult::CutoverCommittedPendingActivation {
        operation_id,
        planner_session_id,
        ..
    } = *cutover;
    let child = original
        .find_by_kind_and_idempotency("handoff-activation", &operation_id)
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(child.phase, crate::operation::Phase::Stuck { .. }));
    assert!(child.tx_output.is_none());
    reserved_track_cannot_close_or_delete(&a).await;
    drop(original);
    let reopened = Arc::new(crate::db::sqlite::SqlxRepo::open(&url).await.unwrap());
    sqlx::query("DROP TRIGGER fail_activation_prepare")
        .execute(reopened.pool())
        .await
        .unwrap();
    let restored = runtime_with_repo(&a, reopened, registry.clone());
    restored
        .apply_recovery(restored.recover_on_boot().await.unwrap())
        .await
        .unwrap();
    coordinator::retry_activation(&restored, &request, child.attempt)
        .await
        .unwrap();
    assert_eq!(
        coordinator::begin(&restored, request.clone())
            .await
            .unwrap(),
        id
    );
    assert_eq!(
        roles(&a, &b).await,
        ("planner_history".into(), "planner".into())
    );
    assert_eq!(snapshot(&a).await.handoff_pause, claims[0]);
    assert_eq!(snapshot(&b).await.handoff_pause, claims[1]);
    assert!(
        matches!(
            coordinator::query(&restored, &request).await.unwrap(),
            CoordinatorResult::Forward {
                activation: ActivationResult::Activated { .. },
                ..
            }
        ),
        "storage repaired: original explicit generation must have an executable forward path"
    );
    assert_eq!(a.daemon.started_thread_params_for_test().len(), 1);
    registry
        .shutdown_and_remove(&planner_session_id)
        .await
        .unwrap();
    terminal_track_can_close_and_delete(&a).await;
}

#[tokio::test]
async fn handoff_workflow_activated_lost_answer_remains_absorbing_after_hold_and_recovery() {
    let dir = tempfile::tempdir().unwrap();
    let url = file_url(&dir);
    let a = Fixture::for_role_at(CardRole::Planner, &url).await;
    let b = a.sibling(CardRole::Assistant).await;
    let (original, registry) = runtime(&a, &b);
    let request = request(&a, &b).await;
    let hook = crate::test_seams::PausePoint::default();
    crate::test_seams::install_pause_for_test(
        "handoff-activation-committed",
        b.harness.inner.card_id.as_str(),
        hook.clone(),
    );
    let begin = {
        let runtime = original.clone();
        let request = request.clone();
        tokio::spawn(async move { coordinator::begin(&runtime, request).await })
    };
    entered(&hook).await;
    let CoordinatorResult::Forward {
        cutover,
        activation,
    } = coordinator::query(&original, &request).await.unwrap()
    else {
        panic!("real final checkpoint must have committed");
    };
    let crate::operation::handoff_cutover::CutoverResult::CutoverCommittedPendingActivation {
        operation_id,
        planner_session_id,
        ..
    } = *cutover;
    assert_eq!(
        activation,
        ActivationResult::Activated {
            session_id: planner_session_id.clone()
        }
    );
    assert_eq!(
        active(&a).await,
        0,
        "terminal and reservation committed together"
    );
    let child = original
        .find_by_kind_and_idempotency("handoff-activation", &operation_id)
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(
        child.phase,
        crate::operation::Phase::AppServerInteract { .. }
    ));
    let source_claim = snapshot(&a).await.handoff_pause;
    let target_claim = snapshot(&b).await.handoff_pause;
    let installed = registry.get(&planner_session_id).unwrap();
    assert_eq!(a.daemon.started_thread_params_for_test().len(), 1);
    begin.abort();
    assert!(begin.await.unwrap_err().is_cancelled());
    hook.release.notify_one();
    drop(original);
    a.daemon
        .emit_notification_for_test(crate::codex_appserver::Notification::Other {
            method: "error".into(),
            params: serde_json::json!({"error":{"codexErrorInfo":"unauthorized"}}),
        });
    assert!(a.daemon.authentication_hold().is_some());
    let reopened = Arc::new(crate::db::sqlite::SqlxRepo::open(&url).await.unwrap());
    let restored = runtime_with_repo(&a, reopened, registry.clone());
    restored
        .apply_recovery(restored.recover_on_boot().await.unwrap())
        .await
        .unwrap();
    coordinator::retry_activation(&restored, &request, child.attempt)
        .await
        .unwrap();
    let after = coordinator::query(&restored, &request).await.unwrap();
    assert_eq!(
        roles(&a, &b).await,
        ("planner_history".into(), "planner".into())
    );
    assert_eq!(snapshot(&a).await.handoff_pause, source_claim);
    assert_eq!(snapshot(&b).await.handoff_pause, target_claim);
    assert_eq!(active(&a).await, 0);
    assert_eq!(a.daemon.started_thread_params_for_test().len(), 1);
    assert!(
        Arc::ptr_eq(
            &installed.inner,
            &registry.get(&planner_session_id).unwrap().inner
        ),
        "historical success must not reinstall the current owner"
    );
    assert!(
        matches!(
            after,
            CoordinatorResult::Forward {
                activation: ActivationResult::Activated { .. },
                ..
            }
        ),
        "committed Activated cannot downgrade after later hold: {after:?}"
    );
    registry
        .shutdown_and_remove(&planner_session_id)
        .await
        .unwrap();
    terminal_track_can_close_and_delete(&a).await;
}
