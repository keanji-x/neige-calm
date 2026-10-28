use super::*;
use crate::operation::ProviderAdapter;
use calm_truth::db::RepoRead;

use crate::card_role_cache::CardRoleCache;

use calm_truth::db::RepoEventWrite;
use calm_truth::db::sqlite::{SqlxRepo, begin_immediate_tx, session_insert_tx};
use calm_truth_test_harness::FakeProvider;
use calm_types::worker::{LivenessTag, WorkerProviderKind};

use serde_json::json;

use crate::model::{Card, TaskStatus};
use crate::operation::workspace_lease::release_workspace_lease_for_card_tx;
use crate::operation::{OperationRepo, SqlxOperationRepo};

use crate::track_area_cache::TrackAreaCache;

use super::test_support::*;

#[derive(Clone, Copy)]
enum Change {
    LegacyRestart,
    AttemptReplaced,
    WrongCard,
    MissingBinding,
}

async fn stale_probe(change: Change) {
    let _guard = REAPER_TEST_LOCK.lock().await;
    reset_reaper_boot_gate_for_test();
    let (repo, track) = seeded_repo_with_plan(|path| {
        crate::db::sqlite::TrackWorkspacePlan::AttachedWithTrackWorktree(path.into())
    })
    .await;
    let task = insert_task(&repo, &track, "fence", TaskStatus::Running).await;
    let card = insert_session(&repo, session("old", track.clone(), 1)).await;
    if matches!(change, Change::LegacyRestart) {
        sqlx::query("UPDATE worker_sessions SET provider = 'claude', agent_session_id = 'legacy-agent' WHERE id = 'old'")
            .execute(repo.pool()).await.unwrap();
    }
    let op = insert_spawn_operation(&repo, Some(&task.id), Some(card.id.as_str())).await;
    sqlx::query("UPDATE worker_sessions SET spawn_op_id = ? WHERE id = 'old'")
        .bind(&op)
        .execute(repo.pool())
        .await
        .unwrap();
    sqlx::query("UPDATE tasks SET worker_card_id = ? WHERE id = ?")
        .bind(card.id.as_str())
        .bind(&task.id)
        .execute(repo.pool())
        .await
        .unwrap();
    let (mut lease, _) = acquire_test_workspace_lease(&repo, card.id.as_str(), &track, &op).await;
    if matches!(change, Change::MissingBinding) {
        sqlx::query("UPDATE tasks SET status = 'failed' WHERE id = ?")
            .bind(&task.id)
            .execute(repo.pool())
            .await
            .unwrap();
        sqlx::query("UPDATE worker_sessions SET spawn_op_id = NULL WHERE id = 'old'")
            .execute(repo.pool())
            .await
            .unwrap();
    }
    let entered = Arc::new(tokio::sync::Notify::new());
    let resume = Arc::new(tokio::sync::Notify::new());
    let mut reaper = Reaper::new(
        repo.clone(),
        registry_for(
            if matches!(change, Change::LegacyRestart) {
                WorkerProviderKind::Claude
            } else {
                WorkerProviderKind::Terminal
            },
            Arc::new(FakeProvider::new().with_probe_script([exited_liveness()])),
        ),
        EventBus::new(),
        write_context(&repo).await,
    );
    reaper.after_probe = Some((entered.clone(), resume.clone()));
    reaper_on_boot();
    let sweep = tokio::spawn(async move { reaper.sweep_all().await });
    entered.notified().await;
    match change {
        Change::LegacyRestart => legacy_restart(&repo, &card).await,
        Change::AttemptReplaced => {
            let mut tx = begin_immediate_tx(repo.db()).await.unwrap();
            sqlx::query("UPDATE worker_sessions SET state = 'exited' WHERE id = 'old'")
                .execute(&mut **tx)
                .await
                .unwrap();
            let mut replacement = session("replacement", track.clone(), 2);
            replacement.card_id = Some(card.id.clone());
            session_insert_tx(&mut tx, replacement).await.unwrap();
            sqlx::query("UPDATE cards SET session_id = 'replacement' WHERE id = ?")
                .bind(card.id.as_str())
                .execute(&mut **tx)
                .await
                .unwrap();
            tx.commit().await.unwrap();
            let new_op = replace_attempt(&repo, &task, &card).await;
            sqlx::query("UPDATE worker_sessions SET spawn_op_id = ? WHERE id = 'replacement'")
                .bind(&new_op)
                .execute(repo.pool())
                .await
                .unwrap();
            (lease, _) =
                acquire_test_workspace_lease(&repo, card.id.as_str(), &track, &new_op).await;
        }
        Change::WrongCard => {
            let replacement = insert_session(&repo, session("replacement", track.clone(), 2)).await;
            sqlx::query("UPDATE tasks SET worker_card_id = ? WHERE id = ?")
                .bind(replacement.id.as_str())
                .bind(&task.id)
                .execute(repo.pool())
                .await
                .unwrap();
        }
        Change::MissingBinding => {
            sqlx::query("UPDATE worker_sessions SET spawn_op_id = NULL WHERE id = 'old'")
                .execute(repo.pool())
                .await
                .unwrap();
            sqlx::query("UPDATE cards SET session_id = NULL WHERE id = ?")
                .bind(card.id.as_str())
                .execute(repo.pool())
                .await
                .unwrap();
        }
    }
    let historical_before = repo.task_get(&task.id).await.unwrap().unwrap();
    let before = repo.tasks_by_track(track.as_str()).await.unwrap();
    let events_before = RepoEventWrite::events_since(repo.as_ref(), 0, i64::MAX)
        .await
        .unwrap()
        .len();
    resume.notify_one();
    sweep.await.unwrap();
    assert_eq!(repo.tasks_by_track(track.as_str()).await.unwrap(), before);
    assert_eq!(
        repo.task_get(&task.id).await.unwrap().unwrap(),
        historical_before
    );
    let state: String = sqlx::query_scalar("SELECT state FROM workspace_leases WHERE lease_id = ?")
        .bind(&lease)
        .fetch_one(repo.pool())
        .await
        .unwrap();
    assert_eq!(state, "held");
    assert!(task_failed_events(&repo, &task.id).await.is_empty());
    assert_eq!(
        RepoEventWrite::events_since(repo.as_ref(), 0, i64::MAX)
            .await
            .unwrap()
            .len(),
        events_before
    );
    reset_reaper_boot_gate_for_test();
}

