//! Report declaration -> scheduler -> harness -> atomic task settlement.
use super::completed_commit_tests::Fixture;
use super::*;
use crate::db::prelude::*;
use crate::event::EditAuthor;
use crate::model::TaskStatus;
use crate::track_report::{TrackReportPayload, persist_report, resolve_report_for_track};
use provider::events::PlannerEventKind;
use serde_json::json;

const HANDOFF: &str =
    "在飞 attempt: c2\n已核实 head: abc\n发现: {request_id} {status}\n```text\n下一步：继续\n```\n";

pub(super) async fn declared(
    f: &Fixture,
) -> (Arc<crate::scheduler::Scheduler>, crate::state::WriteContext) {
    use crate::event::EditAuthor;
    use crate::model::NewCard;
    use crate::state::WriteContext;
    use crate::track_report::TrackReportPayload;
    use crate::track_report::{persist_report, resolve_report_for_track};
    let inner = &f.harness.inner;
    *inner.last_turn_id.lock().await = Some("old-turn".into());
    f.repo
        .card_create(NewCard {
            track_id: inner.track_id.clone(),
            kind: "track-report".into(),
            sort: None,
            title: None,
            payload: serde_json::to_value(TrackReportPayload::initial()).unwrap(),
        })
        .await
        .unwrap();
    let write = WriteContext::new(
        inner.card_role_cache.clone(),
        inner.track_area_cache.clone(),
    );
    let (track, card, current) = resolve_report_for_track(f.repo.as_ref(), inner.track_id.as_str())
        .await
        .unwrap();
    let body = format!(
        "{}\n{}",
        current.body,
        calm_types::report_blocks::render_fence(
            "task",
            &json!({
                "key":"compact", "kind":"compact", "context":HANDOFF, "ready":true, "declared_by":"spec"
            })
        )
    );
    persist_report(
        f.repo.as_ref(),
        &inner.events,
        &write,
        ActorId::KernelDispatcher,
        EditAuthor::Planner,
        track,
        card,
        current.clone(),
        TrackReportPayload::new(current.summary, body),
        current.doc_rev,
        None,
    )
    .await
    .unwrap();
    let scheduler = crate::scheduler::Scheduler::new(
        f.repo.clone(),
        inner.events.clone(),
        write.clone(),
        std::sync::Weak::new(),
        Default::default(),
        Arc::new(tokio::sync::Semaphore::new(1)),
        std::path::PathBuf::from("/tmp"),
        crate::scheduler::WorkerLiveness::DEFAULT,
        crate::scheduler::WorkerIdleWake::new(
            f.daemon.clone(),
            Duration::from_secs(60),
            Duration::from_secs(1),
        ),
    );
    let registry = crate::harness::HarnessRegistry::new();
    registry.insert(inner.worker_session_id.clone(), f.harness.clone());
    scheduler.bind_compaction_harness(registry);
    scheduler.mark_boot_sweep_complete();
    scheduler.mark_context_sweep_boot_complete();
    scheduler.schedule_track(inner.track_id.clone()).await;
    (scheduler, write)
}

async fn finish(f: &Fixture, status: &str) -> Result<()> {
    let live = f.harness.inner.live_claim.writer();
    for kind in [
        PlannerEventKind::TurnStarted {
            turn_id: "compact-task".into(),
        },
        PlannerEventKind::TurnCompleted {
            turn: json!({"id":"compact-task", "status":status}),
        },
    ] {
        on_notification(
            &f.harness.inner,
            &live,
            PlannerEvent {
                thread_id: Some("thread-commit-wake".into()),
                kind,
            },
        )
        .await?;
    }
    Ok(())
}

async fn redeclare(f: &Fixture, write: &crate::state::WriteContext) {
    let inner = &f.harness.inner;
    let (track, card, current) = resolve_report_for_track(f.repo.as_ref(), inner.track_id.as_str())
        .await
        .unwrap();
    persist_report(
        f.repo.as_ref(),
        &inner.events,
        write,
        ActorId::KernelDispatcher,
        EditAuthor::Planner,
        track,
        card,
        current.clone(),
        TrackReportPayload::new(current.summary, current.body),
        current.doc_rev,
        None,
    )
    .await
    .unwrap();
}

