use super::*;
use calm_truth::db::RepoRead;

use calm_truth::db::RepoEventWrite;

use calm_truth::session_repo::SessionRepo;
use calm_truth_test_harness::FakeProvider;

use calm_types::worker::{
    LivenessTag, SessionMode, WorkerProviderKind, WorkerSessionId, WorkerSessionState,
};

use crate::model::TaskStatus;

use super::test_support::*;
#[tokio::test]
async fn sweep_resumable_codex_exited_arbiter_alive_records_t2_only() {
    let _guard = REAPER_TEST_LOCK.lock().await;
    reset_reaper_boot_gate_for_test();

    let (repo, track_id) = seeded_repo().await;
    let task = insert_task(&repo, &track_id, "codex-alive", TaskStatus::Running).await;
    let op_id = insert_spawn_operation(&repo, Some(&task.id), None).await;
    let mut worker = session("ws-codex-alive", track_id.clone(), 1);
    worker.provider = WorkerProviderKind::Codex;
    worker.mode = SessionMode::Resumable;
    worker.thread_id = Some("t-codex-alive".into());
    worker.spawn_op_id = Some(op_id);
    insert_session(&repo, worker).await;

    let fake = Arc::new(
        FakeProvider::new()
            .with_session_mode(SessionMode::Resumable)
            .with_death_verdict(DeathVerdict::Alive)
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
    assert_eq!(fake.death_verdict_call_count(), 1);

    let worker = repo
        .session_get(&WorkerSessionId::from("ws-codex-alive"))
        .await
        .expect("session get")
        .expect("session exists");
    assert_eq!(worker.liveness, LivenessTag::Exited);
    assert!(worker.liveness_probed_at_ms.is_some());
    assert_eq!(
        worker.state,
        WorkerSessionState::Running,
        "arbiter Alive must NOT terminalize the session"
    );
    assert_eq!(worker.exit_code, None);
    assert_eq!(worker.exit_interpretation, None);
    assert!(worker.completed_at_ms.is_none());

    assert_eq!(task_failed_events(&repo, &task.id).await.len(), 0);
    let task_row = repo
        .task_get(&task.id)
        .await
        .expect("task get")
        .expect("task exists");
    assert_eq!(task_row.status, TaskStatus::Running);

    reset_reaper_boot_gate_for_test();
}

#[tokio::test]
async fn sweep_resumable_codex_exited_arbiter_unknown_records_t2_only() {
    let _guard = REAPER_TEST_LOCK.lock().await;
    reset_reaper_boot_gate_for_test();

    let (repo, track_id) = seeded_repo().await;
    let task = insert_task(&repo, &track_id, "codex-unknown", TaskStatus::Running).await;
    let op_id = insert_spawn_operation(&repo, Some(&task.id), None).await;
    let mut worker = session("ws-codex-unknown", track_id.clone(), 1);
    worker.provider = WorkerProviderKind::Codex;
    worker.mode = SessionMode::Resumable;
    worker.thread_id = Some("t-codex-unknown".into());
    worker.spawn_op_id = Some(op_id);
    insert_session(&repo, worker).await;

    let fake = Arc::new(
        FakeProvider::new()
            .with_session_mode(SessionMode::Resumable)
            .with_death_verdict(DeathVerdict::Unknown)
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
    assert_eq!(fake.death_verdict_call_count(), 1);

    let worker = repo
        .session_get(&WorkerSessionId::from("ws-codex-unknown"))
        .await
        .expect("session get")
        .expect("session exists");
    assert_eq!(worker.liveness, LivenessTag::Exited);
    assert!(worker.liveness_probed_at_ms.is_some());
    assert_eq!(
        worker.state,
        WorkerSessionState::Running,
        "arbiter Unknown must NOT terminalize the session"
    );
    assert_eq!(worker.exit_code, None);
    assert_eq!(worker.exit_interpretation, None);
    assert!(worker.completed_at_ms.is_none());

    assert_eq!(task_failed_events(&repo, &task.id).await.len(), 0);

    reset_reaper_boot_gate_for_test();
}

/// Arbiter would say `Dead`, but the pre-gate never asks it.
#[tokio::test]
async fn sweep_resumable_codex_exited_recent_activity_pregate_skips_arbiter() {
    let _guard = REAPER_TEST_LOCK.lock().await;
    reset_reaper_boot_gate_for_test();

    let (repo, track_id) = seeded_repo().await;
    let task = insert_task(&repo, &track_id, "codex-recent", TaskStatus::Running).await;
    let op_id = insert_spawn_operation(&repo, Some(&task.id), None).await;
    let mut worker = session("ws-codex-recent", track_id.clone(), 1);
    worker.provider = WorkerProviderKind::Codex;
    worker.mode = SessionMode::Resumable;
    worker.thread_id = Some("t-codex-recent".into());
    worker.last_activity_ms = Some(now_ms());
    worker.spawn_op_id = Some(op_id);
    insert_session(&repo, worker).await;

    let fake = Arc::new(
        FakeProvider::new()
            .with_session_mode(SessionMode::Resumable)
            // Arbiter WOULD reap, proving the pre-gate is what holds it.
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
        0,
        "recent-activity pre-gate must short-circuit WITHOUT consulting the arbiter"
    );

    let worker = repo
        .session_get(&WorkerSessionId::from("ws-codex-recent"))
        .await
        .expect("session get")
        .expect("session exists");
    assert_eq!(worker.liveness, LivenessTag::Exited);
    assert!(worker.liveness_probed_at_ms.is_some());
    assert_eq!(
        worker.state,
        WorkerSessionState::Running,
        "recent-activity pre-gate must NOT terminalize the session"
    );
    assert_eq!(worker.exit_code, None);
    assert_eq!(worker.exit_interpretation, None);

    assert_eq!(task_failed_events(&repo, &task.id).await.len(), 0);

    reset_reaper_boot_gate_for_test();
}

/// A supervisor `proc_running:false` in the spawn window means 'not registered YET', not 'exited': liveness is recorded and the spawn operation owns convergence.
#[tokio::test]
async fn sweep_exited_starting_session_records_liveness_without_convergence() {
    let _guard = REAPER_TEST_LOCK.lock().await;
    reset_reaper_boot_gate_for_test();

    let (repo, track_id) = seeded_repo().await;
    let task = insert_task(&repo, &track_id, "spawn-window", TaskStatus::Running).await;
    let op_id = insert_spawn_operation(&repo, Some(&task.id), None).await;
    let mut worker = session("ws-starting", track_id.clone(), 1);
    worker.state = WorkerSessionState::Starting;
    worker.spawn_op_id = Some(op_id);
    insert_session(&repo, worker).await;

    let fake = Arc::new(FakeProvider::new().with_probe_script([exited_liveness()]));
    let repo_dyn: Arc<dyn Repo> = repo.clone();
    let reaper = Reaper::new(
        repo_dyn,
        registry(fake.clone()),
        EventBus::new(),
        write_context(&repo).await,
    );

    reaper_on_boot();
    reaper.sweep_all().await;

    assert_eq!(fake.probe_call_count(), 1);
    let worker = repo
        .session_get(&WorkerSessionId::from("ws-starting"))
        .await
        .expect("session get")
        .expect("session exists");
    assert_eq!(worker.liveness, LivenessTag::Exited);
    assert!(worker.liveness_probed_at_ms.is_some());
    assert_eq!(
        worker.state,
        WorkerSessionState::Starting,
        "spawn-window Exited must NOT terminalize a `starting` session"
    );
    assert_eq!(worker.exit_code, None, "no exit committed in spawn window");
    assert_eq!(worker.exit_interpretation, None);
    assert!(worker.completed_at_ms.is_none());

    assert_eq!(task_failed_events(&repo, &task.id).await.len(), 0);
    let task_row = repo
        .task_get(&task.id)
        .await
        .expect("task get")
        .expect("task exists");
    assert_eq!(task_row.status, TaskStatus::Running);

    reset_reaper_boot_gate_for_test();
}

#[tokio::test]
async fn sweep_exited_with_null_spawn_op_task_key_terminalizes_without_task_failed() {
    let _guard = REAPER_TEST_LOCK.lock().await;
    reset_reaper_boot_gate_for_test();

    let (repo, track_id) = seeded_repo().await;
    let task = insert_task(&repo, &track_id, "null-op-key", TaskStatus::Running).await;
    let op_id = insert_spawn_operation(&repo, None, None).await;
    let mut worker = session("ws-null-op-key", track_id.clone(), 1);
    worker.spawn_op_id = Some(op_id);
    insert_session(&repo, worker).await;

    let fake = Arc::new(FakeProvider::new().with_probe_script([exited_liveness()]));
    let repo_dyn: Arc<dyn Repo> = repo.clone();
    let reaper = Reaper::new(
        repo_dyn,
        registry(fake),
        EventBus::new(),
        write_context(&repo).await,
    );

    reaper_on_boot();
    reaper.sweep_all().await;

    let worker = repo
        .session_get(&WorkerSessionId::from("ws-null-op-key"))
        .await
        .expect("session get")
        .expect("session exists");
    assert_eq!(worker.state, WorkerSessionState::Failed);
    assert_eq!(task_failed_events(&repo, &task.id).await.len(), 0);

    reset_reaper_boot_gate_for_test();
}

#[tokio::test]
async fn sweep_exited_race_lost_after_live_terminal_completion_emits_no_second_event() {
    let _guard = REAPER_TEST_LOCK.lock().await;
    reset_reaper_boot_gate_for_test();

    let (repo, track_id) = seeded_repo().await;
    let task = insert_task(&repo, &track_id, "race", TaskStatus::Running).await;
    let mut worker = session("ws-race", track_id.clone(), 1);
    let worker_card = insert_session(&repo, worker.clone()).await;
    let op_id = insert_spawn_operation(&repo, Some(&task.id), Some(worker_card.id.as_str())).await;
    worker.spawn_op_id = Some(op_id);
    sqlx::query("UPDATE worker_sessions SET spawn_op_id = ?1 WHERE id = ?2")
        .bind(worker.spawn_op_id.as_deref())
        .bind(worker.id.as_str())
        .execute(repo.pool())
        .await
        .expect("stamp session spawn op");

    let events = EventBus::new();
    let write = write_context(&repo).await;
    crate::scheduler::complete_terminal_task(
        repo.as_ref(),
        &events,
        &write,
        &task.id,
        track_id.as_str(),
        worker_card.id.as_str(),
        Some(0),
        false,
        "",
        false,
    )
    .await
    .expect("live terminal completion");

    let fake = Arc::new(FakeProvider::new().with_probe_script([exited_liveness()]));
    let repo_dyn: Arc<dyn Repo> = repo.clone();
    let reaper = Reaper::new(repo_dyn, registry(fake), events, write);

    reaper_on_boot();
    reaper.sweep_all().await;

    assert_eq!(task_failed_events(&repo, &task.id).await.len(), 0);
    let task_row = repo
        .task_get(&task.id)
        .await
        .expect("task get")
        .expect("task exists");
    assert_eq!(task_row.status, TaskStatus::Done);
    let completed = RepoEventWrite::events_since(repo.as_ref(), 0, i64::MAX)
        .await
        .expect("events")
        .into_iter()
        .filter(|(_id, _version, _scope, event)| {
            matches!(event, Event::TaskCompleted { idempotency_key, .. } if idempotency_key == &task.id)
        })
        .count();
    assert_eq!(completed, 1);
    let worker = repo
        .session_get(&WorkerSessionId::from("ws-race"))
        .await
        .expect("session get")
        .expect("session exists");
    assert_eq!(worker.state, WorkerSessionState::Failed);

    reset_reaper_boot_gate_for_test();
}

#[tokio::test]
async fn sweep_unknown_liveness_records_t2_without_death_convergence() {
    let _guard = REAPER_TEST_LOCK.lock().await;
    reset_reaper_boot_gate_for_test();

    let (repo, track_id) = seeded_repo().await;
    let task = insert_task(&repo, &track_id, "unknown", TaskStatus::Running).await;
    let op_id = insert_spawn_operation(&repo, Some(&task.id), None).await;
    let mut worker = session("ws-unknown-death", track_id.clone(), 1);
    worker.spawn_op_id = Some(op_id);
    insert_session(&repo, worker).await;

    let fake =
        Arc::new(FakeProvider::new().with_probe_script([Liveness::Unknown { since_ms: 55 }]));
    let repo_dyn: Arc<dyn Repo> = repo.clone();
    let reaper = Reaper::new(
        repo_dyn,
        registry(fake),
        EventBus::new(),
        write_context(&repo).await,
    );

    reaper_on_boot();
    reaper.sweep_all().await;

    let worker = repo
        .session_get(&WorkerSessionId::from("ws-unknown-death"))
        .await
        .expect("session get")
        .expect("session exists");
    assert_eq!(worker.state, WorkerSessionState::Running);
    assert_eq!(worker.liveness, LivenessTag::Unknown);
    assert!(worker.liveness_probed_at_ms.is_some());
    let task_row = repo
        .task_get(&task.id)
        .await
        .expect("task get")
        .expect("task exists");
    assert_eq!(task_row.status, TaskStatus::Running);
    assert_eq!(task_failed_events(&repo, &task.id).await.len(), 0);

    reset_reaper_boot_gate_for_test();
}

#[tokio::test]
async fn sweep_noops_until_reaper_on_boot_opens_gate() {
    let _guard = REAPER_TEST_LOCK.lock().await;
    reset_reaper_boot_gate_for_test();

    let (repo, track_id) = seeded_repo().await;
    insert_session(&repo, session("ws-gated", track_id, 1)).await;
    let fake = Arc::new(FakeProvider::new().with_probe_script([Liveness::Alive {
        active_turn_id: None,
    }]));
    let repo_dyn: Arc<dyn Repo> = repo.clone();
    let reaper = Reaper::new(
        repo_dyn,
        registry(fake.clone()),
        EventBus::new(),
        write_context(&repo).await,
    );

    reaper.sweep_all().await;
    assert_eq!(fake.probe_call_count(), 0);
    let before = repo
        .session_get(&WorkerSessionId::from("ws-gated"))
        .await
        .expect("session get")
        .expect("session exists");
    assert_eq!(before.liveness, LivenessTag::Unknown);
    assert_eq!(before.liveness_probed_at_ms, None);

    reaper_on_boot();
    reaper.sweep_all().await;

    assert_eq!(fake.probe_call_count(), 1);
    let after = repo
        .session_get(&WorkerSessionId::from("ws-gated"))
        .await
        .expect("session get")
        .expect("session exists");
    assert_eq!(after.liveness, LivenessTag::Alive);
    assert_eq!(after.state, WorkerSessionState::Running);
    assert!(after.liveness_probed_at_ms.is_some());

    reset_reaper_boot_gate_for_test();
}
