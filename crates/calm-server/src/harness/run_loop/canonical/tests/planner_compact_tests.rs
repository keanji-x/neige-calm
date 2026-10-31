//! Declared canonical maintenance through the scheduler and real native transport.
use super::compact_tests::{ready, shutdown};
use super::*;
use crate::harness::run_loop::compact_task_tests::{HANDOFF, declared_for};
use crate::model::TaskStatus;
use calm_truth::db::RepoEventWrite;

async fn declare(fx: &Fixture) -> Arc<crate::scheduler::Scheduler> {
    declared_for(fx.original.repo.clone(), &fx.harness, fx.daemon.clone())
        .await
        .0
}

async fn assert_terminal(fx: &Fixture, status: TaskStatus, detail: Option<&str>) {
    let id = format!("{}:compact", fx.harness.inner.track_id);
    let task = fx.original.repo.task_get(&id).await.unwrap().unwrap();
    assert_eq!(task.status, status);
    assert_eq!(task.status_detail.as_deref(), detail);
    let events = fx.original.repo.events_since(0, 1000).await.unwrap();
    let wakes: Vec<_> = events
        .iter()
        .filter_map(|event| {
            if let Event::TrackWakeRequested { key, text, .. } = &event.3 {
                (key == &id).then_some(text)
            } else {
                None
            }
        })
        .collect();
    assert_eq!(wakes.len(), 1);
    assert!(wakes[0].ends_with(HANDOFF));
    assert_eq!(events.iter().filter(|event| matches!(&event.3, Event::TaskCompleted { idempotency_key, .. } | Event::TaskFailed { idempotency_key, .. } if idempotency_key == &id)).count(), 1);
    let write = crate::state::WriteContext::new(
        fx.harness.inner.card_role_cache.clone(),
        fx.harness.inner.track_area_cache.clone(),
    );
    let mut pushes = 0;
    for (_, _, _, event) in &events {
        if matches!(
            event,
            Event::TaskCompleted { .. }
                | Event::TaskFailed { .. }
                | Event::TrackWakeRequested { .. }
        ) && crate::dispatcher::task_event_pushes_planner_for_test(
            fx.original.repo.as_ref(),
            &write,
            event,
            &ActorId::Kernel,
        )
        .await
        {
            pushes += 1;
        }
    }
    assert_eq!(pushes, 1, "one qualified wake and no duplicate task push");
}

pub(super) async fn positive() {
    let (fx, _client, mut peer) = ready().await;
    let before = fx.frame().await;
    fx.auth.sessions.remove(&fx.session).unwrap();
    let scheduler = declare(&fx).await;
    let id = format!("{}:compact", fx.harness.inner.track_id);
    assert_ne!(
        fx.original
            .repo
            .task_get(&id)
            .await
            .unwrap()
            .unwrap()
            .status,
        TaskStatus::Failed
    );
    let call = request(&mut peer).await;
    assert_eq!(call["method"], "thread/compact/start");
    let running = fx.frame().await;
    assert_eq!(running.header.compact_outcome.unwrap().operation_id, id);
    assert_eq!(
        fx.original
            .repo
            .task_get(&id)
            .await
            .unwrap()
            .unwrap()
            .status,
        TaskStatus::Running
    );
    let pause = crate::test_seams::PausePoint::default();
    crate::test_seams::install_pause_for_test("compact-sweep-before-drive", &id, pause.clone());
    let sweep = scheduler.sweep_all();
    tokio::pin!(sweep);
    tokio::select! {
        _ = &mut sweep => panic!("sweep did not pause"),
        _ = pause.entered.notified() => {}
    }
    send(&mut peer, json!({"id":call["id"], "result":{}})).await;
    let thread = before.header.last_thread_id.as_deref().unwrap();
    notify_thread(
        &fx.daemon,
        &mut peer,
        thread,
        "turn/started",
        json!({"id":"maintenance"}),
    )
    .await;
    notify_thread(
        &fx.daemon,
        &mut peer,
        thread,
        "turn/completed",
        json!({"id":"maintenance", "status":"completed"}),
    )
    .await;
    let done = fx.wait_phase(HarnessPhaseTag::TurnCompleted).await;
    assert_eq!(done.canonical_inputs, before.canonical_inputs);
    assert_eq!(done.header.last_turn_id, before.header.last_turn_id);
    assert_terminal(&fx, TaskStatus::Done, None).await;
    pause.release.notify_one();
    sweep.await;
    scheduler.sweep_all().await;
    assert_terminal(&fx, TaskStatus::Done, None).await;
    crate::shared_codex_appserver::native_owner_tests::assert_no_request(&mut peer).await;
    shutdown(&fx.harness, &mut peer).await;
}