async fn closed_loop(status: &str) {
    let f = Fixture::new().await;
    let (scheduler, write) = declared(&f).await;
    let inner = &f.harness.inner;
    let id = format!("{}:compact", inner.track_id);
    // A queued input and a running turn both precede compaction.
    *inner.state.lock().await = HarnessState::turn_running("old-turn".into(), Instant::now());
    self_compact::tick(inner, true).await.unwrap();
    assert!(f.daemon.compacted_threads_for_test().is_empty());
    *inner.state.lock().await = HarnessState::Idle;
    self_compact::tick(inner, false).await.unwrap();
    assert!(f.daemon.compacted_threads_for_test().is_empty());
    scheduler.sweep_all().await;
    self_compact::tick(inner, true).await.unwrap();
    let running = f.repo.task_get(&id).await.unwrap().unwrap();
    assert_eq!(running.status, TaskStatus::Running);
    assert!(running.worker_card_id.is_none() && running.running_deadline_ms.is_none());
    finish(&f, status).await.unwrap();
    let terminal = f.repo.task_get(&id).await.unwrap().unwrap();
    assert_eq!(
        terminal.status,
        if status == "completed" {
            TaskStatus::Done
        } else {
            TaskStatus::Failed
        }
    );
    if status != "completed" {
        assert_eq!(
            terminal.status_detail,
            Some(format!("compaction-incomplete: {status}"))
        );
    }
    let events = f.repo.events_since(0, 1000).await.unwrap();
    let mut pushes = 0;
    let mut wakes = 0;
    for (_, _, _, event) in &events {
        if matches!(
            event,
            Event::TaskCompleted { .. }
                | Event::TaskFailed { .. }
                | Event::TrackWakeRequested { .. }
        ) && crate::dispatcher::task_event_pushes_planner_for_test(
            f.repo.as_ref(),
            &write,
            event,
            &ActorId::Kernel,
        )
        .await
        {
            pushes += 1;
        }
        if let Event::TrackWakeRequested { key, text, .. } = event {
            assert_eq!(key, &id);
            assert!(text.contains("neige_report_read") && text.contains("neige_task_ls"));
            assert!(text.ends_with(HANDOFF), "handoff changed: {text:?}");
            wakes += 1;
        }
    }
    assert_eq!((pushes, wakes), (1, 1), "exactly one qualified wake");
    self_compact::clear_settled(inner).await;
    redeclare(&f, &write).await;
    scheduler.schedule_track(inner.track_id.clone()).await;
    scheduler.sweep_all().await;
    self_compact::tick(inner, true).await.unwrap();
    assert_eq!(
        f.repo.task_get(&id).await.unwrap().unwrap().status,
        terminal.status,
        "terminal key stays frozen"
    );
    assert_eq!(f.daemon.compacted_threads_for_test().len(), 1);
    assert!(
        inner.planner_compaction.lock().await.is_none(),
        "redeclare never requests again"
    );
    let after = f.repo.task_get(&id).await.unwrap().unwrap();
    assert_eq!(after.finished_at_ms, terminal.finished_at_ms);
    assert_eq!(after.status_detail, terminal.status_detail);
    let events = f.repo.events_since(0, 1000).await.unwrap();
    assert_eq!(events.iter().filter(|event| matches!(&event.3, Event::TaskDispatched { idempotency_key, .. } if idempotency_key == &id)).count(), 1);
}

#[tokio::test]
async fn compact_task_handoff_completed() {
    closed_loop("completed").await;
}
#[tokio::test]
async fn compact_task_handoff_interrupted() {
    closed_loop("interrupted").await;
}
#[tokio::test]
async fn compact_task_handoff_failed() {
    closed_loop("failed").await;
}
#[tokio::test]
async fn compact_task_handoff_unconfirmed() {
    closed_loop("unconfirmed").await;
}

