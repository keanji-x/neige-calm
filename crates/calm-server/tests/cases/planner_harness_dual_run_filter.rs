use std::sync::Arc;
use std::time::{Duration, Instant};

use calm_server::card_role_cache::CardRoleCache;
use calm_server::codex_appserver::Notification;
use calm_server::db::prelude::*;
use calm_server::db::sqlite::{SqlxRepo, card_create_with_id_tx, session_start_runtime_tx};
use calm_server::dispatcher::Dispatcher;
use calm_server::event::{EditAuthor, Event, EventBus, EventScope};
use calm_server::harness::{
    HarnessConfig, HarnessPhaseTag, HarnessRegistry, HarnessSnapshot, HarnessState, Observation,
    PlannerHarness, PlannerHarnessParams,
};
use calm_server::ids::ActorId;
use calm_server::model::{CardRole, NewArea, NewCard, NewTrack, new_id, now_ms};
use calm_server::session_projection_repo::{
    AgentProvider, WorkerSessionInit, WorkerSessionKind, WorkerSessionState,
};
use calm_server::shared_codex_appserver::SharedCodexAppServer;
use calm_server::state::{CodexClient, DaemonClient, WriteContext};
use calm_server::terminal_renderer::TerminalRendererRegistry;
use calm_server::track_area_cache::TrackAreaCache;
use serde_json::json;