#[tokio::test]
async fn canonical_planner_compact_codex_positive() {
    positive().await;
}

async fn started() -> (
    Fixture,
    Arc<crate::codex_appserver::CodexAppServer>,
    crate::shared_codex_appserver::native_owner_tests::Peer,
    Arc<crate::scheduler::Scheduler>,
) {
    let (fx, client, mut peer) = ready().await;
    let scheduler = declare(&fx).await;
    let id = format!("{}:compact", fx.harness.inner.track_id);
    assert_ne!(
        fx.original
            .repo
            .task_get(&id)
            .await
            .unwrap()
            .unwrap()
            .status,
        TaskStatus::Failed
    );
    let call = request(&mut peer).await;
    assert_eq!(call["method"], "thread/compact/start");
    send(&mut peer, json!({"id":call["id"],"result":{}})).await;
    (fx, client, peer, scheduler)
}

#[tokio::test]
async fn canonical_planner_compact_unconfirmed() {
    let (fx, _client, mut peer) = ready().await;
    declare(&fx).await;
    let id = format!("{}:compact", fx.harness.inner.track_id);
    assert_ne!(
        fx.original
            .repo
            .task_get(&id)
            .await
            .unwrap()
            .unwrap()
            .status,
        TaskStatus::Failed
    );
    let call = request(&mut peer).await;
    assert_eq!(call["method"], "thread/compact/start");
    send(&mut peer, json!({"id":call["id"],"result":null})).await;
    fx.wait_phase(HarnessPhaseTag::Wedged).await;
    assert_terminal(
        &fx,
        TaskStatus::Failed,
        Some("compaction-incomplete: unconfirmed"),
    )
    .await;
    shutdown(&fx.harness, &mut peer).await;
}

#[tokio::test]
async fn canonical_planner_compact_late_end_restores_context_without_rewriting_failed_task() {
    let (fx, _client, mut peer, scheduler) = started().await;
    let inner = &fx.harness.inner;
    let thread = fx.frame().await.header.last_thread_id.unwrap();
    notify_thread(
        &fx.daemon,
        &mut peer,
        &thread,
        "turn/started",
        json!({"id":"maintenance"}),
    )
    .await;
    super::compact_tests::wait_turn(&fx, "maintenance").await;
    let issuance = inner.issuance.lock().await;
    *inner.state.lock().await = HarnessState::CompactionRunning {
        turn_id: "maintenance".into(),
        started_at: Instant::now() - inner.config.max_turn_duration,
    };
    drop(issuance);
    fx.wait_phase_at(HarnessPhaseTag::Wedged, PhaseWaitStage::ExpiredMaintenance)
        .await;
    assert_terminal(
        &fx,
        TaskStatus::Failed,
        Some("compaction-incomplete: unconfirmed"),
    )
    .await;
    let id = format!("{}:compact", inner.track_id);
    let failed_task = serde_json::to_value(fx.original.repo.task_get(&id).await.unwrap()).unwrap();
    let held = fx.frame().await;
    assert_eq!(
        held.header.compact_outcome.as_ref().unwrap().status,
        CompactStatus::Unconfirmed
    );
    notify_thread(
        &fx.daemon,
        &mut peer,
        &thread,
        "turn/completed",
        json!({"id":"maintenance","status":"completed"}),
    )
    .await;
    let completed = fx.wait_phase(HarnessPhaseTag::TurnCompleted).await;
    assert!(completed.header.wedged_reason.is_none());
    let outcome = completed.header.compact_outcome.as_ref().unwrap();
    assert_eq!(outcome.operation_id, id);
    assert_eq!(outcome.turn_id.as_deref(), Some("maintenance"));
    assert_eq!(outcome.status, CompactStatus::Completed);
    assert!(completed.header.context_usage_invalidated);
    scheduler.sweep_all().await;
    assert_eq!(
        serde_json::to_value(fx.original.repo.task_get(&id).await.unwrap()).unwrap(),
        failed_task,
        "late physical completion must not rewrite the historical failed task"
    );
    assert_terminal(
        &fx,
        TaskStatus::Failed,
        Some("compaction-incomplete: unconfirmed"),
    )
    .await;
    crate::shared_codex_appserver::native_owner_tests::assert_no_request(&mut peer).await;
    shutdown(&fx.harness, &mut peer).await;
}