// Materialize a released recovery shape, as existing persisted attempts may carry it.
async fn replace_attempt(repo: &SqlxRepo, task: &crate::model::Task, card: &Card) -> String {
    use calm_types::task_recovery::{
        TASK_IN_TRACK_ROUTE, TaskAttemptOrigin, TaskRecoveryConstraint,
    };
    let mut tx = begin_immediate_tx(repo.db()).await.unwrap();
    sqlx::query("UPDATE tasks SET status = 'failed', finished_at_ms = ? WHERE id = ?")
        .bind(now_ms())
        .bind(&task.id)
        .execute(&mut **tx)
        .await
        .unwrap();
    release_workspace_lease_for_card_tx(
        &mut tx,
        card.id.as_str(),
        ReleaseDelivery::CommitAsTaskEnded,
    )
    .await
    .unwrap();
    let mut replacement = task.clone();
    replacement.id = format!("{}:replacement", task.id);
    replacement.worker_card_id = Some(card.id.to_string());
    let origin = TaskAttemptOrigin::Recovery {
        previous_attempt_id: task.id.clone(),
        idempotency_key: "retry".into(),
        request_fingerprint: "retry".into(),
        reason: "retry".into(),
        actor: ActorId::Kernel,
        constraint: TaskRecoveryConstraint::V1 {
            refs: vec![calm_types::event::TaskContextRef {
                track_id: task.track_id.clone().into(),
                block_id: "root".into(),
                rev: 0,
                hash: "a".repeat(64),
                is_root: true,
            }],
            spawn: TASK_IN_TRACK_ROUTE.into(),
            declared_by: "user".into(),
        },
    };
    sqlx::query("INSERT INTO task_attempt_allocations(attempt_id,track_id,key,generation,origin_json,created_at_ms) VALUES(?,?,?,2,?,?)")
        .bind(&replacement.id).bind(&task.track_id).bind(&task.key)
        .bind(serde_json::to_string(&origin).unwrap()).bind(now_ms()).execute(&mut **tx).await.unwrap();
    crate::test_support::insert_task_tx(&mut tx, &replacement)
        .await
        .unwrap();
    // Preserve the stale live row a delayed writer can still see after allocation replacement,
    // as in the terminal-worker supersession regression. Current-session fencing must dominate it.
    sqlx::query("UPDATE tasks SET status = 'running' WHERE id = ?")
        .bind(&task.id)
        .execute(&mut **tx)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    insert_spawn_operation(repo, Some(&replacement.id), Some(card.id.as_str())).await
}

