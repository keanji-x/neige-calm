use super::*;
use calm_truth::db::RepoRead;

use calm_truth::db::RepoEventWrite;
use calm_truth::db::sqlite::status_detail_class;
use calm_truth::session_repo::SessionRepo;
use calm_truth_test_harness::FakeProvider;
use calm_types::ids::CardId;
use calm_types::worker::{
    LivenessTag, SessionMode, WorkerProviderKind, WorkerSessionId, WorkerSessionState,
};

use crate::model::TaskStatus;

use super::test_support::*;
#[tokio::test]
async fn sweep_records_non_exit_liveness_and_terminals_exited_without_spawn_op() {
    let _guard = REAPER_TEST_LOCK.lock().await;
    reset_reaper_boot_gate_for_test();

    let (repo, track_id) = seeded_repo().await;
    for (idx, id) in ["ws-alive", "ws-idle", "ws-unknown", "ws-exited"]
        .into_iter()
        .enumerate()
    {
        insert_session(&repo, session(id, track_id.clone(), idx as i64 + 1)).await;
    }

    let fake = Arc::new(FakeProvider::new().with_probe_script([
        Liveness::Alive {
            active_turn_id: Some("turn-1".into()),
        },
        Liveness::Idle,
        Liveness::Unknown { since_ms: 99 },
        exited_liveness(),
    ]));
    let repo_dyn: Arc<dyn Repo> = repo.clone();
    let reaper = Reaper::new(
        repo_dyn,
        registry(fake.clone()),
        EventBus::new(),
        write_context(&repo).await,
    );
    let before_events = RepoEventWrite::events_since(repo.as_ref(), 0, i64::MAX)
        .await
        .expect("events before");

    reaper_on_boot();
    reaper.sweep_all().await;

    assert_eq!(fake.probe_call_count(), 4);
    assert_eq!(
        RepoEventWrite::events_since(repo.as_ref(), 0, i64::MAX)
            .await
            .expect("events after")
            .len(),
        before_events.len(),
        "exited reaper session without spawn_op_id must not emit task events"
    );

    for (id, tag) in [
        ("ws-alive", LivenessTag::Alive),
        ("ws-idle", LivenessTag::Idle),
        ("ws-unknown", LivenessTag::Unknown),
    ] {
        let row = repo
            .session_get(&WorkerSessionId::from(id))
            .await
            .expect("session get")
            .expect("session exists");
        assert_eq!(row.liveness, tag, "{id} liveness tag");
        assert!(
            row.liveness_probed_at_ms.is_some(),
            "{id} liveness_probed_at_ms"
        );
        assert_eq!(
            row.state,
            WorkerSessionState::Running,
            "{id} state must not transition"
        );
        assert_eq!(row.exit_code, None, "{id} exit_code untouched");
        assert_eq!(
            row.exit_interpretation, None,
            "{id} exit_interpretation untouched"
        );
    }

    let exited = repo
        .session_get(&WorkerSessionId::from("ws-exited"))
        .await
        .expect("session get")
        .expect("session exists");
    assert_eq!(exited.liveness, LivenessTag::Exited);
    assert!(exited.liveness_probed_at_ms.is_some());
    assert_eq!(exited.state, WorkerSessionState::Failed);
    assert_eq!(exited.exit_code, Some(7));
    assert_eq!(exited.exit_interpretation.as_deref(), Some("failed"));
    assert!(exited.completed_at_ms.is_some());

    reset_reaper_boot_gate_for_test();
}