#[tokio::test]
async fn canonical_planner_compact_settlement_and_wake_roll_back_together() {
    let (fx, _client, mut peer, _) = started().await;
    let inner = &fx.harness.inner;
    let thread = fx.frame().await.header.last_thread_id.unwrap();
    notify_thread(
        &fx.daemon,
        &mut peer,
        &thread,
        "turn/started",
        json!({"id":"maintenance"}),
    )
    .await;
    super::compact_tests::wait_turn(&fx, "maintenance").await;
    let issuance = inner.issuance.lock().await;
    sqlx::query(
        "CREATE TRIGGER canonical_wake_fail BEFORE INSERT ON events \
         WHEN NEW.kind='track.wake_requested' \
         BEGIN SELECT RAISE(ABORT,'blocked wake'); END",
    )
    .execute(fx.original.repo.pool())
    .await
    .unwrap();
    notify_thread(
        &fx.daemon,
        &mut peer,
        &thread,
        "turn/completed",
        json!({"id":"maintenance","status":"completed"}),
    )
    .await;
    assert!(
        crate::harness::run_loop::compact_command::canonical::observe(inner)
            .await
            .is_err()
    );
    let id = format!("{}:compact", inner.track_id);
    assert_eq!(
        fx.original
            .repo
            .task_get(&id)
            .await
            .unwrap()
            .unwrap()
            .status,
        TaskStatus::Running
    );
    assert_eq!(fx.frame().await.header.phase, HarnessPhaseTag::Compacting);
    assert!(!fx.original.repo.events_since(0,1000).await.unwrap().iter().any(|event| matches!(&event.3, Event::TaskCompleted { idempotency_key, .. } if idempotency_key == &id)));
    sqlx::query("DROP TRIGGER canonical_wake_fail")
        .execute(fx.original.repo.pool())
        .await
        .unwrap();
    drop(issuance);
    fx.wait_phase(HarnessPhaseTag::TurnCompleted).await;
    assert_terminal(&fx, TaskStatus::Done, None).await;
    shutdown(&fx.harness, &mut peer).await;
}