#[tokio::test]
async fn harness_drops_foreign_thread_notifications() {
    let repo = Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap());
    let area = repo
        .area_create(NewArea {
            name: "dual".into(),
            color: "#111111".into(),
            sort: None,
        })
        .await
        .unwrap();
    let track = repo
        .track_create(NewTrack {
            template_input: None,
            area_id: area.id,
            title: "dual".into(),
            sort: None,
            cwd: "/tmp".into(),
            template_id: None,
            plugin_scope: None,
            attach_folder: false,
            theme: calm_server::routes::theme::RequestTheme::default_dark(),
        })
        .await
        .unwrap();
    let card = repo
        .card_create(NewCard {
            track_id: track.id,
            title: None,
            kind: "codex".into(),
            sort: None,
            payload: json!({"schemaVersion": 1}),
        })
        .await
        .unwrap();
    let thread_b = "thread-harness-b".to_string();
    let runtime_id = new_id();
    let mut snapshot = HarnessSnapshot::initial(0, vec![]);
    snapshot.phase = HarnessPhaseTag::Idle;
    snapshot.last_thread_id = Some(thread_b.clone());
    let mut tx = repo.pool().begin().await.unwrap();
    session_start_runtime_tx(
        &mut tx,
        WorkerSessionInit {
            id: runtime_id.clone(),
            card_id: card.id.to_string(),
            kind: WorkerSessionKind::SharedPlanner,
            agent_provider: Some(AgentProvider::Codex),
            status: WorkerSessionState::Idle,
            terminal_run_id: None,
            thread_id: Some(thread_b.clone()),
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

    let events = EventBus::new();
    let daemon = SharedCodexAppServer::new_fake_running_with_pending(repo.clone(), None);
    let harness = PlannerHarness::run(PlannerHarnessParams {
        worker_session_id: runtime_id,
        track_id: card.track_id,
        card_id: card.id,
        thread_id: Some(thread_b.clone()),
        repo,
        events,
        card_role_cache: CardRoleCache::new(),
        track_area_cache: TrackAreaCache::new(),
        daemon: daemon.clone(),
        config: HarnessConfig::default(),
        snapshot,
    });

    daemon.emit_notification_for_test(Notification::TurnStarted {
        thread_id: "thread-legacy-a".into(),
        turn: json!({ "id": "foreign-turn" }),
    });
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(matches!(harness.state_for_test().await, HarnessState::Idle));

    daemon.emit_notification_for_test(Notification::TurnStarted {
        thread_id: thread_b,
        turn: json!({ "id": "own-turn" }),
    });
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        if matches!(
            harness.state_for_test().await,
            HarnessState::TurnRunning { .. }
        ) {
            break;
        }
        assert!(Instant::now() < deadline);
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    harness.shutdown().await.unwrap();
}

#[tokio::test]
async fn dispatcher_routes_report_edit_to_harness_runtime() {
    let repo = Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap());
    let area = repo
        .area_create(NewArea {
            name: "harness-route".into(),
            color: "#111111".into(),
            sort: None,
        })
        .await
        .unwrap();
    let track = repo
        .track_create(NewTrack {
            template_input: None,
            area_id: area.id.clone(),
            title: "harness-route".into(),
            sort: None,
            cwd: "/tmp".into(),
            template_id: None,
            plugin_scope: None,
            attach_folder: false,
            theme: calm_server::routes::theme::RequestTheme::default_dark(),
        })
        .await
        .unwrap();
    let role_cache = CardRoleCache::new();
    let track_area_cache = TrackAreaCache::new();
    track_area_cache.insert(track.id.clone(), area.id.clone());
    let mut tx = repo.pool().begin().await.unwrap();
    let card = card_create_with_id_tx(
        &mut tx,
        new_id(),
        NewCard {
            track_id: track.id.clone(),
            title: None,
            kind: "codex".into(),
            sort: None,
            payload: json!({"schemaVersion": 1}),
        },
        CardRole::Planner,
        false,
        &role_cache,
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();

    let thread_id = "thread-harness-route".to_string();
    let runtime_id = new_id();
    let mut snapshot = HarnessSnapshot::initial(0, vec![]);
    snapshot.phase = HarnessPhaseTag::Idle;
    snapshot.last_thread_id = Some(thread_id.clone());
    let mut tx = repo.pool().begin().await.unwrap();
    session_start_runtime_tx(
        &mut tx,
        WorkerSessionInit {
            id: runtime_id.clone(),
            card_id: card.id.to_string(),
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

    let events = EventBus::new();
    let repo_dyn: Arc<dyn Repo> = repo.clone();
    let route_repo: Arc<dyn calm_server::db::RouteRepo> = repo.clone();
    let registry = HarnessRegistry::new();
    let daemon = SharedCodexAppServer::new_fake_running_with_pending(repo_dyn.clone(), None);
    let harness = PlannerHarness::run(PlannerHarnessParams {
        worker_session_id: runtime_id.clone(),
        track_id: track.id.clone(),
        card_id: card.id.clone(),
        thread_id: Some(thread_id),
        repo: repo_dyn.clone(),
        events: events.clone(),
        card_role_cache: CardRoleCache::new(),
        track_area_cache: TrackAreaCache::new(),
        daemon: daemon.clone(),
        config: HarnessConfig {
            debounce_min_idle: Duration::from_secs(60),
            debounce_max_wait: Duration::from_secs(60),
            ..HarnessConfig::default()
        },
        snapshot,
    });
    registry.insert(runtime_id.clone(), harness.clone());
    let codex = Arc::new(CodexClient::new_stub());
    let _dispatcher = Dispatcher::spawn_with_terminal_renderer_and_harness(
        repo_dyn.clone(),
        events.clone(),
        WriteContext::new(role_cache.clone(), track_area_cache.clone()),
        codex,
        Arc::new(DaemonClient {
            data_dir: std::env::temp_dir().join("neige-harness-route-dispatcher"),
            proc_supervisor_sock: None,
        }),
        TerminalRendererRegistry::new_with_repo(route_repo),
        None,
        registry.clone(),
        daemon,
        // #1147 S2 — attached fixtures: materialization on lease is a no-op.
        std::env::temp_dir().join("neige-calm-test-unused-workspace-root"),
        4,
    );

    repo.log_pure_event(
        ActorId::User,
        EventScope::Track {
            track: track.id.clone(),
            area: area.id,
        },
        None,
        &events,
        &role_cache,
        &track_area_cache,
        Event::TrackReportEdited {
            track_id: track.id.clone(),
            card_id: card.id.clone(),
            author: EditAuthor::User,
            author_plugin_id: None,
            edit_id: "edit-harness-route".into(),
            summary_before: String::new(),
            summary_after: "summary".into(),
            body_before: String::new(),
            body_after: "body after".into(),
            agent_message: None,
        },
    )
    .await
    .unwrap();

    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        let snapshot = harness.snapshot().await;
        if snapshot.pending_queue.iter().any(|obs| {
            matches!(
                obs,
                Observation::ReportEdited { body, .. } if body == "body after"
            )
        }) {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "timed out waiting for harness report edit; snapshot={snapshot:?}"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    harness.shutdown().await.unwrap();
}

#[tokio::test]
async fn dispatcher_harness_full_queue_retries_without_advancing_cursor() {
    let repo = Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap());
    let area = repo
        .area_create(NewArea {
            name: "harness-full".into(),
            color: "#111111".into(),
            sort: None,
        })
        .await
        .unwrap();
    let track = repo
        .track_create(NewTrack {
            template_input: None,
            area_id: area.id.clone(),
            title: "harness-full".into(),
            sort: None,
            cwd: "/tmp".into(),
            template_id: None,
            plugin_scope: None,
            attach_folder: false,
            theme: calm_server::routes::theme::RequestTheme::default_dark(),
        })
        .await
        .unwrap();
    let role_cache = CardRoleCache::new();
    let track_area_cache = TrackAreaCache::new();
    track_area_cache.insert(track.id.clone(), area.id.clone());
    let mut tx = repo.pool().begin().await.unwrap();
    let card = card_create_with_id_tx(
        &mut tx,
        new_id(),
        NewCard {
            track_id: track.id.clone(),
            title: None,
            kind: "codex".into(),
            sort: None,
            payload: json!({"schemaVersion": 1}),
        },
        CardRole::Planner,
        false,
        &role_cache,
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();

    let thread_id = "thread-harness-full".to_string();
    let runtime_id = new_id();
    let mut snapshot = HarnessSnapshot::initial(0, vec![]);
    snapshot.phase = HarnessPhaseTag::Idle;
    snapshot.last_thread_id = Some(thread_id.clone());
    let mut tx = repo.pool().begin().await.unwrap();
    session_start_runtime_tx(
        &mut tx,
        WorkerSessionInit {
            id: runtime_id.clone(),
            card_id: card.id.to_string(),
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

    let events = EventBus::new();
    let repo_dyn: Arc<dyn Repo> = repo.clone();
    let route_repo: Arc<dyn calm_server::db::RouteRepo> = repo.clone();
    let registry = HarnessRegistry::new();
    let daemon = SharedCodexAppServer::new_fake_running_with_pending(repo_dyn.clone(), None);
    let (harness, mut observations) = PlannerHarness::run_unstarted_for_test(
        PlannerHarnessParams {
            worker_session_id: runtime_id.clone(),
            track_id: track.id.clone(),
            card_id: card.id.clone(),
            thread_id: Some(thread_id),
            repo: repo_dyn.clone(),
            events: events.clone(),
            card_role_cache: CardRoleCache::new(),
            track_area_cache: TrackAreaCache::new(),
            daemon: daemon.clone(),
            config: HarnessConfig::default(),
            snapshot,
        },
        1,
    );
    harness
        .observe(Observation::TrackGoal {
            text: "queue filler".into(),
        })
        .unwrap();
    registry.insert(runtime_id, harness.clone());
    let dispatcher = Dispatcher::spawn_with_terminal_renderer_and_harness(
        repo_dyn.clone(),
        events,
        WriteContext::new(role_cache.clone(), track_area_cache.clone()),
        Arc::new(CodexClient::new_stub()),
        Arc::new(DaemonClient {
            data_dir: std::env::temp_dir().join("neige-harness-full-dispatcher"),
            proc_supervisor_sock: None,
        }),
        TerminalRendererRegistry::new_with_repo(route_repo),
        None,
        registry,
        daemon,
        // #1147 S2 — attached fixtures: materialization on lease is a no-op.
        std::env::temp_dir().join("neige-calm-test-unused-workspace-root"),
        4,
    );

    let event = Event::TrackReportEdited {
        track_id: track.id.clone(),
        card_id: card.id.clone(),
        author: EditAuthor::User,
        author_plugin_id: None,
        edit_id: "edit-harness-full".into(),
        summary_before: String::new(),
        summary_after: "summary".into(),
        body_before: String::new(),
        body_after: "body after".into(),
        agent_message: None,
    };
    let cold_bus = EventBus::new();
    let envelope_id = repo
        .log_pure_event(
            ActorId::User,
            EventScope::Track {
                track: track.id.clone(),
                area: area.id,
            },
            None,
            &cold_bus,
            &role_cache,
            &track_area_cache,
            event.clone(),
        )
        .await
        .unwrap();

    dispatcher
        .catch_up_push(track.id.clone(), event.clone(), envelope_id)
        .await;
    assert_eq!(
        dispatcher.push_cursor_for_test(&card.id),
        0,
        "full live harness queue must not advance the push cursor before retry"
    );
    assert!(matches!(
        observations.recv().await.unwrap().observation,
        Observation::TrackGoal { .. }
    ));

    dispatcher
        .catch_up_push(track.id.clone(), event, envelope_id)
        .await;
    assert_eq!(dispatcher.push_cursor_for_test(&card.id), envelope_id);
    harness.shutdown().await.unwrap();
}