#[tokio::test]
async fn sweep_exited_failed_converges_dead_worker_task() {
    let _guard = REAPER_TEST_LOCK.lock().await;
    reset_reaper_boot_gate_for_test();

    let (repo, track_id) = seeded_repo().await;
    let task = insert_task(&repo, &track_id, "dead-worker", TaskStatus::Running).await;
    let card = insert_session(&repo, session("ws-dead-worker", track_id.clone(), 1)).await;
    // §3.7: a first report may precede the task's card stamp, with an op-target proof.
    let op_id = insert_spawn_operation(&repo, Some(&task.id), Some(card.id.as_str())).await;
    sqlx::query("UPDATE worker_sessions SET spawn_op_id = ? WHERE id = 'ws-dead-worker'")
        .bind(op_id)
        .execute(repo.pool())
        .await
        .unwrap();

    let fake = Arc::new(FakeProvider::new().with_probe_script([exited_liveness()]));
    let events = EventBus::new();
    let repo_dyn: Arc<dyn Repo> = repo.clone();
    let reaper = Reaper::new(
        repo_dyn,
        registry(fake.clone()),
        events,
        write_context(&repo).await,
    );

    reaper_on_boot();
    reaper.sweep_all().await;

    assert_eq!(fake.probe_call_count(), 1);
    let worker = repo
        .session_get(&WorkerSessionId::from("ws-dead-worker"))
        .await
        .expect("session get")
        .expect("session exists");
    assert_eq!(worker.state, WorkerSessionState::Failed);
    assert_eq!(worker.liveness, LivenessTag::Exited);
    assert_eq!(worker.exit_code, Some(7));
    assert_eq!(worker.exit_interpretation.as_deref(), Some("failed"));

    let task_row = repo
        .task_get(&task.id)
        .await
        .expect("task get")
        .expect("task exists");
    assert_eq!(task_row.status, TaskStatus::Failed);
    // The `spawn-failed` classifier is knowingly wrong for a runtime death; only the reason tail is asserted.
    let detail = task_row.status_detail.clone().unwrap_or_default();
    assert_eq!(status_detail_class(&detail), "spawn-failed");
    assert!(
        detail.contains("outcome unknown") && detail.contains("supervisor probe"),
        "status_detail must carry the reaper reason, got {detail:?}"
    );

    let failed = task_failed_events(&repo, &task.id).await;
    assert_eq!(failed.len(), 1);
    match &failed[0] {
        Event::TaskFailed {
            idempotency_key,
            reason,
            agent_message,
            ..
        } => {
            assert_eq!(idempotency_key, &task.id);
            // The probe-sourced evidence hides the exit sentinel behind 'outcome unknown'.
            assert!(
                reason.contains("outcome unknown") && reason.contains("supervisor probe"),
                "expected provider reason, got {reason:?}"
            );
            assert!(!reason.contains("exit Some("));
            assert_eq!(agent_message, &None);
        }
        other => panic!("expected task.failed, got {other:?}"),
    }

    reset_reaper_boot_gate_for_test();
}

