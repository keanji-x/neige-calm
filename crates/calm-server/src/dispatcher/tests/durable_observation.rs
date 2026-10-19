use super::*;
use crate::db::prelude::*;
use crate::db::sqlite::{
    SqlxRepo, append_decision_event_in_tx, card_create_with_id_tx, session_start_runtime_tx,
};
use crate::harness::{
    HarnessConfig, HarnessPhaseTag, HarnessSnapshot, PlannerHarness, PlannerHarnessParams,
};
use crate::model::{Card, NewArea, NewCard, NewTrack, Track, new_id, now_ms};
use crate::session_projection_repo::{
    AgentProvider, WorkerSessionInit, WorkerSessionKind, WorkerSessionState,
};
use crate::test_seams::{DURABLE_OBSERVATION_STAGED, PausePoint, install_pause_for_test};
use crate::track_area_cache::TrackAreaCache;
use std::time::Duration;

struct Fixture {
    repo: Arc<SqlxRepo>,
    inner: Arc<Inner>,
    handle: PlannerHarness,
    card: Card,
    track: Track,
    runtime: String,
    roles: CardRoleCache,
    _dir: tempfile::TempDir,
    url: String,
}

async fn fixture() -> Fixture {
    let dir = tempfile::TempDir::new().unwrap();
    let url = format!(
        "sqlite://{}?mode=rwc",
        dir.path().join("ack.sqlite").display()
    );
    let repo = Arc::new(SqlxRepo::open(&url).await.unwrap());
    let role_cache = CardRoleCache::new();
    let track_area_cache = TrackAreaCache::new();
    let area = repo
        .area_create(
            calm_types::organization::LEGACY_OWNER_USER_ID,
            NewArea {
                name: "delivery wake".into(),
                color: "#222222".into(),
                sort: None,
            },
        )
        .await
        .unwrap();
    let track = repo
        .track_create(NewTrack {
            template_input: None,
            area_id: area.id.clone(),
            title: "delivery wake".into(),
            sort: None,
            cwd: "/tmp".into(),
            template_id: None,
            plugin_scope: None,
            attach_folder: false,
            theme: crate::routes::theme::RequestTheme::default_dark(),
        })
        .await
        .unwrap();
    track_area_cache.insert(track.id.clone(), area.id.clone());

    let mut tx = crate::db::sqlite::begin_immediate_tx(repo.db())
        .await
        .unwrap();
    let planner_card = card_create_with_id_tx(
        &mut tx,
        new_id(),
        NewCard {
            track_id: track.id.clone(),
            title: None,
            kind: "codex".into(),
            sort: None,
            payload: serde_json::json!({"schemaVersion": 1, "planner_harness": true, "planner_provider": "codex"}),
        },
        CardRole::Planner,
        false,
        &role_cache,
    )
    .await
    .unwrap();
    let worker_session_id = new_id();
    let thread_id = "thread-delivery-wake".to_string();
    let mut snapshot = HarnessSnapshot::initial(0, vec![]);
    snapshot.phase = HarnessPhaseTag::Idle;
    snapshot.last_thread_id = Some(thread_id.clone());
    session_start_runtime_tx(
        &mut tx,
        WorkerSessionInit {
            id: worker_session_id.clone(),
            card_id: planner_card.id.to_string(),
            kind: WorkerSessionKind::SharedPlanner,
            agent_provider: Some(AgentProvider::Codex),
            status: WorkerSessionState::Idle,
            terminal_run_id: None,
            thread_id: Some(thread_id.clone()),
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

    // A live Planner harness in the registry the dispatcher consults; the forced `TurnRunning`
    // phase keeps every delivered observation queued instead of issuing a turn on the stub.
    let events = EventBus::new();
    let registry = HarnessRegistry::new();
    let handle = PlannerHarness::run(PlannerHarnessParams {
        worker_session_id: worker_session_id.clone(),
        track_id: track.id.clone(),
        card_id: planner_card.id.clone(),
        thread_id: Some(thread_id),
        repo: repo.clone(),
        events: events.clone(),
        card_role_cache: role_cache.clone(),
        track_area_cache: track_area_cache.clone(),
        backend: SharedCodexAppServer::new_stub(repo.clone()).into(),
        live_replies: crate::harness::LiveReplies::for_test(),
        config: HarnessConfig::default(),
        snapshot,
    });
    handle
        .force_phase_for_dev(HarnessPhaseTag::TurnRunning)
        .await
        .unwrap();
    let reservation = registry
        .try_reserve(worker_session_id.clone())
        .expect("vacant slot");
    assert!(reservation.install(handle.clone()));

    // The production `Inner` behind `handle_envelope`, minus the bus listener and the periodic
    // loops. The scheduler's operation runtime is dropped, so its poke drives no gate.
    let write = WriteContext::new(role_cache.clone(), track_area_cache);
    let semaphore = Arc::new(Semaphore::new(1));
    let scheduler = Scheduler::new(
        repo.clone(),
        events.clone(),
        write.clone(),
        std::sync::Weak::new(),
        crate::per_card_lock::new_per_card_locks(),
        Arc::clone(&semaphore),
        std::env::temp_dir().join("neige-dispatcher-test-gate-logs"),
        crate::scheduler::WorkerLiveness::DEFAULT,
        crate::scheduler::WorkerIdleWake::new(
            crate::shared_codex_appserver::SharedCodexAppServer::new_stub(repo.clone()),
            crate::scheduler::WORKER_IDLE_TURN_GRACE,
            crate::scheduler::WORKER_IDLE_PROBE_TIMEOUT,
        ),
    );
    let context_monitor = Arc::new(TaskContextMonitor::new_with_metrics(
        repo.clone(),
        events,
        write.clone(),
        scheduler.context_metrics(),
    ));
    let inner = Arc::new(Inner {
        planner_wake: std::sync::RwLock::new(None),
        repo: repo.clone(),
        write,
        harness: registry,
        scheduler,
        context_monitor,
        push_cursor: EventCursorCache::new(),
        push_locks: DashMap::new(),
        failure_push_hook: std::sync::Mutex::new(None),
        semaphore,
    });

    Fixture {
        repo,
        inner,
        handle,
        card: planner_card,
        track,
        runtime: worker_session_id,
        roles: role_cache,
        _dir: dir,
        url,
    }
}

impl Fixture {
    async fn stored(&self) -> HarnessSnapshot {
        let row = self
            .repo
            .session_projection_by_id(&self.runtime)
            .await
            .unwrap()
            .unwrap();
        HarnessSnapshot::from_value_strict(row.handle_state_json.unwrap())
    }

    async fn envelope(&self, text: &str) -> BroadcastEnvelope {
        let event = Event::from_kind_and_payload(
            "forge.pr.checks",
            serde_json::json!({
                "track_id": self.track.id, "pr_number": 42, "conclusion": text,
                "snapshot": {"head_sha":"head", "mergeable":"mergeable"}, "failed_checks":[]
            }),
        )
        .unwrap();
        self.persist(event).await
    }

    async fn persist(&self, event: Event) -> BroadcastEnvelope {
        let scope = track_scope(&self.track.id, &self.track.area_id);
        let mut tx = crate::db::sqlite::begin_immediate_tx(self.repo.db())
            .await
            .unwrap();
        let id = append_decision_event_in_tx(&mut tx, &ActorId::Kernel, &scope, None, &event)
            .await
            .unwrap();
        tx.commit().await.unwrap();
        BroadcastEnvelope {
            id,
            event_version: 1,
            actor: ActorId::Kernel,
            scope,
            event,
        }
    }
}

#[tokio::test]
async fn durable_observation_ack_and_cursor_wait_for_commit_and_retry_is_idempotent() {
    let fx = fixture().await;
    let envelope = fx.envelope("success").await;
    let id = envelope.id;
    let pause = PausePoint::default();
    install_pause_for_test(
        DURABLE_OBSERVATION_STAGED,
        fx.card.id.as_str(),
        pause.clone(),
    );
    let dispatcher = fx.inner.clone();
    let mut push = tokio::spawn(async move { dispatcher.handle_envelope(envelope).await });
    tokio::time::timeout(Duration::from_secs(3), pause.entered.notified())
        .await
        .unwrap();
    assert!(
        tokio::time::timeout(Duration::from_millis(100), &mut push)
            .await
            .is_err()
    );
    assert_eq!(fx.inner.push_cursor.get(&fx.card.id), 0);
    assert_eq!(fx.stored().await.push_watermark, 0);
    // Public snapshot writes also serialize behind the staged command.
    let writer = fx.handle.clone();
    let mut snapshot = tokio::spawn(async move { writer.persist_snapshot().await });
    assert!(
        tokio::time::timeout(Duration::from_millis(100), &mut snapshot)
            .await
            .is_err()
    );
    pause.release.notify_one();
    push.await.unwrap();
    snapshot.await.unwrap().unwrap();
    assert_eq!(fx.inner.push_cursor.get(&fx.card.id), id);
    let stored = fx.stored().await;
    assert_eq!(stored.push_watermark, id);
    assert_eq!(stored.pending_entries().len(), 1);
    let obs = stored.pending_observations().remove(0);
    fx.handle.observe_envelope(obs.clone(), id).await.unwrap();
    assert_eq!(
        fx.stored().await.pending_entries(),
        stored.pending_entries()
    );
    // Simulate a lost ACK/cold dispatcher cursor. SQL floor retains pending <= watermark.
    fx.inner.push_cursor.remove(&fx.card.id);
    let guard = fx.inner.acquire_push_lock(&fx.track.id).await;
    let event = fx.envelope("success").await.event;
    fx.inner
        .observe_harness_under_lock(&guard, &event, id)
        .await;
    assert_eq!(fx.inner.push_cursor.get(&fx.card.id), id);
    assert_eq!(
        fx.stored().await.pending_entries(),
        stored.pending_entries()
    );
    fx.handle.shutdown().await.unwrap();
    assert!(fx.handle.observe_envelope(obs, id + 1).await.is_err());
}

#[tokio::test]
async fn durable_observation_storage_failure_restores_memory_and_replay_cursor() {
    let fx = fixture().await;
    let envelope = fx.envelope("failure").await;
    let id = envelope.id;
    sqlx::query(concat!(
        "CREATE TRIGGER durable_observation_fail ",
        "BEFORE UPDATE OF handle_state_json ON worker_sessions ",
        "BEGIN SELECT RAISE(ABORT, 'durable observation failure'); END",
    ))
    .execute(fx.repo.db().writer_pool())
    .await
    .unwrap();
    fx.inner.clone().handle_envelope(envelope).await;
    assert_eq!(fx.inner.push_cursor.get(&fx.card.id), 0);
    assert_eq!(fx.handle.snapshot().await.push_watermark, 0);
    assert!(fx.handle.snapshot().await.pending_entries().is_empty());
    assert_eq!(fx.stored().await.push_watermark, 0);
    assert!(fx.stored().await.pending_entries().is_empty());
    sqlx::query("DROP TRIGGER durable_observation_fail")
        .execute(fx.repo.db().writer_pool())
        .await
        .unwrap();
    let event = fx.envelope("failure").await.event;
    let guard = fx.inner.acquire_push_lock(&fx.track.id).await;
    fx.inner
        .observe_harness_under_lock(&guard, &event, id)
        .await;
    assert_eq!(fx.inner.push_cursor.get(&fx.card.id), id);
    assert_eq!(fx.stored().await.pending_entries().len(), 1);
    assert_eq!(fx.stored().await.push_watermark, id);
    // If even the persisted snapshot cannot be read, refuse this runtime until recovery.
    // Renaming the joined table makes both the guarded write and outcome read fail in SQL.
    sqlx::query("ALTER TABLE cards RENAME TO durable_observation_cards_hidden")
        .execute(fx.repo.db().writer_pool())
        .await
        .unwrap();
    assert!(
        fx.handle
            .observe_envelope(
                HarnessObservation::TrackGoal {
                    text: "unknown outcome".into()
                },
                id + 1
            )
            .await
            .is_err()
    );
    sqlx::query("ALTER TABLE durable_observation_cards_hidden RENAME TO cards")
        .execute(fx.repo.db().writer_pool())
        .await
        .unwrap();
    assert_eq!(fx.handle.snapshot().await.push_watermark, id);
    assert_eq!(fx.stored().await.push_watermark, id);
    assert_eq!(fx.stored().await.pending_entries().len(), 1);
    assert!(
        fx.handle
            .observe(HarnessObservation::TrackGoal {
                text: "must refuse further writes".into()
            })
            .is_err(),
        "an unreadable persistence outcome must close ingress, not just skip ordinary snapshots"
    );
    fx.handle.shutdown().await.unwrap();
}

#[tokio::test]
async fn durable_observation_rechecks_active_old_pointer_and_sql_role_after_staging() {
    for mismatch in ["pointer", "role", "track", "inactive"] {
        let fx = fixture().await;
        let pause = PausePoint::default();
        install_pause_for_test(
            DURABLE_OBSERVATION_STAGED,
            fx.card.id.as_str(),
            pause.clone(),
        );
        let handle = fx.handle.clone();
        let accepting = tokio::spawn(async move {
            handle
                .observe_envelope(
                    HarnessObservation::TrackGoal {
                        text: "must not commit".into(),
                    },
                    42,
                )
                .await
        });
        tokio::time::timeout(Duration::from_secs(3), pause.entered.notified())
            .await
            .unwrap();
        // Cache deliberately retains Planner. An active historical session is not authority.
        assert_eq!(fx.roles.get(&fx.card.id), Some(CardRole::Planner));
        if mismatch == "track" {
            let other = fx
                .repo
                .track_create(NewTrack {
                    template_input: None,
                    area_id: fx.track.area_id.clone(),
                    title: "other track".into(),
                    sort: None,
                    cwd: "/tmp".into(),
                    template_id: None,
                    plugin_scope: None,
                    attach_folder: false,
                    theme: crate::routes::theme::RequestTheme::default_dark(),
                })
                .await
                .unwrap();
            sqlx::query("UPDATE worker_sessions SET track_id=? WHERE id=?")
                .bind(other.id.as_str())
                .bind(&fx.runtime)
                .execute(fx.repo.db().writer_pool())
                .await
                .unwrap();
        }
        let sql = match mismatch {
            "pointer" => "UPDATE cards SET session_id=NULL WHERE id=?",
            "role" => "UPDATE cards SET role='assistant' WHERE id=?",
            "track" => "UPDATE worker_sessions SET updated_at_ms=updated_at_ms WHERE id=?",
            _ => "UPDATE worker_sessions SET state='failed' WHERE id=?",
        };
        sqlx::query(sql)
            .bind(if matches!(mismatch, "pointer" | "role") {
                fx.card.id.as_str()
            } else {
                &fx.runtime
            })
            .execute(fx.repo.db().writer_pool())
            .await
            .unwrap();
        pause.release.notify_one();
        assert!(accepting.await.unwrap().is_err(), "{mismatch}");
        assert_eq!(fx.handle.snapshot().await.push_watermark, 0, "{mismatch}");
        assert!(
            fx.handle.snapshot().await.pending_entries().is_empty(),
            "{mismatch}"
        );
        let state: String =
            sqlx::query_scalar("SELECT handle_state_json FROM worker_sessions WHERE id=?")
                .bind(&fx.runtime)
                .fetch_one(fx.repo.pool())
                .await
                .unwrap();
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&state).unwrap()["push_watermark"],
            0,
            "{mismatch}"
        );
        fx.handle.shutdown().await.unwrap();
    }
}