async fn legacy_restart(repo: &Arc<SqlxRepo>, card: &Card) {
    let settings = tempfile::tempdir().unwrap();
    sqlx::query("UPDATE cards SET kind = 'claude', payload = ? WHERE id = ?")
        .bind(json!({"settings_path": settings.path().join("settings.json"), "session_id": "legacy-agent"}).to_string())
        .bind(card.id.as_str()).execute(repo.pool()).await.unwrap();
    let mut tx = begin_immediate_tx(repo.db()).await.unwrap();
    crate::db::sqlite::session_complete_tx(&mut tx, &"old".to_owned(), WorkerSessionState::Exited)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    let adapter = crate::operation::claude_restart_adapter::ClaudeRestartAdapter::new(
        repo.clone(),
        Arc::new(crate::state::CodexClient::new_stub()),
        None,
        CardRoleCache::new(),
        TrackAreaCache::new(),
    );
    let operations = SqlxOperationRepo::new(repo.db().clone());
    let id = insert_spawn_operation(repo, None, None).await;
    let op = operations.get_operation(&id).await.unwrap().unwrap();
    let mut tx = begin_immediate_tx(repo.db()).await.unwrap();
    let payload = crate::operation::claude_restart_adapter::ClaudeRestartOperationPayload {
        actor: ActorId::KernelDispatcher,
        worker_session_id: Some("restarted".into()),
        card_id: card.id.to_string(),
    };
    adapter
        .prepare_tx(&mut tx, &serde_json::to_value(payload).unwrap(), &op)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    let current: String = sqlx::query_scalar("SELECT session_id FROM cards WHERE id = ?")
        .bind(card.id.as_str())
        .fetch_one(repo.pool())
        .await
        .unwrap();
    assert_eq!(current, "restarted");
}

#[tokio::test]
async fn w_reaper_stale_probe_after_legacy_restart_is_noop() {
    stale_probe(Change::LegacyRestart).await;
}
#[tokio::test]
async fn w_reaper_stale_probe_after_attempt_replaced_spares_new_attempt() {
    stale_probe(Change::AttemptReplaced).await;
}
#[tokio::test]
async fn w_reaper_task_fail_requires_card_binding() {
    stale_probe(Change::WrongCard).await;
}
#[tokio::test]
async fn w_reaper_stale_binding_missing_releases_nothing() {
    stale_probe(Change::MissingBinding).await;
}