/// A stale `last_activity_ms` lets the pre-gate through; a `Dead` verdict must converge like the ephemeral path.
#[tokio::test]
async fn sweep_resumable_codex_exited_arbiter_dead_converges() {
    let _guard = REAPER_TEST_LOCK.lock().await;
    reset_reaper_boot_gate_for_test();

    let (repo, track_id) = seeded_repo().await;
    let task = insert_task(&repo, &track_id, "codex-dead", TaskStatus::Running).await;
    let op_id = insert_spawn_operation(&repo, Some(&task.id), None).await;
    // `created_at_ms = 1` (NULL last_activity ⇒ created_at) is far past the deadline, so the pre-gate does not short-circuit.
    let mut worker = session("ws-codex-dead", track_id.clone(), 1);
    worker.provider = WorkerProviderKind::Codex;
    worker.mode = SessionMode::Resumable;
    worker.thread_id = Some("t-codex-dead".into());
    worker.spawn_op_id = Some(op_id);
    let card = insert_session(&repo, worker).await;
    sqlx::query("UPDATE tasks SET worker_card_id = ? WHERE id = ?")
        .bind(card.id.as_str())
        .bind(&task.id)
        .execute(repo.pool())
        .await
        .unwrap();

    let fake = Arc::new(
        FakeProvider::new()
            .with_session_mode(SessionMode::Resumable)
            .with_death_verdict(DeathVerdict::Dead)
            .with_probe_script([exited_liveness()]),
    );
    let repo_dyn: Arc<dyn Repo> = repo.clone();
    let reaper = Reaper::new(
        repo_dyn,
        registry_for(WorkerProviderKind::Codex, fake.clone()),
        EventBus::new(),
        write_context(&repo).await,
    );

    reaper_on_boot();
    reaper.sweep_all().await;

    assert_eq!(fake.probe_call_count(), 1);
    assert_eq!(
        fake.death_verdict_call_count(),
        1,
        "arbiter must be consulted for a stale resumable Exited"
    );

    let worker = repo
        .session_get(&WorkerSessionId::from("ws-codex-dead"))
        .await
        .expect("session get")
        .expect("session exists");
    assert_eq!(worker.state, WorkerSessionState::Failed);
    assert_eq!(worker.liveness, LivenessTag::Exited);
    assert_eq!(worker.exit_code, Some(7));
    assert_eq!(worker.exit_interpretation.as_deref(), Some("failed"));
    assert!(worker.completed_at_ms.is_some());

    let task_row = repo
        .task_get(&task.id)
        .await
        .expect("task get")
        .expect("task exists");
    assert_eq!(task_row.status, TaskStatus::Failed);
    let detail = task_row.status_detail.clone().unwrap_or_default();
    assert_eq!(status_detail_class(&detail), "spawn-failed");
    let event_reason = match &task_failed_events(&repo, &task.id).await[0] {
        Event::TaskFailed { reason, .. } => reason.clone(),
        other => panic!("expected task.failed, got {other:?}"),
    };
    assert!(
        detail.ends_with(&event_reason),
        "row detail {detail:?} must carry the event reason {event_reason:?}"
    );

    let failed = task_failed_events(&repo, &task.id).await;
    assert_eq!(failed.len(), 1);

    reset_reaper_boot_gate_for_test();
}

#[tokio::test]
async fn sweep_resumable_codex_dead_worker_releases_same_boot_workspace_lease() {
    let _guard = REAPER_TEST_LOCK.lock().await;
    reset_reaper_boot_gate_for_test();

    let (repo, track_id) = seeded_repo_with_plan(|path| {
        crate::db::sqlite::TrackWorkspacePlan::AttachedWithTrackWorktree(path.into())
    })
    .await;
    let task = insert_task(&repo, &track_id, "codex-lease-dead", TaskStatus::Running).await;
    let op_id = insert_spawn_operation(&repo, Some(&task.id), None).await;
    let mut worker = session("ws-codex-lease-dead", track_id.clone(), 1);
    worker.provider = WorkerProviderKind::Codex;
    worker.mode = SessionMode::Resumable;
    worker.thread_id = Some("t-codex-lease-dead".into());
    worker.spawn_op_id = Some(op_id.clone());
    let card = insert_session(&repo, worker).await;
    sqlx::query("UPDATE tasks SET worker_card_id = ? WHERE id = ?")
        .bind(card.id.as_str())
        .bind(&task.id)
        .execute(repo.pool())
        .await
        .unwrap();
    let (lease_id, lease_path) =
        acquire_test_workspace_lease(&repo, card.id.as_str(), &track_id, &op_id).await;
    assert!(
        std::path::Path::new(&lease_path).is_dir(),
        "leased cwd exists before reaping"
    );

    let fake = Arc::new(
        FakeProvider::new()
            .with_session_mode(SessionMode::Resumable)
            .with_death_verdict(DeathVerdict::Dead)
            .with_probe_script([exited_liveness()]),
    );
    let repo_dyn: Arc<dyn Repo> = repo.clone();
    let reaper = Reaper::new(
        repo_dyn,
        registry_for(WorkerProviderKind::Codex, fake),
        EventBus::new(),
        write_context(&repo).await,
    );

    reaper_on_boot();
    reaper.sweep_all().await;

    let state: String =
        sqlx::query_scalar("SELECT state FROM workspace_leases WHERE lease_id = ?1")
            .bind(&lease_id)
            .fetch_one(repo.pool())
            .await
            .expect("lease state");
    assert_eq!(state, "released");
    assert!(
        std::path::Path::new(&lease_path).is_dir(),
        "reaper release preserves leased cwd"
    );
    assert_eq!(
        git_stdout(
            std::path::Path::new(&lease_path),
            ["rev-parse", "--abbrev-ref", "HEAD"]
        )
        .trim(),
        format!("neige/track-{}", track_id.as_str()),
        "reaper release preserves the track branch"
    );
    // #1830 S2 D7: the reaper's fail transaction commits the attempt as `failed`.
    let outcome: String = sqlx::query_scalar(
        "SELECT outcome FROM task_git_deliveries WHERE producer_attempt_id = ?1",
    )
    .bind(&task.id)
    .fetch_one(repo.pool())
    .await
    .expect("the dead attempt's delivery row");
    assert_eq!(outcome, "failed");
    let released_events: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM events WHERE kind = 'workspace.released'")
            .fetch_one(repo.pool())
            .await
            .expect("released event count");
    assert_eq!(released_events, 1);
    let removed_events: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM events WHERE kind = 'worktree.removed'")
            .fetch_one(repo.pool())
            .await
            .expect("removed event count");
    assert_eq!(removed_events, 0);

    reset_reaper_boot_gate_for_test();
}

