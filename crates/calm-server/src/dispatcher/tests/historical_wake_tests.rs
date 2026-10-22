//! Historical Dispatcher delivery through the real PlannerHarness ingress.
use super::*;

#[tokio::test]
async fn dispatcher_historical_wake_out_of_order_preserves_prefix() {
    use crate::db::prelude::*;
    use crate::db::sqlite::{
        SqlxRepo, append_decision_event_in_tx, card_create_with_id_tx, session_start_runtime_tx,
    };
    use crate::harness::queue::{MutationRefused, QueueEntryId, QueueMutation};
    use crate::harness::{
        HarnessConfig, HarnessPhaseTag, HarnessSnapshot, PlannerHarness, PlannerHarnessParams,
    };
    use crate::model::{NewArea, NewCard, NewTrack, new_id, now_ms};
    use crate::session_projection_repo::{
        AgentProvider, WorkerSessionInit, WorkerSessionKind, WorkerSessionState,
    };
    use crate::track_area_cache::TrackAreaCache;

    let repo = Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap());
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

    let mut tx = repo.pool().begin().await.unwrap();
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
        track_delete_locks: Default::default(),
        backend: crate::harness::backend::PlannerBackend::codex(
            SharedCodexAppServer::new_stub(repo.clone()),
            planner_card.id.as_ref(),
            track.id.as_ref(),
            worker_session_id.as_ref(),
        )
        .unwrap(),
        worker_session_id: worker_session_id.clone(),
        track_id: track.id.clone(),
        card_id: planner_card.id.clone(),
        thread_id: Some(thread_id),
        repo: repo.clone(),
        events: events.clone(),
        card_role_cache: role_cache.clone(),
        track_area_cache: track_area_cache.clone(),

        registry: registry.clone(),
        config: HarnessConfig::default(),
        snapshot: (snapshot)
            .try_into()
            .expect("historical fixture snapshot contract"),
    })
    .await
    .expect("historical fixture harness");
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
    let write = WriteContext::new(role_cache, track_area_cache);
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
        canonical_producer: std::sync::RwLock::new(None),
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

    let scope = track_scope(&track.id, &area.id);
    let mut envelopes = Vec::new();
    for key in ["A", "B"] {
        let event = Event::TrackWakeRequested {
            track_id: track.id.clone(),
            source: "objective".into(),
            key: key.into(),
            text: format!("objective notice {key}"),
        };
        let mut tx = repo.pool().begin().await.unwrap();
        let id = append_decision_event_in_tx(&mut tx, &ActorId::Kernel, &scope, None, &event)
            .await
            .unwrap();
        tx.commit().await.unwrap();
        envelopes.push(BroadcastEnvelope {
            id,
            event_version: 1,
            actor: ActorId::Kernel,
            scope: scope.clone(),
            event,
        });
    }
    let b = envelopes.pop().unwrap();
    let b_id = b.id;
    let a = envelopes.pop().unwrap();
    // Force the legal same-track lock acquisition order B, A via the production entry point.
    inner.clone().handle_envelope(b.clone()).await;
    inner.clone().handle_envelope(a).await;
    inner.clone().handle_envelope(b).await;
    assert_eq!(
        handle
            .mutate_pending_entry(
                QueueMutation::Delete {
                    entry_id: QueueEntryId::from_wire("barrier".into()),
                    if_entry_rev: 1
                },
                ActorId::User,
            )
            .await
            .unwrap(),
        Err(MutationRefused::NotFound)
    );
    let snapshot = handle.snapshot().await.unwrap();
    let texts: Vec<_> = snapshot
        .pending_observations()
        .iter()
        .map(|o| o.to_turn_text())
        .collect();
    assert_eq!(
        texts,
        vec![
            "Wake from objective (A): objective notice A",
            "Wake from objective (B): objective notice B"
        ]
    );
    assert_eq!(snapshot.push_watermark, b_id);
    assert_eq!(inner.push_cursor.get(&planner_card.id), b_id);
    handle.shutdown().await.unwrap();
}