#[tokio::test]
async fn canonical_planner_compact_refused_restores_and_fails() {
    let (fx, client, mut peer) = ready().await;
    let blocked = client.lock_outbound_sink_for_test().await;
    let _scheduler = declare(&fx).await;
    let id = format!("{}:compact", fx.harness.inner.track_id);
    assert_ne!(
        fx.original
            .repo
            .task_get(&id)
            .await
            .unwrap()
            .unwrap()
            .status,
        TaskStatus::Failed
    );
    tokio::time::timeout(Duration::from_secs(5), async {
        while client.pending_requests_for_test() != 1 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    fx.harness
        .inner
        .issuance_paused
        .store(true, Ordering::SeqCst);
    drop(blocked);
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if fx
                .frame()
                .await
                .header
                .compact_outcome
                .as_ref()
                .is_some_and(|outcome| {
                    outcome.status == crate::harness::compact_outcome::CompactStatus::Refused
                })
            {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert_terminal(
        &fx,
        TaskStatus::Failed,
        Some("compaction-incomplete: refused"),
    )
    .await;
    assert_eq!(
        fx.frame().await.header.phase,
        HarnessPhaseTag::TurnCompleted
    );
    crate::shared_codex_appserver::native_owner_tests::assert_no_request(&mut peer).await;
    shutdown(&fx.harness, &mut peer).await;
}

#[tokio::test]
async fn canonical_planner_compact_restart_never_resubmits() {
    let (fx, _client, mut peer, scheduler) = started().await;
    let inner = &fx.harness.inner;
    shutdown(&fx.harness, &mut peer).await;
    let restored = PlannerHarness::run(PlannerHarnessParams {
        track_delete_locks: Default::default(),
        worker_session_id: inner.worker_session_id.clone(),
        track_id: inner.track_id.clone(),
        card_id: inner.card_id.clone(),
        thread_id: inner.thread_id.read().await.clone(),
        repo: inner.repo.clone(),
        events: inner.events.clone(),
        card_role_cache: inner.card_role_cache.clone(),
        track_area_cache: inner.track_area_cache.clone(),
        backend: inner.backend.clone(),
        registry: fx.registry.clone(),
        config: HarnessConfig::default(),
        snapshot: HarnessRuntimeSnapshot::decode(serde_json::to_value(fx.frame().await).unwrap())
            .unwrap(),
    })
    .await
    .unwrap();
    assert_eq!(
        restored.runtime_header().await.unwrap().phase,
        HarnessPhaseTag::Wedged
    );
    assert_terminal(
        &fx,
        TaskStatus::Failed,
        Some("compaction-incomplete: unconfirmed"),
    )
    .await;
    scheduler.sweep_all().await;
    assert_terminal(
        &fx,
        TaskStatus::Failed,
        Some("compaction-incomplete: unconfirmed"),
    )
    .await;
    crate::shared_codex_appserver::native_owner_tests::assert_no_request(&mut peer).await;
    shutdown(&restored, &mut peer).await;
}

#[tokio::test]
async fn canonical_planner_compact_pending_input_wins_admission() {
    let (fx, _client, mut peer) = ready().await;
    super::compact_tests::accept_during_compact(&fx).await;
    let scheduler = declare(&fx).await;
    let id = format!("{}:compact", fx.harness.inner.track_id);
    let task = fx.original.repo.task_get(&id).await.unwrap().unwrap();
    // The fixture's original Human is still pending, so no task or input is consumed.
    assert_eq!(task.status, TaskStatus::Dispatched);
    let before = fx.frame().await;
    assert_eq!(before.canonical_inputs.pending.len(), 1);
    assert!(before.header.compact_outcome.is_none());
    scheduler.sweep_all().await;
    assert_eq!(fx.frame().await, before);
    crate::shared_codex_appserver::native_owner_tests::assert_no_request(&mut peer).await;
    shutdown(&fx.harness, &mut peer).await;
}

#[tokio::test]
async fn canonical_planner_compact_second_key_already_pending() {
    use crate::track_report::{TrackReportPayload, persist_report, resolve_report_for_track};
    let (fx, _client, mut peer) = ready().await;
    let issuance = crate::per_card_lock::lock_key(
        &fx.harness.inner.track_delete_locks,
        fx.harness.inner.track_id.as_str(),
    )
    .await;
    let declaration = declare(&fx);
    tokio::pin!(declaration);
    // Acknowledgement stays available while native admission waits on this track fence.
    let scheduler = declaration.await;
    let inner = &fx.harness.inner;
    let write = crate::state::WriteContext::new(
        inner.card_role_cache.clone(),
        inner.track_area_cache.clone(),
    );
    let (track, card, current) =
        resolve_report_for_track(fx.original.repo.as_ref(), inner.track_id.as_str())
            .await
            .unwrap();
    let mut declaration = current
        .blocks
        .as_ref()
        .unwrap()
        .iter()
        .find(|block| block.kind == "task")
        .unwrap()
        .payload
        .clone();
    declaration["key"] = json!("second");
    let body = format!(
        "{}\n{}",
        current.body,
        calm_types::report_blocks::render_fence("task", &declaration)
    );
    persist_report(
        fx.original.repo.as_ref(),
        &inner.events,
        &write,
        ActorId::KernelDispatcher,
        crate::event::EditAuthor::Planner,
        track,
        card,
        current.clone(),
        TrackReportPayload::new(current.summary, body),
        current.doc_rev,
        None,
    )
    .await
    .unwrap();
    scheduler.schedule_track(inner.track_id.clone()).await;
    let task = fx
        .original
        .repo
        .task_get(&format!("{}:second", inner.track_id))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        task.status_detail.as_deref(),
        Some("compaction-refused: already_pending")
    );
    drop(issuance);
    // Keep the test bounded without leaving a blocked native request behind.
    let call = request(&mut peer).await;
    assert_eq!(call["method"], "thread/compact/start");
    send(&mut peer, json!({"id":call["id"],"result":null})).await;
    fx.wait_phase(HarnessPhaseTag::Wedged).await;
    shutdown(&fx.harness, &mut peer).await;
}

#[tokio::test]
async fn canonical_planner_compact_system_error_settles_unconfirmed() {
    let (fx, _client, mut peer, _) = started().await;
    let thread = fx.frame().await.header.last_thread_id.unwrap();
    super::content_tests::notify(
        &fx,
        &mut peer,
        "thread/status/changed",
        json!({"threadId":thread,"status":{"type":"systemError"}}),
    )
    .await;
    fx.wait_phase(HarnessPhaseTag::Wedged).await;
    assert_terminal(
        &fx,
        TaskStatus::Failed,
        Some("compaction-incomplete: unconfirmed"),
    )
    .await;
    shutdown(&fx.harness, &mut peer).await;
}

#[tokio::test]
async fn canonical_planner_compact_start_between_request_reads() {
    use crate::test_seams::{PausePoint, install_pause_for_test};
    let (fx, _client, mut peer) = ready().await;
    let id = format!("{}:compact", fx.harness.inner.track_id);
    fx.harness
        .inner
        .issuance_paused
        .store(true, Ordering::SeqCst);
    let pause = PausePoint::default();
    install_pause_for_test("planner-compact-request-frame-read", &id, pause.clone());
    let declaration = declare(&fx);
    tokio::pin!(declaration);
    tokio::select! {
        _ = &mut declaration => panic!("request did not pause"),
        _ = pause.entered.notified() => {}
    }
    assert!(fx.frame().await.header.compact_outcome.is_none());
    let dispatched = fx.original.repo.task_get(&id).await.unwrap().unwrap();
    assert_eq!(dispatched.status, TaskStatus::Dispatched);
    // Hold a second, directly observable acknowledgement at the same boundary.
    let acknowledgement_pause = PausePoint::default();
    install_pause_for_test(
        "planner-compact-request-frame-read",
        &id,
        acknowledgement_pause.clone(),
    );
    let acknowledgement = fx.harness.request_compaction(id.clone());
    tokio::pin!(acknowledgement);
    tokio::select! {
        _ = &mut acknowledgement => panic!("acknowledgement did not pause"),
        _ = acknowledgement_pause.entered.notified() => {}
    }
    fx.harness
        .inner
        .issuance_paused
        .store(false, Ordering::SeqCst);
    // Start commits both Running and the outcome while acknowledgement holds
    // the old frame. Keep the fake native RPC outstanding until after refusal.
    let call = request(&mut peer).await;
    assert_eq!(call["method"], "thread/compact/start");
    assert_eq!(
        fx.original
            .repo
            .task_get(&id)
            .await
            .unwrap()
            .unwrap()
            .status,
        TaskStatus::Running
    );
    pause.release.notify_one();
    acknowledgement_pause.release.notify_one();
    let scheduler = declaration.await;
    assert_eq!(
        acknowledgement.await.unwrap(),
        crate::harness::compact_outcome::CompactionAnswer::Scheduled
    );
    let track = fx
        .original
        .repo
        .track_get(dispatched.track_id.as_str())
        .await
        .unwrap()
        .unwrap();
    // Independently fence a stale refusal, even when admission reads correctly.
    scheduler
        .refuse_compact_for_test(&dispatched, &track)
        .await
        .unwrap();
    assert_eq!(
        fx.original
            .repo
            .task_get(&id)
            .await
            .unwrap()
            .unwrap()
            .status,
        TaskStatus::Running
    );
    assert_eq!(
        fx.harness.request_compaction(id).await.unwrap(),
        crate::harness::compact_outcome::CompactionAnswer::Scheduled
    );
    send(&mut peer, json!({"id":call["id"], "result":{}})).await;
    shutdown(&fx.harness, &mut peer).await;
}

#[tokio::test]
async fn canonical_planner_compact_unavailable_conversation_still_refused() {
    let fx = Fixture::new().await;
    assert!(fx.frame().await.header.last_turn_id.is_none());
    let scheduler = declare(&fx).await;
    let id = format!("{}:compact", fx.harness.inner.track_id);
    let task = fx.original.repo.task_get(&id).await.unwrap().unwrap();
    assert_eq!(task.status, TaskStatus::Failed);
    assert_eq!(
        task.status_detail.as_deref(),
        Some("compaction-refused: conversation_unavailable")
    );
    scheduler.sweep_all().await;
    assert_eq!(
        fx.original
            .repo
            .task_get(&id)
            .await
            .unwrap()
            .unwrap()
            .status,
        TaskStatus::Failed
    );
    fx.harness.shutdown().await.unwrap();
}
