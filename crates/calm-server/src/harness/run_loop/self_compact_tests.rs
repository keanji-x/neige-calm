//! Exercise harness admission, persistence and existing event replay.
use super::completed_commit_tests::Fixture;
use super::*;
use crate::harness::compact_outcome::{CompactionAnswer, CompactionRefusal};
use crate::session_projection_repo::AgentProvider;
use calm_truth::db::RepoEventWrite as _;
use calm_truth::db::RepoRead;
use serde_json::json;

async fn request(f: &Fixture) -> CompactionAnswer {
    *f.harness.inner.last_turn_id.lock().await = Some("old-turn".into());
    f.harness
        .request_compaction("request".into())
        .await
        .unwrap()
}

async fn reload_params(f: &Fixture) -> PlannerHarnessParams {
    let inner = &f.harness.inner;
    let value: String =
        sqlx::query_scalar("SELECT handle_state_json FROM worker_sessions WHERE id = ?")
            .bind(&inner.worker_session_id)
            .fetch_one(f.repo.pool())
            .await
            .unwrap();
    let snapshot = HarnessSnapshot::from_value_strict(serde_json::from_str(&value).unwrap());
    PlannerHarnessParams {
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
        registry: crate::harness::HarnessRegistry::new(),
        config: HarnessConfig::default(),
        snapshot: snapshot.try_into().unwrap(),
    }
}

pub(super) async fn reload(f: &Fixture) -> PlannerHarness {
    PlannerHarness::run_unstarted_for_test(reload_params(f).await, 8).0
}

#[tokio::test]
async fn compact_request_deferral_preserves_request_until_pending_input_drains() {
    let f = Fixture::new().await;
    *f.harness.inner.state.lock().await =
        HarnessState::turn_running("old-turn".into(), Instant::now());
    assert_eq!(request(&f).await, CompactionAnswer::Scheduled);

    assert_eq!(request(&f).await, CompactionAnswer::Scheduled);
    let duplicate = f
        .harness
        .request_compaction("other-request".into())
        .await
        .unwrap();
    assert_eq!(
        duplicate,
        CompactionAnswer::AlreadyPending {
            request_id: "request".into()
        }
    );
    self_compact::tick(&f.harness.inner, true).await.unwrap();
    assert!(f.daemon.compacted_threads_for_test().is_empty());
    *f.harness.inner.state.lock().await = HarnessState::Idle;
    f.enqueue(vec![QueueEntry::user_message(
        "pending".into(),
        None,
        vec![],
    )])
    .await;
    *f.harness.inner.compaction_retry_after.lock().await = None;
    self_compact::tick(&f.harness.inner, true).await.unwrap();
    assert!(f.daemon.compacted_threads_for_test().is_empty());
    assert!(matches!(
        snapshot_for(&f.harness.inner)
            .await
            .unwrap()
            .planner_compaction
            .unwrap()
            .phase,
        crate::harness::compact_outcome::PlannerCompactionPhase::Requested
    ));
    f.harness
        .inner
        .historical_queue()
        .unwrap()
        .lock()
        .await
        .clear();
    *f.harness.inner.compaction_retry_after.lock().await = None;
    self_compact::tick(&f.harness.inner, true).await.unwrap();
    assert_eq!(f.daemon.compacted_threads_for_test().len(), 1);
}

#[tokio::test]
async fn compact_pending_request_blocks_live_hibernation() {
    let f = Fixture::new().await;
    assert_eq!(request(&f).await, CompactionAnswer::Scheduled);
    assert!(!f.harness.hibernate_if_idle().await.unwrap());
}

#[tokio::test]
async fn compact_restart_no_resend_and_wake_is_idempotent() {
    let f = Fixture::new().await;
    assert_eq!(request(&f).await, CompactionAnswer::Scheduled);
    let restored = reload(&f).await;
    self_compact::tick(&restored.inner, true).await.unwrap();
    assert_eq!(f.daemon.compacted_threads_for_test().len(), 1);
    let submitted = snapshot_for(&restored.inner).await.unwrap();
    assert!(matches!(
        submitted.planner_compaction.unwrap().phase,
        crate::harness::compact_outcome::PlannerCompactionPhase::Submitted { .. }
    ));
    let restored = reload(&f).await;
    self_compact::tick(&restored.inner, true).await.unwrap();
    persist_snapshot(&restored.inner).await.unwrap();
    let again = reload(&f).await;
    self_compact::tick(&again.inner, true).await.unwrap();
    persist_snapshot(&again.inner).await.unwrap();
    assert_eq!(
        f.daemon.compacted_threads_for_test().len(),
        1,
        "never resend after Start"
    );
    let events = f.repo.events_since(0, 1000).await.unwrap();
    let wakes: Vec<_> = events
        .iter()
        .filter(|e| {
            matches!(&e.3,
        Event::TrackWakeRequested { source, .. } if source == "planner_compaction")
        })
        .collect();
    assert_eq!(wakes.len(), 1, "one durable wake across restarts");
    let Event::TrackWakeRequested { key, text, .. } = &wakes[0].3 else {
        unreachable!()
    };
    assert_eq!(key, "request");
    assert!(
        text.contains("unconfirmed")
            && text.contains("neige_report_read")
            && text.contains("neige_task_ls")
    );
}