#[tokio::test]
async fn converge_dead_worker_without_spawn_op_releases_workspace_lease() {
    let _guard = REAPER_TEST_LOCK.lock().await;

    let (repo, track_id) = seeded_repo_with_plan(|path| {
        crate::db::sqlite::TrackWorkspacePlan::AttachedWithTrackWorktree(path.into())
    })
    .await;
    let mut worker = session("ws-codex-no-spawn-op", track_id.clone(), 1);
    worker.provider = WorkerProviderKind::Codex;
    worker.mode = SessionMode::Resumable;
    worker.thread_id = Some("t-codex-no-spawn-op".into());
    let card = insert_session(&repo, worker.clone()).await;
    worker.card_id = Some(CardId(card.id.to_string()));
    let (lease_id, lease_path) =
        acquire_test_workspace_lease(&repo, card.id.as_str(), &track_id, "missing-spawn-op").await;
    assert!(
        std::path::Path::new(&lease_path).is_dir(),
        "leased cwd exists before converge guard"
    );

    let events = EventBus::new();
    let write = write_context(&repo).await;
    let repo_dyn: Arc<dyn Repo> = repo.clone();
    converge_dead_worker(repo_dyn.as_ref(), &events, &write, &worker, "dead")
        .await
        .expect("converge dead worker");

    let state: String =
        sqlx::query_scalar("SELECT state FROM workspace_leases WHERE lease_id = ?1")
            .bind(&lease_id)
            .fetch_one(repo.pool())
            .await
            .expect("lease state");
    assert_eq!(state, "released");
    assert!(
        std::path::Path::new(&lease_path).is_dir(),
        "spawn_op_id guard release preserves leased cwd"
    );
    assert_eq!(
        git_stdout(
            std::path::Path::new(&lease_path),
            ["rev-parse", "--abbrev-ref", "HEAD"]
        )
        .trim(),
        format!("neige/track-{}", track_id.as_str()),
        "spawn_op_id guard release preserves the track branch"
    );
    let released_events: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM events WHERE kind = 'workspace.released'")
            .fetch_one(repo.pool())
            .await
            .expect("released event count");
    assert_eq!(released_events, 1);
    let removed_events: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM events WHERE kind = 'worktree.removed'")
            .fetch_one(repo.pool())
            .await
            .expect("removed event count");
    assert_eq!(removed_events, 0);
}
