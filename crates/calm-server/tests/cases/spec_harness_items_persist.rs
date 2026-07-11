use std::sync::Arc;
use std::time::{Duration, Instant};

use calm_server::codex_appserver::Notification;
use calm_server::db::prelude::*;
use calm_server::db::sqlite::{SqlxRepo, session_start_runtime_tx};
use calm_server::event::{BroadcastEnvelope, Event, EventBus, EventScope};
use calm_server::harness::{
    HarnessConfig, HarnessPhaseTag, HarnessSnapshot, HarnessState, SpecHarness, SpecHarnessParams,
};
use calm_server::ids::ActorId;
use calm_server::model::{NewCard, NewCove, NewWave, new_id, now_ms};
use calm_server::session_projection_repo::{
    AgentProvider, WorkerSessionInit, WorkerSessionKind, WorkerSessionState,
};
use calm_server::shared_codex_appserver::SharedCodexAppServer;
use serde_json::{Value, json};

async fn seed_harness(
    repo: Arc<SqlxRepo>,
    events: EventBus,
) -> (SpecHarness, Arc<SharedCodexAppServer>, String, String) {
    let cove = repo
        .cove_create(NewCove {
            name: "items-persist".into(),
            color: "#111111".into(),
            sort: None,
        })
        .await
        .unwrap();
    let wave = repo
        .wave_create(NewWave {
            workflow_input: None,
            cove_id: cove.id.clone(),
            title: "items persist".into(),
            sort: None,
            cwd: "/tmp".into(),
            workflow_id: None,
            attach_folder: false,
            theme: calm_server::routes::theme::RequestTheme::default_dark(),
        })
        .await
        .unwrap();
    let card = repo
        .card_create(NewCard {
            wave_id: wave.id.clone(),
            kind: "codex".into(),
            sort: None,
            payload: json!({"schemaVersion": 1, "spec_harness": true}),
        })
        .await
        .unwrap();
    let runtime_id = new_id();
    let thread_id = "thread-items-persist".to_string();
    let mut snapshot = HarnessSnapshot::initial(0, vec![]);
    snapshot.phase = HarnessPhaseTag::Idle;
    snapshot.last_thread_id = Some(thread_id.clone());

    let mut tx = repo.pool().begin().await.unwrap();
    session_start_runtime_tx(
        &mut tx,
        WorkerSessionInit {
            id: runtime_id.clone(),
            card_id: card.id.to_string(),
            kind: WorkerSessionKind::SharedSpec,
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

    let daemon = SharedCodexAppServer::new_fake_running_with_pending(repo.clone(), None);
    let repo_dyn: Arc<dyn Repo> = repo.clone();
    let wave_cove_cache = calm_server::wave_cove_cache::WaveCoveCache::new();
    wave_cove_cache.insert(wave.id.clone(), cove.id);
    let harness = SpecHarness::run(SpecHarnessParams {
        runtime_id,
        wave_id: card.wave_id.clone(),
        card_id: card.id.clone(),
        thread_id: Some(thread_id),
        repo: repo_dyn,
        events,
        card_role_cache: calm_server::card_role_cache::CardRoleCache::new(),
        wave_cove_cache,
        daemon: daemon.clone(),
        config: HarnessConfig {
            debounce_min_idle: Duration::from_secs(60),
            debounce_max_wait: Duration::from_secs(60),
            ..HarnessConfig::default()
        },
        snapshot,
    });

    (
        harness,
        daemon,
        card.id.to_string(),
        card.wave_id.to_string(),
    )
}

async fn wait_for_rows(
    repo: &SqlxRepo,
    card_id: &str,
    count: usize,
) -> Vec<calm_server::model::HarnessItem> {
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        let rows = repo
            .harness_item_list_by_card(card_id, 0, 100, false)
            .await
            .unwrap();
        if rows.len() == count {
            return rows;
        }
        assert!(
            Instant::now() < deadline,
            "timed out waiting for {count} harness item rows; got {}",
            rows.len()
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

async fn wait_for_notification_receiver(daemon: &SharedCodexAppServer) {
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        if daemon.notification_receiver_count_for_test() > 0 {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "timed out waiting for harness notification receiver"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

async fn recv_item_event(
    rx: &mut tokio::sync::broadcast::Receiver<calm_server::event::BroadcastEnvelope>,
) -> BroadcastEnvelope {
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        assert!(
            !remaining.is_zero(),
            "timed out waiting for harness item event"
        );
        let env = tokio::time::timeout(remaining, rx.recv())
            .await
            .expect("event timeout")
            .expect("event receive");
        if matches!(env.event, Event::HarnessItemAdded { .. }) {
            assert_eq!(env.actor, ActorId::Kernel);
            assert_ne!(env.id, 0, "HarnessItemAdded must carry a durable events.id");
            return env;
        }
    }
}

async fn recv_phase_event(
    rx: &mut tokio::sync::broadcast::Receiver<calm_server::event::BroadcastEnvelope>,
) -> BroadcastEnvelope {
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        assert!(
            !remaining.is_zero(),
            "timed out waiting for harness phase event"
        );
        let env = tokio::time::timeout(remaining, rx.recv())
            .await
            .expect("event timeout")
            .expect("event receive");
        if matches!(env.event, Event::HarnessPhaseChanged { .. }) {
            assert_eq!(env.actor, ActorId::Kernel);
            assert_ne!(
                env.id, 0,
                "HarnessPhaseChanged must carry a durable events.id"
            );
            return env;
        }
    }
}

#[tokio::test]
async fn item_notification_persists_row_and_emits_event() {
    let repo = Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap());
    let events = EventBus::new();
    let mut rx = events.subscribe();
    let (harness, daemon, card_id, wave_id) = seed_harness(repo.clone(), events).await;
    wait_for_notification_receiver(&daemon).await;

    daemon.emit_notification_for_test(Notification::Item {
        method: "item/completed".into(),
        params: json!({
            "threadId": "thread-items-persist",
            "turn": { "id": "turn-items-1" },
            "item": {
                "id": "item-agent-1",
                "type": "agent_message",
                "text": "persisted assistant text"
            }
        }),
    });

    let rows = wait_for_rows(&repo, &card_id, 1).await;
    let row = &rows[0];
    assert_eq!(row.card_id.as_str(), card_id);
    assert_eq!(row.wave_id.as_str(), wave_id);
    assert_eq!(row.thread_id, "thread-items-persist");
    assert_eq!(row.turn_id.as_deref(), Some("turn-items-1"));
    assert_eq!(row.item_uuid.as_deref(), Some("item-agent-1"));
    assert_eq!(row.item_type.as_deref(), Some("agent_message"));
    assert_eq!(row.method, "item/completed");
    let params: Value = serde_json::from_str(&row.params).unwrap();
    assert_eq!(params["item"]["text"], "persisted assistant text");

    let envelope = recv_item_event(&mut rx).await;
    let event_id = envelope.id;
    let event_scope = envelope.scope.clone();
    match envelope.event {
        Event::HarnessItemAdded {
            card_id: event_card_id,
            wave_id: event_wave_id,
            item_db_id,
            item_uuid,
            item_type,
            turn_id,
            method,
            ..
        } => {
            assert_eq!(event_card_id.as_str(), card_id);
            assert_eq!(event_wave_id.as_str(), wave_id);
            assert_eq!(item_db_id, row.id);
            assert_eq!(item_uuid.as_deref(), Some("item-agent-1"));
            assert_eq!(item_type.as_deref(), Some("agent_message"));
            assert_eq!(turn_id.as_deref(), Some("turn-items-1"));
            assert_eq!(method, "item/completed");
        }
        other => panic!("expected HarnessItemAdded, got {other:?}"),
    }
    assert!(
        matches!(
            event_scope,
            EventScope::Card { ref card, ref wave, .. }
                if card.as_str() == card_id && wave.as_str() == wave_id
        ),
        "HarnessItemAdded envelope must be card-scoped, got {event_scope:?}"
    );

    let events = repo.events_since(0, i64::MAX).await.unwrap();
    let durable_item_event_id = events
        .iter()
        .find_map(|(id, _version, _scope, event)| match event {
            Event::HarnessItemAdded { item_db_id, .. } if *item_db_id == row.id => Some(*id),
            _ => None,
        })
        .expect("HarnessItemAdded row must exist in events_since");
    assert_ne!(durable_item_event_id, 0);
    assert_eq!(durable_item_event_id, event_id);

    harness.shutdown().await.unwrap();
}

#[tokio::test]
async fn phase_log_failure_keeps_last_phase_for_retry() {
    let repo = Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap());
    let events = EventBus::new();
    let (harness, _daemon, card_id, wave_id) = seed_harness(repo.clone(), events).await;

    harness
        .set_state_for_test(HarnessState::TurnRunning {
            turn_id: "turn-rollback".into(),
            started_at: Instant::now(),
        })
        .await;

    sqlx::query("ALTER TABLE events RENAME TO events_broken")
        .execute(repo.pool())
        .await
        .unwrap();
    let err = harness
        .persist_snapshot()
        .await
        .expect_err("missing events table should fail phase event persistence");
    assert!(
        err.to_string().contains("events"),
        "expected events-table failure, got {err}"
    );
    sqlx::query("ALTER TABLE events_broken RENAME TO events")
        .execute(repo.pool())
        .await
        .unwrap();

    harness.persist_snapshot().await.unwrap();
    let events = repo.events_since(0, i64::MAX).await.unwrap();
    assert!(
        events.iter().any(|(_id, _version, _scope, event)| matches!(
            event,
            Event::HarnessPhaseChanged {
                card_id: event_card_id,
                wave_id: event_wave_id,
                old_phase: HarnessPhaseTag::Idle,
                new_phase: HarnessPhaseTag::TurnRunning,
                ..
            } if event_card_id.as_str() == card_id && event_wave_id.as_str() == wave_id
        )),
        "retry must persist Idle -> TurnRunning after first log failure: {events:?}"
    );

    harness.shutdown().await.unwrap();
}

#[tokio::test]
async fn phase_transition_persists_row_and_emits_durable_event_id() {
    let repo = Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap());
    let events = EventBus::new();
    let mut rx = events.subscribe();
    let (harness, daemon, card_id, wave_id) = seed_harness(repo.clone(), events).await;
    wait_for_notification_receiver(&daemon).await;

    daemon.emit_notification_for_test(Notification::TurnStarted {
        thread_id: "thread-items-persist".into(),
        turn: json!({ "id": "turn-phase-1" }),
    });

    let envelope = recv_phase_event(&mut rx).await;
    let event_id = envelope.id;
    match envelope.event {
        Event::HarnessPhaseChanged {
            card_id: event_card_id,
            wave_id: event_wave_id,
            old_phase,
            new_phase,
            ..
        } => {
            assert_eq!(event_card_id.as_str(), card_id);
            assert_eq!(event_wave_id.as_str(), wave_id);
            assert_eq!(old_phase, HarnessPhaseTag::Idle);
            assert_eq!(new_phase, HarnessPhaseTag::TurnRunning);
        }
        other => panic!("expected HarnessPhaseChanged, got {other:?}"),
    }

    let events = repo.events_since(0, i64::MAX).await.unwrap();
    let durable_phase_event_id = events
        .iter()
        .find_map(|(id, _version, _scope, event)| match event {
            Event::HarnessPhaseChanged {
                card_id: event_card_id,
                old_phase,
                new_phase,
                ..
            } if event_card_id.as_str() == card_id
                && *old_phase == HarnessPhaseTag::Idle
                && *new_phase == HarnessPhaseTag::TurnRunning =>
            {
                Some(*id)
            }
            _ => None,
        })
        .expect("HarnessPhaseChanged row must exist in events_since");
    assert_ne!(durable_phase_event_id, 0);
    assert_eq!(durable_phase_event_id, event_id);

    harness.shutdown().await.unwrap();
}