#[tokio::test]
async fn w_reaper_nonterminal_task_race_releases_nothing() {
    let _guard = REAPER_TEST_LOCK.lock().await;
    for status in [TaskStatus::Pending, TaskStatus::Verifying] {
        reset_reaper_boot_gate_for_test();
        let (repo, track) = seeded_repo_with_plan(|path| {
            crate::db::sqlite::TrackWorkspacePlan::AttachedWithTrackWorktree(path.into())
        })
        .await;
        let task = insert_task(&repo, &track, "nonterminal", status).await;
        let card = insert_session(&repo, session("current", track.clone(), 1)).await;
        let op = insert_spawn_operation(&repo, Some(&task.id), Some(card.id.as_str())).await;
        sqlx::query("UPDATE worker_sessions SET spawn_op_id = ? WHERE id = 'current'")
            .bind(&op)
            .execute(repo.pool())
            .await
            .unwrap();
        sqlx::query("UPDATE tasks SET worker_card_id = ? WHERE id = ?")
            .bind(card.id.as_str())
            .bind(&task.id)
            .execute(repo.pool())
            .await
            .unwrap();
        // Legacy leases have no delivery guard to mask an unauthorized release.
        let lease_path = tempfile::tempdir().unwrap();
        let mut tx = begin_immediate_tx(repo.db()).await.unwrap();
        let (lease, _) = crate::operation::workspace_lease::acquire_plain_workspace_lease_tx(
            &mut tx,
            card.id.as_str(),
            track.as_str(),
            &op,
            lease_path.path(),
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();
        let lease = lease.lease_id;
        let before = repo.task_get(&task.id).await.unwrap();
        let events_before = RepoEventWrite::events_since(repo.as_ref(), 0, i64::MAX)
            .await
            .unwrap();
        let reaper = Reaper::new(
            repo.clone(),
            registry(Arc::new(
                FakeProvider::new().with_probe_script([exited_liveness()]),
            )),
            EventBus::new(),
            write_context(&repo).await,
        );
        reaper_on_boot();
        reaper.sweep_all().await;
        assert_eq!(repo.task_get(&task.id).await.unwrap(), before);
        let state: String =
            sqlx::query_scalar("SELECT state FROM workspace_leases WHERE lease_id = ?")
                .bind(&lease)
                .fetch_one(repo.pool())
                .await
                .unwrap();
        assert_eq!(state, "held", "nonterminal task {status:?}");
        assert_eq!(
            RepoEventWrite::events_since(repo.as_ref(), 0, i64::MAX)
                .await
                .unwrap()
                .len(),
            events_before.len()
        );
    }
    reset_reaper_boot_gate_for_test();
}

/// A legacy Claude restart's current session carries no spawn op. When it dies while its attempt
/// is still Running, the reaper retires the session and leaves the attempt and its lease to the
/// scheduler's timeout; when the attempt already ended, the reaper releases its lease.
async fn restarted_session_death(access: crate::model::TaskAccess, attempt_ended: bool) {
    let _guard = REAPER_TEST_LOCK.lock().await;
    reset_reaper_boot_gate_for_test();
    let (repo, track) = seeded_repo_with_plan(|path| {
        crate::db::sqlite::TrackWorkspacePlan::AttachedWithTrackWorktree(path.into())
    })
    .await;
    let task = insert_task(&repo, &track, "restarted", TaskStatus::Running).await;
    let card = insert_session(&repo, session("old", track.clone(), 1)).await;
    sqlx::query("UPDATE worker_sessions SET provider = 'claude', agent_session_id = 'legacy-agent' WHERE id = 'old'")
        .execute(repo.pool()).await.unwrap();
    let op = insert_spawn_operation(&repo, Some(&task.id), Some(card.id.as_str())).await;
    sqlx::query("UPDATE worker_sessions SET spawn_op_id = ? WHERE id = 'old'")
        .bind(&op)
        .execute(repo.pool())
        .await
        .unwrap();
    sqlx::query("UPDATE tasks SET worker_card_id = ? WHERE id = ?")
        .bind(card.id.as_str())
        .bind(&task.id)
        .execute(repo.pool())
        .await
        .unwrap();
    let (lease, _) =
        acquire_test_workspace_lease_as(&repo, card.id.as_str(), &track, &op, access).await;
    legacy_restart(&repo, &card).await;
    // The replacement spawn came up; its row carries no spawn op.
    let spawned = sqlx::query("UPDATE worker_sessions SET state = 'running' WHERE id = 'restarted' AND spawn_op_id IS NULL")
        .execute(repo.pool())
        .await
        .unwrap();
    assert_eq!(spawned.rows_affected(), 1);
    if attempt_ended {
        sqlx::query("UPDATE tasks SET status = 'failed' WHERE id = ?")
            .bind(&task.id)
            .execute(repo.pool())
            .await
            .unwrap();
    }
    let task_before = repo.task_get(&task.id).await.unwrap().unwrap();
    let reaper = Reaper::new(
        repo.clone(),
        registry_for(
            WorkerProviderKind::Claude,
            Arc::new(FakeProvider::new().with_probe_script([exited_liveness()])),
        ),
        EventBus::new(),
        write_context(&repo).await,
    );
    reaper_on_boot();
    reaper.sweep_all().await;
    let restarted = repo
        .session_get(&WorkerSessionId::from("restarted"))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(restarted.state, WorkerSessionState::Failed);
    assert_eq!(repo.task_get(&task.id).await.unwrap().unwrap(), task_before);
    let state: String = sqlx::query_scalar("SELECT state FROM workspace_leases WHERE lease_id = ?")
        .bind(&lease)
        .fetch_one(repo.pool())
        .await
        .unwrap();
    if attempt_ended {
        assert_eq!(task_before.status, TaskStatus::Failed);
        assert_eq!(
            state, "released",
            "an ended attempt's {access:?} lease is released"
        );
    } else {
        assert_eq!(task_before.status, TaskStatus::Running);
        assert_eq!(
            state, "held",
            "a Running attempt keeps its {access:?} lease"
        );
    }
    assert!(task_failed_events(&repo, &task.id).await.is_empty());
    reset_reaper_boot_gate_for_test();
}

#[tokio::test]
async fn w_reaper_restarted_session_death_while_running_converges() {
    restarted_session_death(crate::model::TaskAccess::ReadWrite, false).await;
}

#[tokio::test]
async fn w_reaper_restarted_session_death_spares_running_read_only_lease() {
    restarted_session_death(crate::model::TaskAccess::ReadOnly, false).await;
}

#[tokio::test]
async fn w_reaper_restarted_session_death_after_attempt_ended_releases_lease() {
    restarted_session_death(crate::model::TaskAccess::ReadWrite, true).await;
}

#[tokio::test]
async fn w_reaper_stale_nonterminal_session_records_liveness() {
    let _guard = REAPER_TEST_LOCK.lock().await;
    reset_reaper_boot_gate_for_test();
    let (repo, track) = seeded_repo().await;
    let card = insert_session(&repo, session("stale", track.clone(), 1)).await;
    // The card no longer names this still-active session as its current one.
    sqlx::query("UPDATE cards SET session_id = NULL WHERE id = ?")
        .bind(card.id.as_str())
        .execute(repo.pool())
        .await
        .unwrap();
    let reaper = Reaper::new(
        repo.clone(),
        registry(Arc::new(
            FakeProvider::new().with_probe_script([exited_liveness()]),
        )),
        EventBus::new(),
        write_context(&repo).await,
    );
    reaper_on_boot();
    reaper.sweep_all().await;
    let stale = repo
        .session_get(&WorkerSessionId::from("stale"))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(stale.state, WorkerSessionState::Running);
    assert_eq!(stale.liveness, LivenessTag::Exited);
    assert!(stale.liveness_probed_at_ms.is_some());
    reset_reaper_boot_gate_for_test();
}
