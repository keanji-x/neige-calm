//! Exercise the production enqueue, replay, persistence and turn issuance paths
//! with an unstarted harness so the test controls the exact delivery boundary.
use super::*;
use crate::db::prelude::*;
use crate::db::sqlite::{
    SqlxRepo, append_decision_event_in_tx, card_create_with_id_tx, session_start_runtime_tx,
};
use crate::model::{
    CardRole, NewArea, NewCard, NewTrack, TrackLifecycle, TrackPatch, new_id, now_ms,
};
use crate::session_projection_repo::{
    AgentProvider, WorkerSessionInit, WorkerSessionKind, WorkerSessionState,
};
use serde_json::json;

struct Fixture {
    repo: Arc<SqlxRepo>,
    harness: PlannerHarness,
    _ingress: mpsc::Receiver<HarnessObservationDelivery>,
}

impl Fixture {
    async fn new() -> Self {
        let repo = Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap());
        let area = repo
            .area_create(NewArea {
                name: "completed commit wake".into(),
                color: "#111111".into(),
                sort: None,
            })
            .await
            .unwrap();
        let track = repo
            .track_create(NewTrack {
                template_input: None,
                area_id: area.id.clone(),
                title: "commit wake".into(),
                sort: None,
                cwd: "/tmp".into(),
                template_id: None,
                plugin_scope: None,
                attach_folder: false,
                theme: crate::routes::theme::RequestTheme::default_dark(),
            })
            .await
            .unwrap();
        let roles = CardRoleCache::new();
        let areas = TrackAreaCache::new();
        areas.insert(track.id.clone(), area.id);
        let mut tx = repo.pool().begin().await.unwrap();
        let card = card_create_with_id_tx(
            &mut tx,
            new_id(),
            NewCard {
                track_id: track.id.clone(),
                title: None,
                kind: "codex".into(),
                sort: None,
                payload: json!({"schemaVersion":1,"planner_harness":true}),
            },
            CardRole::Planner,
            false,
            &roles,
        )
        .await
        .unwrap();
        let worker_session_id = new_id();
        let mut snapshot = HarnessSnapshot::initial(0, vec![]);
        snapshot.phase = HarnessPhaseTag::Idle;
        snapshot.last_thread_id = Some("thread-commit-wake".into());
        session_start_runtime_tx(
            &mut tx,
            WorkerSessionInit {
                id: worker_session_id.clone(),
                card_id: card.id.to_string(),
                kind: WorkerSessionKind::SharedPlanner,
                agent_provider: Some(AgentProvider::Codex),
                status: WorkerSessionState::Idle,
                terminal_run_id: None,
                thread_id: snapshot.last_thread_id.clone(),
                session_id: None,
                active_turn_id: None,
                handle_state_json: Some(serde_json::to_value(&snapshot).unwrap()),
                spawn_op_id: None,
                now_ms: now_ms(),
            },
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();
        let daemon = SharedCodexAppServer::new_fake_running_with_pending(repo.clone(), None);
        let (harness, ingress) = PlannerHarness::run_unstarted_for_test(
            PlannerHarnessParams {
                worker_session_id,
                track_id: track.id,
                card_id: card.id,
                thread_id: snapshot.last_thread_id.clone(),
                repo: repo.clone(),
                events: EventBus::new(),
                card_role_cache: roles,
                track_area_cache: areas,
                daemon,
                config: HarnessConfig {
                    debounce_min_idle: Duration::ZERO,
                    debounce_max_wait: Duration::ZERO,
                    ..HarnessConfig::default()
                },
                snapshot,
            },
            8,
        );
        Self {
            repo,
            harness,
            _ingress: ingress,
        }
    }