#[tokio::test]
async fn compact_closed_loop_uses_existing_trusted_wake_and_resumes_planner() {
    use provider::events::PlannerEventKind;
    let f = Fixture::new().await;
    assert_eq!(request(&f).await, CompactionAnswer::Scheduled);
    self_compact::tick(&f.harness.inner, true).await.unwrap();
    let live = f.harness.inner.live_claim.writer();
    for kind in [
        PlannerEventKind::TurnStarted {
            turn_id: "compact-turn".into(),
        },
        PlannerEventKind::TurnCompleted {
            turn: json!({"id":"compact-turn","status":"completed"}),
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
        .await
        .unwrap();
    }
    assert_eq!(f.daemon.compacted_threads_for_test().len(), 1);
    let events = f.repo.events_since(0, 1000).await.unwrap();
    let (id, _, _, event) = events
        .into_iter()
        .find(|(_, _, _, event)| {
            matches!(event,
        Event::TrackWakeRequested { source, .. } if source == "planner_compaction")
        })
        .unwrap();
    let Event::TrackWakeRequested { key, text, .. } = &event else {
        unreachable!()
    };
    assert_eq!(key, "request");
    assert!(
        text.contains("compacted at your request")
            && text.contains("Earlier conversation may be missing")
    );
    let observation = crate::dispatcher::resolve_harness_observation(
        f.repo.as_ref(),
        &f.harness.inner.track_id,
        &event,
    )
    .await
    .unwrap()
    .unwrap();
    f.enqueue(vec![QueueEntry::system(observation, Some(id)).unwrap()])
        .await;
    f.issue().await;
    let segments = f.projected_segments().await;
    assert!(
        segments
            .iter()
            .any(|segment| segment.text.contains("neige_report_read")
                && segment.text.contains("neige_task_ls"))
    );
    assert_eq!(f.daemon.turn_start_count_for_test(), 1);
}

#[test]
fn compact_submitted_restart_settles_without_resend() {
    let mut raw = serde_json::to_value(HarnessSnapshot::initial(0, vec![])).unwrap();
    raw["planner_compaction"] = serde_json::json!({
        "request_id":"request-restart", "requested_turn_id":"old-turn", "requested_at_ms":1,"wake_enqueued":false,
        "phase":{"state":"submitted","operation_id":"operation-restart"}
    });
    let mut snapshot = HarnessSnapshot::from_value_strict(raw);
    snapshot.restore_context_feedback();
    let restored = serde_json::to_value(snapshot).unwrap();
    assert_eq!(restored["planner_compaction"]["phase"]["state"], "settled");
    assert_eq!(
        restored["planner_compaction"]["phase"]["status"],
        "unconfirmed"
    );
}

#[test]
fn compact_requested_snapshot_is_not_hibernation_idle() {
    let mut raw = serde_json::to_value(HarnessSnapshot::initial(0, vec![])).unwrap();
    raw["planner_compaction"] = serde_json::json!({
        "request_id":"pending", "requested_turn_id":"old-turn", "requested_at_ms":1,"wake_enqueued":false,
        "phase":{"state":"requested"}
    });
    assert!(!HarnessRuntimeSnapshot::idle_for_lifecycle(raw).unwrap());
}

#[tokio::test]
async fn compact_request_refuses_acp() {
    use crate::harness::profile::{HarnessProfile, PlannerBinding};
    use crate::harness::wiring::{PlannerRow, PlannerWiring};
    let mut f = Fixture::new().await;
    let inner = &f.harness.inner;
    let wiring = PlannerWiring::unconfigured_for_test(f.repo.clone());
    let backend = crate::harness::backend::factory::PlannerFactory::new(
        Arc::downgrade(&f.daemon),
        &wiring.claude,
        &wiring.acp,
    )
    .open(
        f.daemon.thread_seals().clone(),
        &wiring,
        f.repo.clone(),
        PlannerRow {
            binding: PlannerBinding {
                profile: HarnessProfile::Planner,
                provider: AgentProvider::OpenCode,
            },
            worker_session_id: inner.worker_session_id.as_str(),
            card_id: inner.card_id.as_str(),
            track_id: inner.track_id.as_str(),
            prior_total_tokens: 0,
        },
    )
    .await
    .unwrap();
    Arc::get_mut(&mut f.harness.inner).unwrap().backend = backend;
    super::compact_task_tests::declared(&f).await;
    let task = f
        .repo
        .task_get(&format!("{}:compact", f.harness.inner.track_id))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(task.status, crate::model::TaskStatus::Failed);
    assert_eq!(
        task.status_detail.as_deref(),
        Some("compaction-refused: unsupported_provider")
    );
}

#[tokio::test]
async fn compact_request_refuses_existing_maintenance_and_unavailable_conversation() {
    let f = Fixture::new().await;
    *f.harness.inner.last_turn_id.lock().await = Some("old-turn".into());
    f.harness.compact().await.unwrap();
    assert_eq!(
        request(&f).await,
        CompactionAnswer::Refused(CompactionRefusal::CompactionInProgress)
    );
    f.harness.inner.shutting_down.store(true, Ordering::SeqCst);
    assert_eq!(
        request(&f).await,
        CompactionAnswer::Refused(CompactionRefusal::ConversationUnavailable)
    );
    assert_eq!(f.daemon.compacted_threads_for_test().len(), 1);
}

#[tokio::test]
async fn compact_settlement_wakes_with_each_non_completed_status() {
    use provider::events::PlannerEventKind;
    for status in ["interrupted", "failed", "unconfirmed"] {
        let f = Fixture::new().await;
        assert_eq!(request(&f).await, CompactionAnswer::Scheduled);
        self_compact::tick(&f.harness.inner, true).await.unwrap();
        let live = f.harness.inner.live_claim.writer();
        for kind in [
            PlannerEventKind::TurnStarted {
                turn_id: "compact-status".into(),
            },
            PlannerEventKind::TurnCompleted {
                turn: json!({"id":"compact-status","status":status}),
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
            .await
            .unwrap();
        }
        let events = f.repo.events_since(0, 1000).await.unwrap();
        let wakes: Vec<_> = events
            .iter()
            .filter_map(|(_, _, _, event)| match event {
                Event::TrackWakeRequested { source, text, .. }
                    if source == "planner_compaction" =>
                {
                    Some(text)
                }
                _ => None,
            })
            .collect();
        assert_eq!(wakes.len(), 1);
        assert!(wakes[0].contains("did not complete") && wakes[0].contains(status));
    }
}

#[tokio::test]
async fn compact_request_retry_preserves_settlement_until_wake_commits() {
    use provider::events::PlannerEventKind;
    let f = Fixture::new().await;
    assert_eq!(request(&f).await, CompactionAnswer::Scheduled);
    self_compact::tick(&f.harness.inner, true).await.unwrap();
    let live = f.harness.inner.live_claim.writer();
    on_notification(
        &f.harness.inner,
        &live,
        PlannerEvent {
            thread_id: Some("thread-commit-wake".into()),
            kind: PlannerEventKind::TurnStarted {
                turn_id: "compact-retry".into(),
            },
        },
    )
    .await
    .unwrap();
    sqlx::query(
        "CREATE TRIGGER compact_wake_fail BEFORE INSERT ON events
        WHEN NEW.kind='track.wake_requested'
        BEGIN SELECT RAISE(ABORT,'compact wake blocked'); END",
    )
    .execute(f.repo.pool())
    .await
    .unwrap();
    assert!(
        on_notification(
            &f.harness.inner,
            &live,
            PlannerEvent {
                thread_id: Some("thread-commit-wake".into()),
                kind: PlannerEventKind::TurnCompleted {
                    turn: json!({"id":"compact-retry","status":"completed"})
                },
            }
        )
        .await
        .is_err()
    );
    let retry = f
        .harness
        .request_compaction("other-request".into())
        .await
        .unwrap();
    assert_eq!(
        retry,
        CompactionAnswer::AlreadyPending {
            request_id: "request".into()
        },
        "do not replace an uncommitted settle notice"
    );
    sqlx::query("DROP TRIGGER compact_wake_fail")
        .execute(f.repo.pool())
        .await
        .unwrap();
    self_compact::tick(&f.harness.inner, true).await.unwrap();
    let events = f.repo.events_since(0, 1000).await.unwrap();
    let keys: Vec<_> = events
        .iter()
        .filter_map(|(_, _, _, event)| match event {
            Event::TrackWakeRequested { source, key, .. } if source == "planner_compaction" => {
                Some(key)
            }
            _ => None,
        })
        .collect();
    assert_eq!(keys, ["request"]);
    assert_eq!(f.daemon.compacted_threads_for_test().len(), 1);
}

#[tokio::test]
async fn compact_completed_wake_remains_idempotent_on_restart() {
    use provider::events::PlannerEventKind;
    let f = Fixture::new().await;
    assert_eq!(request(&f).await, CompactionAnswer::Scheduled);
    self_compact::tick(&f.harness.inner, true).await.unwrap();
    let live = f.harness.inner.live_claim.writer();
    for kind in [
        PlannerEventKind::TurnStarted {
            turn_id: "compact-completed-restart".into(),
        },
        PlannerEventKind::TurnCompleted {
            turn: json!({"id":"compact-completed-restart","status":"completed"}),
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
        .await
        .unwrap();
    }
    for _ in 0..2 {
        let restored = reload(&f).await;
        self_compact::tick(&restored.inner, true).await.unwrap();
        persist_snapshot(&restored.inner).await.unwrap();
    }
    let events = f.repo.events_since(0, 1000).await.unwrap();
    let keys: Vec<_> = events
        .iter()
        .filter_map(|(_, _, _, event)| match event {
            Event::TrackWakeRequested { source, key, .. } if source == "planner_compaction" => {
                Some(key)
            }
            _ => None,
        })
        .collect();
    assert_eq!(
        keys,
        ["request"],
        "one wake even after a completed session restarts"
    );
    assert_eq!(f.daemon.compacted_threads_for_test().len(), 1);
}

#[tokio::test]
async fn compact_running_owner_defers_to_pending_ingress_on_ready_tick() {
    use crate::test_seams::{PausePoint, install_pause_for_test};
    let f = Fixture::new().await;
    assert_eq!(request(&f).await, CompactionAnswer::Scheduled);
    let card = f.harness.inner.card_id.as_str();
    let tick = PausePoint::default();
    let dequeued = PausePoint::default();
    install_pause_for_test("planner-compact-tick", card, tick.clone());
    install_pause_for_test("conversation-command-dequeued", card, dequeued.clone());
    let running = PlannerHarness::run(reload_params(&f).await).await.unwrap();
    tokio::time::timeout(Duration::from_secs(5), tick.entered.notified())
        .await
        .expect("running owner selected its ready tick");

    // The tick has won select!, but has not checked admission. Put a real user
    // send in ingress, leaving the folded historical queue empty.
    let ObservationIngress::Running(sender) = &running.inner.observations else {
        panic!("must exercise the running owner");
    };
    let (persisted, confirmation) = oneshot::channel();
    sender
        .try_send(HarnessObservationCommand::Durable {
            deliveries: vec![HarnessObservationDelivery {
                entry: QueueEntry::user_message("pending user input".into(), None, vec![]),
            }],
            key: SendKey::unique_for_test(),
            source: None,
            persisted,
        })
        .unwrap();
    assert!(sender.capacity() < sender.max_capacity());
    assert!(
        running
            .inner
            .historical_queue()
            .unwrap()
            .lock()
            .await
            .is_empty()
    );
    tick.release.notify_one();
    tokio::time::timeout(Duration::from_secs(5), dequeued.entered.notified())
        .await
        .expect("owner drains queued input after the tick");
    let compacted = f.daemon.compacted_threads_for_test();
    let phase = running
        .inner
        .planner_compaction
        .lock()
        .await
        .as_ref()
        .unwrap()
        .phase
        .clone();
    dequeued.release.notify_one();
    tokio::time::timeout(Duration::from_secs(5), confirmation)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    // Stop the fixture owner before assertions, including in the red/mutant run.
    let owner = running.inner.run_loop_task.lock().await.take().unwrap();
    owner.abort();
    let _ = owner.await;
    assert!(
        compacted.is_empty(),
        "pending ingress must precede Planner compaction"
    );
    assert!(matches!(
        phase,
        crate::harness::compact_outcome::PlannerCompactionPhase::Requested
    ));
}

/// Hold a real public checkpoint after its transaction, then let the owner
/// change its record before that checkpoint publishes back into live memory.
async fn checkpoint_during_compact_change(f: &Fixture, change: impl Future<Output = ()>) {
    use crate::test_seams::{PausePoint, install_pause_for_test};
    let before = f.harness.inner.planner_compaction.lock().await.clone();
    let checkpoint = PausePoint::default();
    install_pause_for_test(
        "conversation-input-checkpoint-attempted",
        f.harness.inner.card_id.as_str(),
        checkpoint.clone(),
    );
    let save = f.harness.persist_snapshot();
    tokio::pin!(save);
    tokio::select! {
        result = &mut save => panic!("checkpoint did not pause: {result:?}"),
        _ = checkpoint.entered.notified() => {}
    }
    tokio::pin!(change);
    let changed = async {
        loop {
            let current = f.harness.inner.planner_compaction.lock().await.clone();
            if current != before {
                break current;
            }
            tokio::task::yield_now().await;
        }
    };
    let expected = tokio::select! {
        _ = &mut change => panic!("owner must wait for checkpoint publication"),
        current = tokio::time::timeout(Duration::from_secs(5), changed) => current.unwrap(),
    };
    checkpoint.release.notify_one();
    let (saved, ()) = tokio::join!(save, change);
    saved.unwrap();
    let current = f.harness.inner.planner_compaction.lock().await.clone();
    // The owner's own checkpoint may additionally commit the wake marker.
    assert_eq!(
        current.as_ref().map(|r| (&r.request_id, &r.phase)),
        expected.as_ref().map(|r| (&r.request_id, &r.phase)),
        "a stale public checkpoint must preserve the owner's newer record"
    );
    assert_eq!(
        reload(f)
            .await
            .inner
            .planner_compaction
            .lock()
            .await
            .clone(),
        current
    );
}

#[tokio::test]
async fn compact_public_checkpoint_preserves_concurrent_request_and_settlement() {
    use provider::events::PlannerEventKind;
    let f = Fixture::new().await;
    checkpoint_during_compact_change(&f, async {
        assert_eq!(request(&f).await, CompactionAnswer::Scheduled);
    })
    .await;
    self_compact::tick(&f.harness.inner, true).await.unwrap();
    let live = f.harness.inner.live_claim.writer();
    on_notification(
        &f.harness.inner,
        &live,
        PlannerEvent {
            thread_id: Some("thread-commit-wake".into()),
            kind: PlannerEventKind::TurnStarted {
                turn_id: "compact-race".into(),
            },
        },
    )
    .await
    .unwrap();
    checkpoint_during_compact_change(&f, async {
        on_notification(
            &f.harness.inner,
            &live,
            PlannerEvent {
                thread_id: Some("thread-commit-wake".into()),
                kind: PlannerEventKind::TurnCompleted {
                    turn: json!({"id":"compact-race","status":"completed"}),
                },
            },
        )
        .await
        .unwrap();
    })
    .await;
    assert!(
        f.harness
            .inner
            .planner_compaction
            .lock()
            .await
            .as_ref()
            .unwrap()
            .wake_enqueued
    );
}

#[tokio::test]
async fn compact_replayed_settled_id_never_rearms_live_or_restored_executor() {
    use provider::events::PlannerEventKind;
    let f = Fixture::new().await;
    assert_eq!(request(&f).await, CompactionAnswer::Scheduled);
    self_compact::tick(&f.harness.inner, true).await.unwrap();
    let live = f.harness.inner.live_claim.writer();
    for kind in [
        PlannerEventKind::TurnStarted {
            turn_id: "compact-replay".into(),
        },
        PlannerEventKind::TurnCompleted {
            turn: json!({"id":"compact-replay","status":"completed"}),
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
        .await
        .unwrap();
    }
    let completed = f.harness.inner.planner_compaction.lock().await.clone();
    assert!(completed.as_ref().unwrap().wake_enqueued);
    let restored = reload(&f).await;
    for harness in [&f.harness, &restored] {
        assert_eq!(
            harness.request_compaction("request".into()).await.unwrap(),
            CompactionAnswer::Scheduled
        );
        *harness.inner.compaction_retry_after.lock().await = None;
        self_compact::tick(&harness.inner, true).await.unwrap();
        harness.persist_snapshot().await.unwrap();
        assert_eq!(
            *harness.inner.planner_compaction.lock().await,
            completed,
            "same retained ID must preserve completed maintenance"
        );
    }
    let events = f.repo.events_since(0, 1000).await.unwrap();
    assert_eq!(
        events
            .iter()
            .filter(|(_, _, _, event)| matches!(event,
                Event::TrackWakeRequested { source, .. } if source == "planner_compaction"
            ))
            .count(),
        1
    );
    assert_eq!(f.daemon.compacted_threads_for_test().len(), 1);
}