#[tokio::test]
async fn compact_task_restart_settles_without_resend() {
    let f = Fixture::new().await;
    let (scheduler, _) = declared(&f).await;
    scheduler.sweep_all().await;
    self_compact::tick(&f.harness.inner, true).await.unwrap();
    let restored = super::self_compact_tests::reload(&f).await;
    self_compact::tick(&restored.inner, true).await.unwrap();
    persist_snapshot(&restored.inner).await.unwrap();
    let id = format!("{}:compact", f.harness.inner.track_id);
    let task = f.repo.task_get(&id).await.unwrap().unwrap();
    assert_eq!(task.status, TaskStatus::Failed);
    assert_eq!(
        task.status_detail.as_deref(),
        Some("compaction-incomplete: unconfirmed")
    );
    assert_eq!(f.daemon.compacted_threads_for_test().len(), 1);
    let events = f.repo.events_since(0, 1000).await.unwrap();
    let text = events
        .iter()
        .find_map(|event| match &event.3 {
            Event::TrackWakeRequested { text, .. } => Some(text),
            _ => None,
        })
        .unwrap();
    assert!(
        text.contains("unconfirmed") && text.ends_with(HANDOFF),
        "{text:?}"
    );
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(event.3, Event::TrackWakeRequested { .. }))
            .count(),
        1
    );
}

#[tokio::test]
async fn compact_task_sweep_fails_superseded_session() {
    for started in [false, true] {
        let f = Fixture::new().await;
        let (scheduler, write) = declared(&f).await;
        if started {
            self_compact::tick(&f.harness.inner, true).await.unwrap();
        }
        sqlx::query("UPDATE worker_sessions SET state='superseded' WHERE id=?")
            .bind(&f.harness.inner.worker_session_id)
            .execute(f.repo.pool())
            .await
            .unwrap();
        scheduler.sweep_all().await;
        let id = format!("{}:compact", f.harness.inner.track_id);
        let task = f.repo.task_get(&id).await.unwrap().unwrap();
        assert_eq!(
            task.status_detail.as_deref(),
            Some("compaction-refused: superseded")
        );
        let event = f
            .repo
            .events_since(0, 1000)
            .await
            .unwrap()
            .into_iter()
            .find(|event| matches!(event.3, Event::TaskFailed { .. }))
            .unwrap();
        assert!(
            crate::dispatcher::task_event_pushes_planner_for_test(
                f.repo.as_ref(),
                &write,
                &event.3,
                &ActorId::KernelDispatcher
            )
            .await
        );
    }
}

#[tokio::test]
async fn compact_task_settlement_rolls_back_with_wake() {
    let f = Fixture::new().await;
    declared(&f).await;
    self_compact::tick(&f.harness.inner, true).await.unwrap();
    sqlx::query(
        "CREATE TRIGGER compact_task_wake_fail BEFORE INSERT ON events \
         WHEN NEW.kind='track.wake_requested' \
         BEGIN SELECT RAISE(ABORT,'blocked wake'); END",
    )
    .execute(f.repo.pool())
    .await
    .unwrap();
    assert!(finish(&f, "completed").await.is_err());
    let id = format!("{}:compact", f.harness.inner.track_id);
    assert_eq!(
        f.repo.task_get(&id).await.unwrap().unwrap().status,
        TaskStatus::Running
    );
    assert!(
        !f.repo
            .events_since(0, 1000)
            .await
            .unwrap()
            .iter()
            .any(|event| matches!(event.3, Event::TaskCompleted { .. }))
    );
    sqlx::query("DROP TRIGGER compact_task_wake_fail")
        .execute(f.repo.pool())
        .await
        .unwrap();
    persist_snapshot(&f.harness.inner).await.unwrap();
    assert_eq!(
        f.repo.task_get(&id).await.unwrap().unwrap().status,
        TaskStatus::Done
    );
}

