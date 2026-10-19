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
            .execute(a.repo.db().writer_pool())
            .await
            .is_err()
    );
    assert!(
        sqlx::query("DELETE FROM tracks WHERE id=?1")
            .bind(a.harness.inner.track_id.as_str())
            .execute(a.repo.db().writer_pool())
            .await
            .is_err()
    );
}

async fn terminal_track_can_close_and_delete(a: &Fixture) {
    assert_eq!(active(a).await, 0);
    sqlx::query("UPDATE tracks SET closed_at=1 WHERE id=?1")
        .bind(a.harness.inner.track_id.as_str())
        .execute(a.repo.db().writer_pool())
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
async fn handoff_coordinator_reserved_restart_without_cancel_never_reacquires_live_approval() {
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
        .find_by_kind_and_idempotency("handoff-workflow", &request.key)
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
        .find_by_kind_and_idempotency("handoff-workflow", &request.key)
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
    // Even a delayed original command after the serial absence barrier cannot recreate
    // the settled claim. This is the same original request, not a fresh approval.
    let late: crate::harness::handoff_pause::PauseRequest =
        serde_json::from_value(input["source"].clone()).unwrap();
    assert!(a.harness.pause_for_handoff(late).await.is_err());
    assert_eq!(snapshot(&a).await.handoff_pause, PauseState::Unpaused);
    terminal_track_can_close_and_delete(&a).await;
}

#[tokio::test]
async fn handoff_coordinator_committed_prepare_abort_reopens_and_explicitly_forwards_original_key()
{
    let dir = tempfile::tempdir().unwrap();
    let url = file_url(&dir);
    let a = Fixture::for_role_at(CardRole::Planner, &url).await;
    let b = a.sibling(CardRole::Assistant).await;
    let (original, registry) = runtime(&a, &b);
    let request = request(&a, &b).await;
    sqlx::query(
        r#"
        CREATE TRIGGER fail_activation_prepare BEFORE UPDATE OF tx_output_json ON operations WHEN
        OLD.kind='handoff-workflow' AND
        json_extract(OLD.tx_output_json,'$.result.resources.state')='unprepared' AND
        json_extract(NEW.tx_output_json,'$.result.resources.state')='prepared' BEGIN SELECT
        RAISE(ABORT,'activation prepare storage unavailable'); END
        "#,
    )
    .execute(a.repo.db().writer_pool())
    .await
    .unwrap();
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
        .find_by_kind_and_idempotency("handoff-workflow", &request.key)
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(
        child.phase,
        crate::operation::Phase::AwaitingRetry { .. }
    ));
    assert_eq!(
        child.tx_output.as_ref().unwrap().result["resources"]["state"],
        "unprepared"
    );
    assert_eq!(child.id, operation_id);
    assert_single_owner(&a).await;
    reserved_track_cannot_close_or_delete(&a).await;
    drop(original);
    let reopened = Arc::new(crate::db::sqlite::SqlxRepo::open(&url).await.unwrap());
    sqlx::query("DROP TRIGGER fail_activation_prepare")
        .execute(reopened.db().writer_pool())
        .await
        .unwrap();
    let restored = runtime_with_repo(&a, reopened, registry.clone());
    restored
        .apply_recovery(restored.recover_on_boot().await.unwrap())
        .await
        .unwrap();
    coordinator::retry(&restored, &request, child.attempt)
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
async fn handoff_coordinator_activated_lost_answer_remains_absorbing_after_hold_and_recovery() {
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
        .find_by_kind_and_idempotency("handoff-workflow", &request.key)
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(
        child.phase,
        crate::operation::Phase::AppServerInteract { .. }
    ));
    assert_eq!(child.id, operation_id);
    assert_single_owner(&a).await;
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
    coordinator::retry(&restored, &request, child.attempt)
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

async fn assert_single_owner(a: &Fixture) {
    let kinds: Vec<String> =
        sqlx::query_scalar("SELECT kind FROM operations WHERE kind LIKE 'handoff-%'")
            .fetch_all(a.repo.pool())
            .await
            .unwrap();
    assert_eq!(kinds, vec!["handoff-workflow"]);
}

// Use the archive route's production writer, never a hand-written archive INSERT.
async fn archive_target(b: &Fixture) {
    let mut tx = crate::db::sqlite::begin_immediate_tx(b.repo.db())
        .await
        .unwrap();
    assert!(
        crate::db::sqlite::conversation_archive_set_tx(
            &mut tx,
            b.harness.inner.card_id.as_str(),
            true,
        )
        .await
        .unwrap()
    );
    tx.commit().await.unwrap();
    assert!(
        crate::db::RepoRead::conversation_archived_at(
            b.repo.as_ref(),
            b.harness.inner.card_id.as_str()
        )
        .await
        .unwrap()
        .is_some()
    );
}

async fn authority_image(a: &Fixture) -> Vec<(String, Vec<String>)> {
    super::handoff_cutover_tests::data_image(a.repo.pool())
        .await
        .into_iter()
        .filter(|(name, _)| matches!(name.as_str(), "cards" | "tracks" | "card_mcp_tokens"))
        .collect()
}

#[tokio::test]
async fn handoff_archived_target_is_refused_at_admission() {
    let a = Fixture::new().await;
    let b = a.sibling(CardRole::Assistant).await;
    let (runtime, _) = runtime(&a, &b);
    archive_target(&b).await;
    let request = request(&a, &b).await;
    let before = super::handoff_cutover_tests::data_image(a.repo.pool()).await;
    let result = coordinator::begin(&runtime, request).await;
    assert!(
        matches!(result, Err(crate::error::CalmError::Conflict(_))),
        "{result:?}"
    );
    assert_eq!(
        super::handoff_cutover_tests::data_image(a.repo.pool()).await,
        before
    );
}

#[tokio::test]
async fn handoff_archived_target_after_precheck_is_refused_without_revoking_authority() {
    let a = Fixture::new().await;
    let b = a.sibling(CardRole::Assistant).await;
    for (f, token) in [(&a, "source-authority"), (&b, "target-authority")] {
        sqlx::query("UPDATE worker_sessions SET mcp_token_hash=?1 WHERE id=?2")
            .bind(token)
            .bind(&f.harness.inner.worker_session_id)
            .execute(a.repo.db().writer_pool())
            .await
            .unwrap();
        sqlx::query("INSERT INTO card_mcp_tokens(card_id,hashed_token,created_at) VALUES(?1,?2,0)")
            .bind(f.harness.inner.card_id.as_str())
            .bind(token)
            .execute(a.repo.db().writer_pool())
            .await
            .unwrap();
    }
    let (runtime, _) = runtime(&a, &b);
    let request = request(&a, &b).await;
    let (id, hook, drive) =
        super::handoff_coordinator_tests::start_at(&runtime, &request, "BeforeCommit").await;
    entered(&hook).await;
    archive_target(&b).await;
    let before = authority_image(&a).await;
    let mut sessions = vec![];
    for f in [&a, &b] {
        sessions.push(
            a.repo
                .session_get_by_id(&f.harness.inner.worker_session_id.as_str().into())
                .await
                .unwrap()
                .unwrap(),
        );
    }
    hook.release.notify_one();
    drive.await.unwrap();
    assert!(matches!(
        coordinator::query(&runtime, &request).await.unwrap(),
        CoordinatorResult::CancelledOrRefused
    ));
    assert_eq!(active(&a).await, 0);
    assert_eq!(
        super::handoff_coordinator_tests::progress(&a, &id).await["state"],
        "finished"
    );
    assert_eq!(authority_image(&a).await, before);
    for (f, mut expected) in [&a, &b].into_iter().zip(sessions) {
        let actual = a
            .repo
            .session_get_by_id(&f.harness.inner.worker_session_id.as_str().into())
            .await
            .unwrap()
            .unwrap();
        // Compensation only releases the pause; every session authority field stays intact.
        expected.handle_state_json = actual.handle_state_json.clone();
        expected.updated_at_ms = actual.updated_at_ms;
        assert_eq!(actual, expected);
        assert!(matches!(
            snapshot(f).await.handoff_pause,
            PauseState::Released {
                outcome: crate::harness::handoff_pause::ReleaseOutcome::HealthyReleased,
                ..
            }
        ));
    }
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM worker_sessions")
        .fetch_one(a.repo.pool())
        .await
        .unwrap();
    assert_eq!(count, 2);
    assert!(
        crate::db::RepoRead::conversation_archived_at(
            b.repo.as_ref(),
            b.harness.inner.card_id.as_str()
        )
        .await
        .unwrap()
        .is_some()
    );
    assert_eq!(a.daemon.started_thread_params_for_test().len(), 0);
}

#[tokio::test]
async fn handoff_archived_committed_target_cannot_activate_on_recovery() {
    let a = Fixture::new().await;
    let b = a.sibling(CardRole::Assistant).await;
    let (runtime, registry) = runtime(&a, &b);
    let request = request(&a, &b).await;
    let (id, hook, drive) =
        super::handoff_coordinator_tests::start_at(&runtime, &request, "Committed").await;
    entered(&hook).await;
    // Reproduce archive metadata retained by a cutover from before the archive guard.
    // The current archive REST API correctly refuses Planner cards.
    archive_target(&b).await;
    let CoordinatorResult::Forward { cutover, .. } =
        coordinator::query(&runtime, &request).await.unwrap()
    else {
        panic!("committed identity must remain forward-only");
    };
    let crate::operation::handoff_cutover::CutoverResult::CutoverCommittedPendingActivation {
        planner_session_id,
        ..
    } = *cutover;
    let before = a
        .repo
        .session_get_by_id(&planner_session_id.as_str().into())
        .await
        .unwrap()
        .unwrap();
    hook.release.notify_one();
    drive.await.unwrap();
    assert!(matches!(
        coordinator::query(&runtime, &request).await.unwrap(),
        CoordinatorResult::Forward {
            activation: ActivationResult::NeedsRecovery { .. },
            ..
        }
    ));
    assert_eq!(
        super::handoff_coordinator_tests::progress(&a, &id).await["resources"]["state"],
        "unprepared"
    );
    assert_eq!(
        a.repo
            .session_get_by_id(&planner_session_id.as_str().into())
            .await
            .unwrap()
            .unwrap(),
        before
    );
    assert!(registry.get(&planner_session_id).is_none());
    assert!(a.daemon.started_thread_params_for_test().is_empty());
    assert_eq!(
        active(&a).await,
        1,
        "recovery must retain the committed reservation"
    );
}
