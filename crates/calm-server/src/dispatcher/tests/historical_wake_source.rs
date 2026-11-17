//! Original historical Assistant Wake through Dispatcher, answer transaction and boot replay.
use super::*;
use crate::db::prelude::*;
use crate::db::sqlite::{
    SqlxRepo, append_decision_event_in_tx, card_create_with_id_tx, session_start_runtime_tx,
};
use crate::harness::{
    HarnessConfig, HarnessPhaseTag, HarnessSnapshot, PlannerHarness, PlannerHarnessParams,
};
use crate::model::{NewArea, NewCard, NewTrack, now_ms};
use crate::session_projection_repo::{AgentProvider, WorkerSessionInit, WorkerSessionState};
use crate::track_area_cache::TrackAreaCache;

#[tokio::test]
async fn historical_assistant_wake_only_original_session_live_and_catch_up() {
    assert_source_delivery(true).await;
}

#[tokio::test]
async fn historical_assistant_wake_cold_catch_up_does_not_reach_planner() {
    assert_source_delivery(false).await;
}

async fn assert_source_delivery(live: bool) {
    let repo = Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap());
    let area = repo
        .area_create(
            calm_types::organization::LEGACY_OWNER_USER_ID,
            NewArea {
                name: "wake source".into(),
                color: "red".into(),
                sort: None,
            },
        )
        .await
        .unwrap();
    let track = repo
        .track_create(NewTrack {
            template_input: None,
            area_id: area.id.clone(),
            title: "wake source".into(),
            sort: None,
            cwd: "/tmp".into(),
            template_id: None,
            plugin_scope: None,
            attach_folder: false,
            theme: crate::routes::theme::RequestTheme::default_dark(),
        })
        .await
        .unwrap();
    let role_cache = CardRoleCache::new();
    let track_area_cache = TrackAreaCache::new();
    track_area_cache.insert(track.id.clone(), area.id.clone());
    let registry = HarnessRegistry::new();
    let events = EventBus::new();
    let mut handles = Vec::new();
    for (card, session, role) in [
        ("planner", "planner-old", CardRole::Planner),
        ("assistant", "assistant-old", CardRole::Assistant),
    ] {
        let mut snapshot = HarnessSnapshot::initial(0, vec![]);
        snapshot.phase = HarnessPhaseTag::Idle;
        snapshot.last_thread_id = Some(format!("thread-{session}"));
        let init = if role == CardRole::Planner {
            WorkerSessionInit::shared_planner(
                session.into(),
                card.into(),
                AgentProvider::Codex,
                WorkerSessionState::Idle,
                snapshot.last_thread_id.clone(),
                serde_json::to_value(&snapshot).unwrap(),
                now_ms(),
            )
        } else {
            WorkerSessionInit::managed_assistant(
                session.into(),
                card.into(),
                AgentProvider::Codex,
                WorkerSessionState::Idle,
                snapshot.last_thread_id.clone(),
                serde_json::to_value(&snapshot).unwrap(),
                now_ms(),
            )
        };
        crate::db::write_in_tx_typed(repo.as_ref(), {
            let roles = role_cache.clone(); let track = track.id.clone();
            move |tx| Box::pin(async move {
                card_create_with_id_tx(tx,card.into(),NewCard {track_id:track, title:None, kind:"codex".into(),sort:None,
                    payload:if role==CardRole::Planner {serde_json::json!({"planner_provider":"codex","permission_mode":"never"})} else {serde_json::json!({"harness_profile":"assistant","assistant_provider":"codex","permission_mode":"never"})}},role,true,&roles).await?;
                session_start_runtime_tx(tx,init).await?; Ok(())
            })
        }).await.unwrap();
        let handle = PlannerHarness::run(PlannerHarnessParams {
            track_delete_locks: Default::default(),
            backend: crate::harness::backend::PlannerBackend::codex(
                SharedCodexAppServer::new_stub(repo.clone()),
                card,
                track.id.as_str(),
                session,
            )
            .unwrap(),
            worker_session_id: session.into(),
            track_id: track.id.clone(),
            card_id: card.into(),
            thread_id: snapshot.last_thread_id.clone(),
            repo: repo.clone(),
            events: events.clone(),
            card_role_cache: role_cache.clone(),
            track_area_cache: track_area_cache.clone(),
            registry: registry.clone(),
            config: HarnessConfig::default(),
            snapshot: snapshot.try_into().unwrap(),
        })
        .await
        .unwrap();
        handle
            .force_phase_for_dev(HarnessPhaseTag::TurnRunning)
            .await
            .unwrap();
        assert!(
            registry
                .try_reserve(session.into())
                .unwrap()
                .install(handle.clone())
        );
        handles.push(handle);
    }
    // The production `Inner` behind `handle_envelope`, minus the bus listener and the periodic
    // loops. The scheduler's operation runtime is dropped, so its poke drives no gate.
    let write = WriteContext::new(role_cache.clone(), track_area_cache.clone());
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
        events.clone(),
        write.clone(),
        scheduler.context_metrics(),
    ));
    let inner = Arc::new(Inner {
        planner_wake: std::sync::RwLock::new(None),
        canonical_producer: std::sync::RwLock::new(None),
        repo: repo.clone(),
        write,
        harness: registry.clone(),
        scheduler,
        context_monitor,
        push_cursor: EventCursorCache::new(),
        push_locks: DashMap::new(),
        failure_push_hook: std::sync::Mutex::new(None),
        semaphore,
    });

    assert!(
        crate::planner_session::historical_session(repo.as_ref(), &"planner".into())
            .await
            .unwrap()
            .is_some(),
        "fixture must have a real historical Planner control"
    );
    for session in ["planner-old", "assistant-old"] {
        let row = repo.session_get(&session.into()).await.unwrap().unwrap();
        assert!(
            crate::harness::profile::live_binding(repo.as_ref(), &row)
                .await
                .unwrap()
                .is_some(),
            "fixture session {session} must be active and current"
        );
    }

    // Keep the real Assistant identity through the shared production ask/answer factories.
    let identity = crate::mcp_server::ToolCallIdentity {
        card_id: "assistant".into(),
        role: CardRole::Assistant,
        provider: AgentProvider::Codex,
        session_id: "assistant-old".into(),
        track_id: Some(track.id.to_string()),
        area_id: area.id.to_string(),
        thread_id: "card-bound".into(),
    };
    // The shared factory and source owner are also the provider Wake entry point.
    let ask_id = crate::db::write_in_tx_typed(repo.as_ref(), {
        let identity = identity.clone();
        move |tx| {
            Box::pin(async move {
                let (scope, event) = crate::ask::ask_requested_tx(
                    tx,
                    &identity.card_id.clone().into(),
                    vec![crate::event::AskQuestion {
                        title: "Original question".into(),
                        detail: None,
                        options: vec!["Allow".into(), "Deny".into()],
                    }],
                )
                .await?;
                Ok(
                    append_decision_event_in_tx(tx, &identity.to_actor_id(), &scope, None, &event)
                        .await?,
                )
            })
        }
    })
    .await
    .unwrap();
    let envelope = crate::db::write_in_tx_typed(repo.as_ref(), {
        let track = track.id.clone();
        move |tx| {
            Box::pin(async move {
                let (scope, events) = crate::ask::ask_answered_tx(
                    tx,
                    &track,
                    ask_id,
                    vec![crate::event::AskAnswer::Option(0)],
                )
                .await?;
                let event = events.into_iter().last().unwrap();
                let id =
                    append_decision_event_in_tx(tx, &ActorId::User, &scope, None, &event).await?;
                Ok(BroadcastEnvelope {
                    id,
                    event_version: 1,
                    actor: ActorId::User,
                    scope,
                    event,
                })
            })
        }
    })
    .await
    .unwrap();
    if !live {
        let replay = crate::harness::catch_up::observations_since(
            repo.as_ref(),
            &track.id,
            &"planner".into(),
            "planner-old",
            0,
            None,
        )
        .await
        .unwrap();
        assert!(
            replay.is_empty(),
            "Planner cold recovery must not receive an Assistant answer: {replay:?}"
        );
    }
    if live {
        inner.clone().handle_envelope(envelope.clone()).await;
        inner.clone().handle_envelope(envelope.clone()).await;
        drain_observations(&handles).await;
        let assistant = handles[1].snapshot().await.unwrap();
        assert_eq!(
            assistant.pending_observations().len(),
            1,
            "original Assistant receives exactly one answer"
        );
        assert!(
            assistant.pending_observations()[0]
                .to_turn_text()
                .contains("Original question")
        );
        assert!(
            handles[0]
                .snapshot()
                .await
                .unwrap()
                .pending_observations()
                .is_empty()
        );
    }
    let assistant = handles[1].snapshot().await.unwrap();
    let planner_event = Event::TrackWakeRequested {
        track_id: track.id.clone(),
        source: "objective".into(),
        key: "later".into(),
        text: "later notice".into(),
    };
    let scope = track_scope(&track.id, &area.id);
    let id = crate::db::write_in_tx_typed(repo.as_ref(), {
        let event = planner_event.clone();
        let scope = scope.clone();
        move |tx| {
            Box::pin(async move {
                Ok(append_decision_event_in_tx(tx, &ActorId::Kernel, &scope, None, &event).await?)
            })
        }
    })
    .await
    .unwrap();
    inner
        .clone()
        .handle_envelope(BroadcastEnvelope {
            id,
            event_version: 1,
            actor: ActorId::Kernel,
            scope,
            event: planner_event,
        })
        .await;
    drain_observations(&handles).await;
    let planner = handles[0].snapshot().await.unwrap();
    assert_eq!(
        planner.pending_observations().len(),
        1,
        "later Planner prefix cannot inherit Assistant answer"
    );
    assert!(!matches!(
        planner.pending_observations()[0],
        HarnessObservation::AskAnswered { .. }
    ));
    for (card, session, watermark, expected) in [
        ("planner", "planner-old", 0, 1),
        ("assistant", "assistant-old", 0, 1),
        (
            "assistant",
            "assistant-old",
            if live { assistant.push_watermark } else { id },
            0,
        ),
    ] {
        let replay = crate::harness::catch_up::observations_since(
            repo.as_ref(),
            &track.id,
            &card.into(),
            session,
            watermark,
            None,
        )
        .await
        .unwrap();
        assert_eq!(
            replay.len(),
            expected,
            "restart replay for {session} at {watermark}"
        );
    }
    // Retained source history must never target the replacement session of the same card.
    crate::db::write_in_tx_typed(repo.as_ref(), move |tx| {
        Box::pin(async move {
            crate::db::sqlite::session_supersede_and_start_tx(
                tx,
                &"assistant-old".into(),
                WorkerSessionInit::managed_assistant(
                    "assistant-new".into(),
                    "assistant".into(),
                    AgentProvider::Codex,
                    WorkerSessionState::Idle,
                    None,
                    serde_json::to_value(HarnessSnapshot::initial(0, vec![])).unwrap(),
                    now_ms(),
                ),
            )
            .await?;
            Ok(())
        })
    })
    .await
    .unwrap();
    inner.clone().handle_envelope(envelope).await;
    let replay = crate::harness::catch_up::observations_since(
        repo.as_ref(),
        &track.id,
        &"assistant".into(),
        "assistant-new",
        0,
        None,
    )
    .await
    .unwrap();
    assert!(replay.is_empty(), "new session cannot inherit old answer");
    for handle in handles {
        handle.shutdown().await.unwrap();
    }
}

// Like the existing historical prefix fixture, await a command on the same ingress
// before reading a snapshot. snapshot() itself does not drain observe_envelope().
async fn drain_observations(handles: &[PlannerHarness]) {
    use crate::harness::queue::{MutationRefused, QueueEntryId, QueueMutation};
    for handle in handles {
        assert_eq!(
            handle
                .mutate_pending_entry(
                    QueueMutation::Delete {
                        entry_id: QueueEntryId::from_wire("barrier".into()),
                        if_entry_rev: 1,
                    },
                    ActorId::User
                )
                .await
                .unwrap(),
            Err(MutationRefused::NotFound)
        );
    }
}