#[tokio::test]
async fn compact_task_second_key_refuses_already_pending_and_pushes() {
    let f = Fixture::new().await;
    let (scheduler, write) = declared(&f).await;
    let inner = &f.harness.inner;
    let (track, card, current) = resolve_report_for_track(f.repo.as_ref(), inner.track_id.as_str())
        .await
        .unwrap();
    let body = format!(
        "{}\n{}",
        current.body,
        calm_types::report_blocks::render_fence(
            "task",
            &json!({
                "key":"compact-second", "kind":"compact", "context":HANDOFF, "ready":true, "declared_by":"spec"
            })
        )
    );
    persist_report(
        f.repo.as_ref(),
        &inner.events,
        &write,
        ActorId::KernelDispatcher,
        EditAuthor::Planner,
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
    let id = format!("{}:compact-second", inner.track_id);
    let task = f.repo.task_get(&id).await.unwrap().unwrap();
    assert_eq!(task.status, TaskStatus::Failed);
    assert_eq!(
        task.status_detail.as_deref(),
        Some("compaction-refused: already_pending")
    );
    let event = f.repo.events_since(0, 1000).await.unwrap().into_iter().find(|event| matches!(&event.3, Event::TaskFailed { idempotency_key, .. } if idempotency_key == &id)).unwrap();
    assert!(
        crate::dispatcher::task_event_pushes_planner_for_test(
            f.repo.as_ref(),
            &write,
            &event.3,
            &ActorId::KernelDispatcher
        )
        .await
    );
    assert!(f.daemon.compacted_threads_for_test().is_empty());
}

#[tokio::test]
async fn compact_task_delayed_sweep_after_record_clear_is_inert() {
    use crate::test_seams::{PausePoint, install_pause_for_test};
    let f = Fixture::new().await;
    let (scheduler, _) = declared(&f).await;
    let inner = &f.harness.inner;
    let id = format!("{}:compact", inner.track_id);
    let pause = PausePoint::default();
    install_pause_for_test("compact-sweep-before-drive", &id, pause.clone());
    let sweep = scheduler.sweep_all();
    tokio::pin!(sweep);
    tokio::select! {
        _ = &mut sweep => panic!("sweep did not pause"),
        _ = pause.entered.notified() => {}
    }
    self_compact::tick(inner, true).await.unwrap();
    finish(&f, "completed").await.unwrap();
    self_compact::clear_settled(inner).await;
    persist_snapshot(inner).await.unwrap();
    let terminal = f.repo.task_get(&id).await.unwrap().unwrap();
    pause.release.notify_one();
    sweep.await;
    assert!(
        inner.planner_compaction.lock().await.is_none(),
        "stale sweep must not request again"
    );
    self_compact::tick(inner, true).await.unwrap();
    assert_eq!(f.daemon.compacted_threads_for_test().len(), 1);
    assert_eq!(
        f.repo.task_get(&id).await.unwrap().unwrap().updated_at_ms,
        terminal.updated_at_ms
    );
    assert_eq!(
        f.repo
            .events_since(0, 1000)
            .await
            .unwrap()
            .iter()
            .filter(|event| matches!(event.3, Event::TrackWakeRequested { .. }))
            .count(),
        1
    );
}

#[tokio::test]
async fn compact_task_start_lost_guard_rolls_back() {
    let f = Fixture::new().await;
    declared(&f).await;
    let inner = &f.harness.inner;
    let id = format!("{}:compact", inner.track_id);
    // The task settles after request admission, before the idle Start transaction.
    sqlx::query("UPDATE tasks SET status='done',finished_at_ms=7,updated_at_ms=7 WHERE id=?")
        .bind(&id)
        .execute(f.repo.pool())
        .await
        .unwrap();
    let before = f
        .repo
        .session_projection_active_for_card(&inner.card_id.to_string())
        .await
        .unwrap()
        .unwrap();
    assert!(self_compact::tick(inner, true).await.is_err());
    let after = f
        .repo
        .session_projection_active_for_card(&inner.card_id.to_string())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        after.handle_state_json, before.handle_state_json,
        "Start snapshot rolled back"
    );
    assert!(matches!(
        inner
            .planner_compaction
            .lock()
            .await
            .as_ref()
            .unwrap()
            .phase,
        crate::harness::compact_outcome::PlannerCompactionPhase::Requested
    ));
    assert!(f.daemon.compacted_threads_for_test().is_empty());
    let task = f.repo.task_get(&id).await.unwrap().unwrap();
    assert_eq!(
        (task.status, task.updated_at_ms, task.finished_at_ms),
        (TaskStatus::Done, 7, Some(7))
    );
    assert!(
        !f.repo
            .events_since(0, 1000)
            .await
            .unwrap()
            .iter()
            .any(|event| matches!(event.3, Event::TrackWakeRequested { .. }))
    );
}