#[tokio::test]
async fn durable_observation_lost_ack_retry_retains_pending_below_watermark_after_restart() {
    let fx = fixture().await;
    let pause = PausePoint::default();
    install_pause_for_test(
        DURABLE_OBSERVATION_STAGED,
        fx.card.id.as_str(),
        pause.clone(),
    );
    let handle = fx.handle.clone();
    let delivery = tokio::spawn(async move {
        handle
            .observe_envelope(
                HarnessObservation::TrackGoal {
                    text: "retained".into(),
                },
                42,
            )
            .await
    });
    tokio::time::timeout(Duration::from_secs(3), pause.entered.notified())
        .await
        .unwrap();
    delivery.abort(); // Caller loses its reply; the serial loop still owns the command.
    let _ = delivery.await;
    pause.release.notify_one();
    fx.handle.persist_snapshot().await.unwrap(); // The real staging/checkpoint fence, not a fixture drain.
    let stored = fx.stored().await;
    assert_eq!(stored.push_watermark, 42);
    assert_eq!(stored.pending_entries().len(), 1);
    fx.handle.shutdown().await.unwrap();
    let row = fx
        .repo
        .session_projection_by_id(&fx.runtime)
        .await
        .unwrap()
        .unwrap();
    let restarted = PlannerHarness::run(PlannerHarnessParams {
        worker_session_id: fx.runtime.clone(),
        card_id: fx.card.id.clone(),
        track_id: fx.track.id.clone(),
        thread_id: row.thread_id,
        repo: fx.repo.clone(),
        events: EventBus::new(),
        card_role_cache: fx.roles.clone(),
        track_area_cache: TrackAreaCache::new(),
        backend: SharedCodexAppServer::new_stub(fx.repo.clone()).into(),
        live_replies: crate::harness::LiveReplies::for_test(),
        config: HarnessConfig::default(),
        snapshot: stored.clone(),
    });
    restarted.pause_issuance_for_dev();
    restarted
        .observe_envelope(stored.pending_observations().remove(0), 42)
        .await
        .unwrap();
    assert_eq!(
        fx.stored().await.pending_entries(),
        stored.pending_entries()
    );
    assert_eq!(fx.stored().await.push_watermark, 42);
    restarted.shutdown().await.unwrap();
}