    async fn lifecycle(&self, lifecycle: TrackLifecycle) {
        self.repo
            .track_update(
                self.harness.inner.track_id.as_str(),
                TrackPatch {
                    lifecycle: Some(lifecycle),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
    }

    async fn commit(&self) -> (i64, QueueEntry) {
        let inner = &self.harness.inner;
        let event = Event::WorktreeCommitted {
            track_id: inner.track_id.clone(),
            card_id: inner.card_id.clone(),
            commit_sha: "retained-commit".into(),
            branch: "completed-slice".into(),
        };
        let scope = harness_event_scope(inner, "worktree.committed");
        let mut tx = self.repo.pool().begin().await.unwrap();
        let id =
            append_decision_event_in_tx(&mut tx, &ActorId::KernelDispatcher, &scope, None, &event)
                .await
                .unwrap();
        tx.commit().await.unwrap();
        let observation = crate::dispatcher::resolve_harness_observation(
            self.repo.as_ref(),
            &inner.track_id,
            &event,
        )
        .await
        .unwrap()
        .unwrap();
        (id, QueueEntry::system(observation, Some(id)).unwrap())
    }

    async fn enqueue(&self, entries: Vec<QueueEntry>) {
        self.harness.observe_durable_entries(entries).await.unwrap();
    }

    async fn stored(&self) -> HarnessSnapshot {
        let row = self
            .repo
            .session_projection_by_id(&self.harness.inner.worker_session_id)
            .await
            .unwrap()
            .unwrap();
        serde_json::from_value(row.handle_state_json.unwrap()).unwrap()
    }

    async fn issue(&self) {
        maybe_issue_turn(&self.harness.inner).await.unwrap();
    }
}

#[tokio::test]
async fn queued_commit_before_done_is_consumed_without_a_turn_and_later_user_input_still_issues() {
    let fx = Fixture::new().await;
    fx.lifecycle(TrackLifecycle::Working).await;
    let (event_id, commit) = fx.commit().await;
    fx.enqueue(vec![commit]).await;
    assert_eq!(fx.stored().await.pending_entries().len(), 1);
    // This is the observed failure: the event existed while work was active,
    // but the next turn can be issued only after the Planner has marked Done.
    fx.lifecycle(TrackLifecycle::Done).await;
    fx.issue().await;
    assert_eq!(fx.harness.inner.daemon.turn_start_count_for_test(), 0);
    let stored = fx.stored().await;
    assert!(stored.pending_entries().is_empty());
    assert_eq!(stored.push_watermark, event_id);
    assert_eq!(stored.phase, HarnessPhaseTag::Idle);
    assert_eq!(
        fx.repo
            .events_for_track(
                fx.harness.inner.track_id.as_str(),
                &["worktree.committed"],
                None
            )
            .await
            .unwrap()
            .len(),
        1
    );
    // No stuck debounce/state after consuming the only soft observation.
    fx.harness
        .observe_user_message_durable("please explain the result".into(), vec![])
        .await
        .unwrap();
    fx.issue().await;
    assert_eq!(fx.harness.inner.daemon.turn_start_count_for_test(), 1);
}

#[tokio::test]
async fn replayed_commit_after_done_is_consumed_and_does_not_replay_again() {
    let fx = Fixture::new().await;
    fx.lifecycle(TrackLifecycle::Done).await;
    let (event_id, _) = fx.commit().await;
    let mut snapshot = fx.stored().await;
    crate::harness::replay_harness_events_since(
        fx.repo.clone(),
        fx.harness.inner.card_id.as_str(),
        &fx.harness.inner.track_id,
        snapshot.push_watermark,
        &mut snapshot,
    )
    .await
    .unwrap();
    assert_eq!(snapshot.pending_entries().len(), 1);
    assert_eq!(snapshot.push_watermark, event_id);
    // Rehydrate through the same constructor used by a recovered harness.
    let inner = &fx.harness.inner;
    let (recovered, _receiver) = PlannerHarness::run_unstarted_for_test(
        PlannerHarnessParams {
            worker_session_id: inner.worker_session_id.clone(),
            track_id: inner.track_id.clone(),
            card_id: inner.card_id.clone(),
            thread_id: snapshot.last_thread_id.clone(),
            repo: fx.repo.clone(),
            events: inner.events.clone(),
            card_role_cache: inner.card_role_cache.clone(),
            track_area_cache: inner.track_area_cache.clone(),
            daemon: inner.daemon.clone(),
            config: inner.config,
            snapshot,
        },
        8,
    );
    maybe_issue_turn(&recovered.inner).await.unwrap();
    assert_eq!(inner.daemon.turn_start_count_for_test(), 0);
    let mut stored = fx.stored().await;
    assert!(stored.pending_entries().is_empty());
    assert_eq!(stored.push_watermark, event_id);
    crate::harness::replay_harness_events_since(
        fx.repo.clone(),
        inner.card_id.as_str(),
        &inner.track_id,
        stored.push_watermark,
        &mut stored,
    )
    .await
    .unwrap();
    assert!(stored.pending_entries().is_empty());
}

#[tokio::test]
async fn completed_commit_filter_keeps_failure_and_user_entries_in_order() {
    let fx = Fixture::new().await;
    fx.lifecycle(TrackLifecycle::Done).await;
    let (event_id, commit) = fx.commit().await;
    let failure = QueueEntry::system(
        Observation::TaskFailed {
            idempotency_key: "still-actionable".into(),
            error: "commit operation failed".into(),
        },
        None,
    )
    .unwrap();
    let user = QueueEntry::user_message("investigate that failure".into(), None, vec![]);
    let expected =
        input_segments_for_entries(&fx.harness.inner.card_id, &[failure.clone(), user.clone()]);
    fx.enqueue(vec![commit, failure, user]).await;
    fx.issue().await;
    let stored = fx.stored().await;
    assert_eq!(
        stored.issued_input_segments.as_ref().unwrap().segments,
        expected
    );
    assert!(stored.pending_entries().is_empty());
    assert_eq!(stored.push_watermark, event_id);
    assert_eq!(fx.harness.inner.daemon.turn_start_count_for_test(), 1);
}

#[tokio::test]
async fn commit_notifications_still_issue_for_every_non_done_lifecycle() {
    for lifecycle in [
        TrackLifecycle::Draft,
        TrackLifecycle::Planning,
        TrackLifecycle::Dispatching,
        TrackLifecycle::Working,
        TrackLifecycle::Reviewing,
        TrackLifecycle::Blocked,
        TrackLifecycle::Canceled,
        TrackLifecycle::Failed,
    ] {
        let fx = Fixture::new().await;
        fx.lifecycle(lifecycle).await;
        let (_, commit) = fx.commit().await;
        let expected =
            input_segments_for_entries(&fx.harness.inner.card_id, std::slice::from_ref(&commit));
        fx.enqueue(vec![commit]).await;
        fx.issue().await;
        assert_eq!(
            fx.harness.inner.daemon.turn_start_count_for_test(),
            1,
            "{lifecycle:?}"
        );
        assert_eq!(
            fx.stored().await.issued_input_segments.unwrap().segments,
            expected,
            "{lifecycle:?}"
        );
    }
}

#[tokio::test]
async fn completed_commit_consumption_restores_queue_and_debounce_on_persist_failure() {
    let fx = Fixture::new().await;
    fx.lifecycle(TrackLifecycle::Done).await;
    let (_, commit) = fx.commit().await;
    fx.enqueue(vec![commit]).await;
    let before = fx.harness.snapshot().await;
    let debounce = *fx.harness.inner.debounce.lock().await;
    sqlx::query("CREATE TRIGGER refuse_consumption BEFORE UPDATE OF handle_state_json ON worker_sessions BEGIN SELECT RAISE(ABORT, 'injected snapshot failure'); END")
        .execute(fx.repo.pool()).await.unwrap();
    let error = maybe_issue_turn(&fx.harness.inner).await.unwrap_err();
    assert!(
        error.to_string().contains("injected snapshot failure"),
        "{error}"
    );
    assert_eq!(
        fx.harness.snapshot().await.pending_entries(),
        before.pending_entries()
    );
    assert_eq!(
        fx.stored().await.pending_entries(),
        before.pending_entries()
    );
    assert_eq!(fx.harness.snapshot().await.phase, HarnessPhaseTag::Idle);
    let after_debounce = *fx.harness.inner.debounce.lock().await;
    assert_eq!(after_debounce.first_pending_at, debounce.first_pending_at);
    assert_eq!(after_debounce.last_pending_at, debounce.last_pending_at);
    assert_eq!(after_debounce.hard_fire, debounce.hard_fire);
    sqlx::query("DROP TRIGGER refuse_consumption")
        .execute(fx.repo.pool())
        .await
        .unwrap();
    fx.issue().await;
    assert!(fx.stored().await.pending_entries().is_empty());
    assert_eq!(fx.harness.inner.daemon.turn_start_count_for_test(), 0);
}