#[tokio::test]
async fn compact_task_handoff_uses_claim_frozen_context() {
    let f = Fixture::new().await;
    let (_, write) = declared(&f).await;
    let inner = &f.harness.inner;
    let id = format!("{}:compact", inner.track_id);
    assert_eq!(
        f.repo.task_get(&id).await.unwrap().unwrap().status,
        TaskStatus::Dispatched
    );
    let target =
        crate::track_report::ReportEditTarget::resolve(f.repo.as_ref(), inner.track_id.as_str())
            .await
            .unwrap();
    let (_, _, current) = resolve_report_for_track(f.repo.as_ref(), inner.track_id.as_str())
        .await
        .unwrap();
    let block = current
        .blocks
        .as_ref()
        .unwrap()
        .iter()
        .find(|block| block.kind == "task")
        .unwrap();
    let mut payload = block.payload.clone();
    payload["context"] = json!("new declaration must not replace frozen handoff");
    let op = crate::track_report::ReportDocOp::UpsertBlock {
        id: Some(block.id.clone()),
        kind: "task".into(),
        content: calm_types::report_blocks::render_fence("task", &payload),
        if_rev: Some(block.rev),
        if_doc_rev: None,
        position: None,
    };
    let (updated, _) = crate::track_report::write::rest_user_block_op(
        f.repo.as_ref(),
        &inner.events,
        &write,
        target,
        op,
    )
    .await
    .unwrap();
    let updated: TrackReportPayload = serde_json::from_value(updated.payload).unwrap();
    assert_eq!(
        updated
            .blocks
            .unwrap()
            .iter()
            .find(|block| block.kind == "task")
            .unwrap()
            .payload["context"],
        payload["context"]
    );
    assert_eq!(
        f.repo.task_get(&id).await.unwrap().unwrap().context_json,
        serde_json::to_string(HANDOFF).unwrap()
    );
    self_compact::tick(inner, true).await.unwrap();
    finish(&f, "completed").await.unwrap();
    let events = f.repo.events_since(0, 1000).await.unwrap();
    let wakes: Vec<_> = events
        .iter()
        .filter_map(|event| match &event.3 {
            Event::TrackWakeRequested { text, .. } => Some(text),
            _ => None,
        })
        .collect();
    assert_eq!(wakes.len(), 1);
    assert!(wakes[0].ends_with(HANDOFF), "{:?}", wakes[0]);
}

#[tokio::test]
async fn compact_task_handoff_historical_empty_object_keeps_old_wake() {
    let f = Fixture::new().await;
    declared(&f).await;
    let inner = &f.harness.inner;
    let id = format!("{}:compact", inner.track_id);
    // Simulate an already-claimed pre-handoff row, not a newly admitted declaration.
    sqlx::query("UPDATE tasks SET context_json='{}' WHERE id=?")
        .bind(&id)
        .execute(f.repo.pool())
        .await
        .unwrap();
    self_compact::tick(inner, true).await.unwrap();
    finish(&f, "completed").await.unwrap();
    let events = f.repo.events_since(0, 1000).await.unwrap();
    let wakes: Vec<_> = events
        .iter()
        .filter_map(|event| match &event.3 {
            Event::TrackWakeRequested { text, .. } => Some(text),
            _ => None,
        })
        .collect();
    assert_eq!(wakes.len(), 1);
    assert_eq!(
        wakes[0],
        &include_str!("../../../tests/goldens/planner_compaction_historical_wake.txt")
            .trim_end()
            .replace("{request_id}", &id)
    );
}

#[tokio::test]
async fn compact_task_handoff_refused_after_start() {
    let f = Fixture::new().await;
    declared(&f).await;
    let inner = &f.harness.inner;
    self_compact::tick(inner, true).await.unwrap();
    compact_command::settle(
        inner,
        crate::harness::compact_outcome::CompactStatus::Refused,
    )
    .await;
    persist_snapshot(inner).await.unwrap();
    persist_snapshot(inner).await.unwrap();
    let events = f.repo.events_since(0, 1000).await.unwrap();
    let wakes: Vec<_> = events
        .iter()
        .filter_map(|event| match &event.3 {
            Event::TrackWakeRequested { text, .. } => Some(text),
            _ => None,
        })
        .collect();
    assert_eq!(wakes.len(), 1);
    assert!(
        wakes[0].contains("refused") && wakes[0].ends_with(HANDOFF),
        "{:?}",
        wakes[0]
    );
}