#[tokio::test]
async fn durable_observation_dispatcher_retries_only_the_changed_current_owner() {
    let fx = fixture().await;
    let prefix = fx.envelope("failure").await;
    let envelope = fx.envelope("success").await;
    let id = envelope.id;
    let pause = PausePoint::default();
    install_pause_for_test(
        DURABLE_OBSERVATION_STAGED,
        fx.card.id.as_str(),
        pause.clone(),
    );
    let dispatcher = fx.inner.clone();
    let push = tokio::spawn(async move { dispatcher.handle_envelope(envelope).await });
    tokio::time::timeout(Duration::from_secs(3), pause.entered.notified())
        .await
        .unwrap();
    let successor_id = new_id();
    let mut snapshot = HarnessSnapshot::initial(0, vec![]);
    snapshot.phase = HarnessPhaseTag::Idle;
    let thread = "successor-thread".to_string();
    snapshot.last_thread_id = Some(thread.clone());
    let mut tx = crate::db::sqlite::begin_immediate_tx(fx.repo.db())
        .await
        .unwrap();
    crate::db::sqlite::session_mark_superseded_runtime_tx(&mut tx, &fx.runtime)
        .await
        .unwrap();
    session_start_runtime_tx(
        &mut tx,
        WorkerSessionInit {
            id: successor_id.clone(),
            card_id: fx.card.id.to_string(),
            kind: WorkerSessionKind::SharedPlanner,
            agent_provider: Some(AgentProvider::Codex),
            status: WorkerSessionState::Idle,
            terminal_run_id: None,
            thread_id: Some(thread.clone()),
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
    let successor = PlannerHarness::run(PlannerHarnessParams {
        worker_session_id: successor_id.clone(),
        card_id: fx.card.id.clone(),
        track_id: fx.track.id.clone(),
        thread_id: Some(thread),
        repo: fx.repo.clone(),
        events: EventBus::new(),
        card_role_cache: fx.roles.clone(),
        track_area_cache: TrackAreaCache::new(),
        backend: SharedCodexAppServer::new_stub(fx.repo.clone()).into(),
        live_replies: crate::harness::LiveReplies::for_test(),
        config: HarnessConfig::default(),
        snapshot,
    });
    successor.pause_issuance_for_dev();
    assert!(
        fx.inner
            .harness
            .try_reserve(successor_id.clone())
            .unwrap()
            .install(successor.clone())
    );
    pause.release.notify_one();
    tokio::time::timeout(Duration::from_secs(3), push)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(fx.inner.push_cursor.get(&fx.card.id), id);
    assert_eq!(fx.handle.snapshot().await.push_watermark, 0);
    assert!(fx.handle.snapshot().await.pending_entries().is_empty());
    let row = fx
        .repo
        .session_projection_by_id(&successor_id)
        .await
        .unwrap()
        .unwrap();
    let stored = HarnessSnapshot::from_value_strict(row.handle_state_json.unwrap());
    assert_eq!(stored.push_watermark, id);
    assert_eq!(
        stored
            .pending_entries()
            .iter()
            .filter_map(|e| e.envelope_id())
            .collect::<Vec<_>>(),
        vec![prefix.id, id],
    );
    fx.handle.shutdown().await.unwrap();
    successor.shutdown().await.unwrap();
}

#[tokio::test]
async fn durable_observation_failed_hook_restores_dedupe_and_filtered_retry_consumes_envelope() {
    let fx = fixture().await;
    let hook = HarnessObservation::WorkerHookStop {
        track_id: fx.track.id.clone(),
        card_id: fx.card.id.clone(),
        kind: calm_types::observation::HookKind::CodexStop,
        idempotency_key: "same-hook".into(),
    };
    sqlx::query(concat!(
        "CREATE TRIGGER durable_observation_hook_failure ",
        "BEFORE UPDATE OF handle_state_json ON worker_sessions ",
        "BEGIN SELECT RAISE(ABORT, 'hook failure'); END",
    ))
    .execute(fx.repo.db().writer_pool())
    .await
    .unwrap();
    assert!(fx.handle.observe_envelope(hook.clone(), 40).await.is_err());
    assert!(fx.handle.snapshot().await.pending_entries().is_empty());
    assert_eq!(fx.handle.snapshot().await.push_watermark, 0);
    sqlx::query("DROP TRIGGER durable_observation_hook_failure")
        .execute(fx.repo.db().writer_pool())
        .await
        .unwrap();
    fx.handle.observe_envelope(hook.clone(), 40).await.unwrap();
    assert_eq!(
        fx.stored().await.pending_entries().len(),
        1,
        "failed staging cannot leave a dedupe key"
    );
    fx.handle.observe_envelope(hook, 41).await.unwrap();
    assert_eq!(
        fx.stored().await.pending_entries().len(),
        1,
        "existing hook filtering is preserved"
    );
    assert_eq!(
        fx.stored().await.push_watermark,
        41,
        "filtered envelope is consumed durably"
    );
    fx.handle.shutdown().await.unwrap();
}

mod prefix_protocol;
