use std::sync::Arc;
use std::time::{Duration, Instant};

use calm_server::codex_appserver::{InputItem, Notification};
use calm_server::db::prelude::*;
use calm_server::db::sqlite::{SqlxRepo, session_start_runtime_tx};
use calm_server::event::{BroadcastEnvelope, Event, EventBus, EventScope};
use calm_server::harness::{
    HarnessConfig, HarnessPhaseTag, HarnessSnapshot, HarnessState, Observation, PlannerHarness,
    PlannerHarnessParams, QueueEntry,
};
use calm_server::ids::ActorId;
use calm_server::model::{HarnessInputPresentation, NewArea, NewCard, NewTrack, new_id, now_ms};
use calm_server::session_projection_repo::{
    AgentProvider, WorkerSessionInit, WorkerSessionKind, WorkerSessionState,
};
use calm_server::shared_codex_appserver::SharedCodexAppServer;
use serde_json::{Value, json};

/// The thread id every harness in this file is seeded with. `on_notification` silently drops any frame whose
/// `threadId` does not match, surfacing only as a `wait_for_rows` timeout; fixtures substitute this constant in.
const SEED_THREAD_ID: &str = "thread-items-persist";

async fn seed_harness(
    repo: Arc<SqlxRepo>,
    events: EventBus,
) -> (PlannerHarness, Arc<SharedCodexAppServer>, String, String) {
    seed_harness_with_pending(repo, events, vec![]).await
}

async fn seed_harness_with_pending(
    repo: Arc<SqlxRepo>,
    events: EventBus,
    pending: Vec<Observation>,
) -> (PlannerHarness, Arc<SharedCodexAppServer>, String, String) {
    let area = repo
        .area_create(NewArea {
            name: "items-persist".into(),
            color: "#111111".into(),
            sort: None,
        })
        .await
        .unwrap();
    let track = repo
        .track_create(NewTrack {
            template_input: None,
            area_id: area.id.clone(),
            title: "items persist".into(),
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
            track_id: track.id.clone(),
            title: None,
            kind: "codex".into(),
            sort: None,
            payload: json!({"schemaVersion": 1, "planner_harness": true, "planner_provider": "codex"}),
        })
        .await
        .unwrap();
    let runtime_id = new_id();
    let thread_id = SEED_THREAD_ID.to_string();
    let mut snapshot =
        HarnessSnapshot::initial(0, QueueEntry::entries_from_observations_for_test(pending));
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

    let daemon = SharedCodexAppServer::new_fake_running_with_pending(repo.clone(), None);
    let repo_dyn: Arc<dyn Repo> = repo.clone();
    let track_area_cache = calm_server::track_area_cache::TrackAreaCache::new();
    track_area_cache.insert(track.id.clone(), area.id);
    let harness = PlannerHarness::run(PlannerHarnessParams {
        worker_session_id: runtime_id,
        track_id: card.track_id.clone(),
        card_id: card.id.clone(),
        thread_id: Some(thread_id),
        repo: repo_dyn,
        events,
        card_role_cache: calm_server::card_role_cache::CardRoleCache::new(),
        track_area_cache,
        backend: daemon.clone().into(),
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
        card.track_id.to_string(),
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
    let (harness, daemon, card_id, track_id) = seed_harness(repo.clone(), events).await;
    wait_for_notification_receiver(&daemon).await;

    daemon.emit_notification_for_test(Notification::Item {
        method: "item/completed".into(),
        params: json!({
            "threadId": SEED_THREAD_ID,
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
    assert_eq!(row.track_id.as_str(), track_id);
    assert_eq!(row.thread_id, SEED_THREAD_ID);
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
            track_id: event_track_id,
            item_db_id,
            item_uuid,
            item_type,
            turn_id,
            method,
            ..
        } => {
            assert_eq!(event_card_id.as_str(), card_id);
            assert_eq!(event_track_id.as_str(), track_id);
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
            EventScope::Card { ref card, ref track, .. }
                if card.as_str() == card_id && track.as_str() == track_id
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

/// The segments of a mixed batch land on the projection row the drain writes; codex's echo upgrades that row and never adds a second one.
#[tokio::test]
async fn issued_mixed_observation_segments_are_on_the_projection_row_and_survive_the_echo() {
    let repo = Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap());
    let events = EventBus::new();
    let pending = vec![
        Observation::TaskCompleted {
            idempotency_key: "task-completed".into(),
            result: json!({"status": "ok"}),
        },
        Observation::UserMessage {
            text: "what changed?".into(),
        },
        Observation::TaskFailed {
            idempotency_key: "task-failed".into(),
            error: "boom".into(),
        },
    ];
    let (harness, daemon, card_id, _track_id) =
        seed_harness_with_pending(repo.clone(), events, pending).await;
    wait_for_notification_receiver(&daemon).await;

    let deadline = Instant::now() + Duration::from_secs(2);
    let issued_text = loop {
        let turns = daemon.started_turns_for_test();
        if let Some((_thread_id, input)) = turns.first()
            && let Some(InputItem::Text { text }) = input.first()
        {
            break text.clone();
        }
        assert!(
            Instant::now() < deadline,
            "timed out waiting for the observation batch to issue"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    };
    let client_id = daemon.started_turn_client_ids_for_test()[0]
        .clone()
        .expect("the drain sends clientUserMessageId");

    // The projection row is written before `turn/start`.
    let rows = wait_for_rows(&repo, &card_id, 1).await;
    let projection = &rows[0];
    assert_eq!(projection.item_type.as_deref(), Some("userMessage"));
    assert_eq!(projection.method, "item/completed");
    assert_eq!(projection.turn_id, None);
    assert_eq!(projection.item_uuid.as_deref(), Some(client_id.as_str()));
    let issued_segments = projection
        .input_segments
        .clone()
        .expect("the projection carries the batch's segments");
    assert_eq!(
        issued_segments
            .iter()
            .map(|segment| segment.presentation)
            .collect::<Vec<_>>(),
        vec![
            HarnessInputPresentation::SystemTaskCompleted,
            HarnessInputPresentation::User,
            HarnessInputPresentation::SystemTaskFailed,
        ]
    );
    assert_eq!(
        issued_text,
        issued_segments
            .iter()
            .map(|segment| segment.text.as_str())
            .collect::<Vec<_>>()
            .join("\n"),
        "the structured segments must be the exact strings flattened for Codex"
    );
    let issued_turn_id = "fake-turn-0001";

    // An echo that names no projection is stored as its own row and inherits nothing.
    daemon.emit_notification_for_test(Notification::Item {
        method: "item/completed".into(),
        params: json!({
            "threadId": SEED_THREAD_ID,
            "turn": { "id": "foreign-turn" },
            "item": {
                "id": "item-user-foreign-turn",
                "type": "userMessage",
                "content": [{ "type": "text", "text": issued_text.clone() }]
            }
        }),
    });
    let rows = wait_for_rows(&repo, &card_id, 2).await;
    assert_eq!(
        rows[1].input_segments, None,
        "a late or foreign user-message must not inherit the batch's provenance"
    );

    daemon.emit_notification_for_test(Notification::Item {
        method: "item/completed".into(),
        params: json!({
            "threadId": SEED_THREAD_ID,
            "turn": { "id": issued_turn_id },
            "item": {
                "id": "item-user-structured-source",
                "clientId": client_id,
                "type": "userMessage",
                "content": [{ "type": "text", "text": issued_text.clone() }]
            }
        }),
    });
    let deadline = Instant::now() + Duration::from_secs(2);
    let rows = loop {
        let rows = repo
            .harness_item_list_by_card(&card_id, 0, 100, false)
            .await
            .unwrap();
        if rows[0].turn_id.is_some() {
            break rows;
        }
        assert!(
            Instant::now() < deadline,
            "timed out waiting for the echo to upgrade the projection row"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    };
    assert_eq!(rows.len(), 2, "the echo upgrades; it never appends");
    assert_eq!(rows[0].id, projection.id);
    assert_eq!(rows[0].turn_id.as_deref(), Some(issued_turn_id));
    assert_eq!(
        rows[0].item_uuid.as_deref(),
        Some("item-user-structured-source")
    );
    assert_eq!(rows[0].input_segments, Some(issued_segments));
    let params: Value = serde_json::from_str(&rows[0].params).unwrap();
    assert_eq!(
        params["item"]["content"][0]["text"], issued_text,
        "provenance belongs in its own column; the upstream frame stays verbatim"
    );
    assert_eq!(
        params.get("_projection"),
        None,
        "codex's frame replaces the kernel-written one wholesale"
    );

    harness.shutdown().await.unwrap();
}

/// The `turn/plan/updated` payload, hand-authored from codex upstream `rust-v0.151.0`; see the `_provenance` block inside the file.
const PLAN_FIXTURE: &str = include_str!("../fixtures/turn_plan_updated.json");

#[tokio::test]
async fn turn_plan_updated_persists_rows_without_events() {
    let fixture: Value = serde_json::from_str(PLAN_FIXTURE).unwrap();
    let mut params = fixture
        .get("params")
        .expect("fixture must carry the wire params under `params`")
        .clone();
    // The fixture's `threadId` is a placeholder; a mismatch would be dropped silently by `on_notification`'s prologue.
    params["threadId"] = json!(SEED_THREAD_ID);

    let repo = Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap());
    let events = EventBus::new();
    let mut rx = events.subscribe();
    let (harness, daemon, card_id, track_id) = seed_harness(repo.clone(), events).await;
    wait_for_notification_receiver(&daemon).await;

    daemon.emit_notification_for_test(Notification::Other {
        method: "turn/plan/updated".into(),
        params: params.clone(),
    });

    let rows = wait_for_rows(&repo, &card_id, 1).await;
    let row = &rows[0];
    assert_eq!(row.card_id.as_str(), card_id);
    assert_eq!(row.track_id.as_str(), track_id);
    assert_eq!(row.thread_id, SEED_THREAD_ID);
    assert_eq!(row.method, "turn/plan/updated");
    // `turnId` is top-level on a plan, not under `turn.id`.
    assert_eq!(row.turn_id.as_deref(), Some("turn-plan-1"));
    assert_eq!(row.item_uuid, None, "a plan is not an item and has no id");
    assert_eq!(
        row.item_type, None,
        "item_type MUST stay null: a plan is not an item, so it has no item \
         type, and writing one would state something untrue about the row"
    );
    // Not a byte-level claim: the frame is a `serde_json::Value` before the kernel sees it and is re-serialized on the way to the DB.
    assert_eq!(row.params, serde_json::to_string(&params).unwrap());
    let stored: Value = serde_json::from_str(&row.params).unwrap();
    assert_eq!(stored, params);
    // The camelCase `inProgress` spelling is stored as sent.
    assert_eq!(
        stored["plan"].as_array().map(Vec::len),
        Some(3),
        "every plan entry must be stored, unfiltered"
    );
    assert_eq!(stored["plan"][0]["status"], "completed");
    assert_eq!(stored["plan"][1]["status"], "inProgress");
    assert_eq!(stored["plan"][2]["status"], "pending");
    assert_eq!(
        stored["plan"][1]["step"],
        "Add the column plus a backfill migration"
    );
    assert!(stored["explanation"].is_string());

    // The snake_case `turn_id` spelling pins which extractor the plan arm uses: `item_turn_id` accepts it, `other_turn_id` does not.
    daemon.emit_notification_for_test(Notification::Other {
        method: "turn/plan/updated".into(),
        params: json!({
            "threadId": SEED_THREAD_ID,
            "turn_id": "turn-plan-2",
            "explanation": null,
            "plan": [ { "step": "second frame", "status": "pending" } ]
        }),
    });
    let rows = wait_for_rows(&repo, &card_id, 2).await;
    assert_eq!(
        rows[1].turn_id.as_deref(),
        Some("turn-plan-2"),
        "the plan arm must read turn ids through `item_turn_id`, which accepts \
         the snake_case `turn_id` spelling"
    );
    let plan_row_ids = [rows[0].id, rows[1].id];

    // A plan row emits NO event: no UI reads plan rows. Fenced against a race rather than a sleep: a real item is
    // sent last, so if a plan had emitted an event it would arrive first.
    daemon.emit_notification_for_test(Notification::Item {
        method: "item/completed".into(),
        params: json!({
            "threadId": SEED_THREAD_ID,
            "turn": { "id": "turn-items-1" },
            "item": { "id": "item-agent-1", "type": "agent_message", "text": "after the plan" }
        }),
    });
    let rows = wait_for_rows(&repo, &card_id, 3).await;
    let item_row = &rows[2];
    let envelope = recv_item_event(&mut rx).await;
    match envelope.event {
        Event::HarnessItemAdded {
            item_db_id, method, ..
        } => {
            assert_eq!(
                method, "item/completed",
                "the first HarnessItemAdded must be the real item's — a plan row must emit none"
            );
            assert_eq!(item_db_id, item_row.id);
        }
        other => panic!("expected HarnessItemAdded, got {other:?}"),
    }

    let durable = repo.events_since(0, i64::MAX).await.unwrap();
    let stray: Vec<i64> = durable
        .iter()
        .filter_map(|(_id, _version, _scope, event)| match event {
            Event::HarnessItemAdded { item_db_id, .. } if plan_row_ids.contains(item_db_id) => {
                Some(*item_db_id)
            }
            _ => None,
        })
        .collect();
    assert!(
        stray.is_empty(),
        "plan rows must not be event-sourced; got HarnessItemAdded for rows {stray:?}"
    );

    harness.shutdown().await.unwrap();
}

#[tokio::test]
async fn phase_log_failure_does_not_reject_or_erase_durable_input() {
    let repo = Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap());
    let events = EventBus::new();
    let (harness, _daemon, card_id, track_id) = seed_harness(repo.clone(), events).await;

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
    harness
        .observe_user_message_durable("survive audit outage".into(), Vec::new())
        .await
        .expect("the snapshot commit accepts input even when its follow-up phase audit fails");
    let stored: Value = sqlx::query_scalar(
        "SELECT handle_state_json FROM worker_sessions WHERE card_id=?1 ORDER BY created_at_ms DESC LIMIT 1",
    )
    .bind(&card_id)
    .fetch_one(repo.pool())
    .await
    .unwrap();
    let stored = HarnessSnapshot::from_value_strict(stored);
    assert!(
        stored
            .pending_observations()
            .iter()
            .any(|observation| matches!(
                observation,
                Observation::UserMessage { text } if text == "survive audit outage"
            )),
        "durably accepted input must remain in the committed snapshot"
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
                track_id: event_track_id,
                old_phase: HarnessPhaseTag::Idle,
                new_phase: HarnessPhaseTag::TurnRunning,
                ..
            } if event_card_id.as_str() == card_id && event_track_id.as_str() == track_id
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
    let (harness, daemon, card_id, track_id) = seed_harness(repo.clone(), events).await;
    wait_for_notification_receiver(&daemon).await;

    daemon.emit_notification_for_test(Notification::TurnStarted {
        thread_id: SEED_THREAD_ID.into(),
        turn: json!({ "id": "turn-phase-1" }),
    });

    let envelope = recv_phase_event(&mut rx).await;
    let event_id = envelope.id;
    match envelope.event {
        Event::HarnessPhaseChanged {
            card_id: event_card_id,
            track_id: event_track_id,
            old_phase,
            new_phase,
            ..
        } => {
            assert_eq!(event_card_id.as_str(), card_id);
            assert_eq!(event_track_id.as_str(), track_id);
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

/// A failed `turn/completed`, hand-authored from the codex 0.153.4 schema; see the `_provenance` block inside the file.
const TURN_COMPLETED_FAILED_FIXTURE: &str = include_str!("../fixtures/turn_completed_failed.json");

/// Driven through the real harness: `TurnStarted` moves the FSM to `TurnRunning`, then the fixture's `TurnCompleted { status: failed }` lands in the arm that writes the row.
#[tokio::test]
async fn turn_completed_failed_persists_outcome_row_readable_by_transcript() {
    let fixture: Value = serde_json::from_str(TURN_COMPLETED_FAILED_FIXTURE).unwrap();
    let turn = fixture["params"]["turn"].clone();
    let turn_id = turn["id"].as_str().unwrap().to_string();

    let repo = Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap());
    let events = EventBus::new();
    let (harness, daemon, card_id, track_id) = seed_harness(repo.clone(), events).await;
    wait_for_notification_receiver(&daemon).await;

    daemon.emit_notification_for_test(Notification::TurnStarted {
        thread_id: SEED_THREAD_ID.into(),
        turn: json!({ "id": turn_id }),
    });
    daemon.emit_notification_for_test(Notification::TurnCompleted {
        thread_id: SEED_THREAD_ID.into(),
        turn: turn.clone(),
    });

    let rows = wait_for_rows(&repo, &card_id, 1).await;
    let row = &rows[0];
    assert_eq!(row.card_id.as_str(), card_id);
    assert_eq!(row.track_id.as_str(), track_id);
    assert_eq!(row.thread_id, SEED_THREAD_ID);
    assert_eq!(row.method, "turn/completed");
    assert_eq!(row.turn_id.as_deref(), Some(turn_id.as_str()));
    assert_eq!(
        row.item_uuid, None,
        "a turn is not an item and has no item id"
    );
    assert_eq!(
        row.item_type, None,
        "a turn is not an item and has no item type"
    );
    assert_eq!(row.input_segments, None);

    // `params` is the codex `turn` object minus `items` / `itemsView`.
    let stored: Value = serde_json::from_str(&row.params).unwrap();
    let mut expected = turn.clone();
    let object = expected.as_object_mut().unwrap();
    object.remove("items");
    object.remove("itemsView");
    assert_eq!(stored, expected);
    assert_eq!(stored["status"], "failed");
    assert_eq!(
        stored["error"]["message"],
        "The conversation exceeded the model's context window."
    );
    assert_eq!(stored["error"]["codexErrorInfo"], "contextWindowExceeded");
    assert_eq!(stored["durationMs"], 42000);
    assert!(stored.get("items").is_none(), "items are rows of their own");
    assert!(stored.get("itemsView").is_none());

    // The read that `TRANSCRIPT_METHOD_PREDICATE` narrows in SQL.
    let transcript = repo
        .harness_item_list_transcript_by_card(&card_id, 0, 100, false)
        .await
        .unwrap();
    assert_eq!(
        transcript.iter().map(|r| r.id).collect::<Vec<_>>(),
        vec![row.id],
        "the transcript predicate must allow 'turn/completed'"
    );

    let snapshot = harness.snapshot().await;
    assert_eq!(snapshot.phase, HarnessPhaseTag::TurnCompleted);

    harness.shutdown().await.unwrap();
}

/// Fenced against a race rather than a sleep: the stale frame goes first; had it written a row, `wait_for_rows(.., 1)` would return that one instead.
#[tokio::test]
async fn stale_turn_completed_writes_no_outcome_row() {
    let repo = Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap());
    let events = EventBus::new();
    let (harness, daemon, card_id, _track_id) = seed_harness(repo.clone(), events).await;
    wait_for_notification_receiver(&daemon).await;

    // Harness is `Idle`: no turn is running, so this completion is stale.
    daemon.emit_notification_for_test(Notification::TurnCompleted {
        thread_id: SEED_THREAD_ID.into(),
        turn: json!({ "id": "turn-stale", "status": "interrupted", "items": [] }),
    });
    daemon.emit_notification_for_test(Notification::TurnStarted {
        thread_id: SEED_THREAD_ID.into(),
        turn: json!({ "id": "turn-real" }),
    });
    daemon.emit_notification_for_test(Notification::TurnCompleted {
        thread_id: SEED_THREAD_ID.into(),
        turn: json!({ "id": "turn-someone-else", "status": "completed", "items": [] }),
    });
    daemon.emit_notification_for_test(Notification::TurnCompleted {
        thread_id: SEED_THREAD_ID.into(),
        turn: json!({ "id": "turn-real", "status": "interrupted", "items": [] }),
    });

    let rows = wait_for_rows(&repo, &card_id, 1).await;
    assert_eq!(rows[0].method, "turn/completed");
    assert_eq!(rows[0].turn_id.as_deref(), Some("turn-real"));
    let stored: Value = serde_json::from_str(&rows[0].params).unwrap();
    assert_eq!(stored["status"], "interrupted");
    assert!(stored.get("error").is_none());

    harness.shutdown().await.unwrap();
}

/// Record, for every event row, how many `turn/completed` transcript rows were durable at the instant that event was
/// written. A trigger reads inside the database at the exact statement, so the answer does not depend on scheduling.
async fn record_outcome_rows_at_each_event(repo: &SqlxRepo) {
    sqlx::query(
        "CREATE TABLE outcome_rows_at_event (event_id INTEGER PRIMARY KEY, outcome_rows INTEGER NOT NULL)",
    )
    .execute(repo.pool())
    .await
    .unwrap();
    sqlx::query(
        "CREATE TRIGGER record_outcome_rows_at_event AFTER INSERT ON events BEGIN \
           INSERT INTO outcome_rows_at_event (event_id, outcome_rows) VALUES (\
             NEW.id, (SELECT COUNT(*) FROM harness_items WHERE method = 'turn/completed')\
           ); \
         END",
    )
    .execute(repo.pool())
    .await
    .unwrap();
}

/// The count `record_outcome_rows_at_each_event` captured for one event.
async fn outcome_rows_at_event(repo: &SqlxRepo, event_id: i64) -> i64 {
    sqlx::query_scalar("SELECT outcome_rows FROM outcome_rows_at_event WHERE event_id = ?1")
        .bind(event_id)
        .fetch_one(repo.pool())
        .await
        .unwrap()
}

/// Receive phase events until the one that lands on `new_phase`.
async fn recv_phase_event_into(
    rx: &mut tokio::sync::broadcast::Receiver<calm_server::event::BroadcastEnvelope>,
    expected_new_phase: HarnessPhaseTag,
) -> BroadcastEnvelope {
    loop {
        let envelope = recv_phase_event(rx).await;
        if matches!(
            envelope.event,
            Event::HarnessPhaseChanged { new_phase, .. } if new_phase == expected_new_phase
        ) {
            return envelope;
        }
    }
}

#[tokio::test]
async fn outcome_row_is_durable_before_the_phase_event_that_announces_it() {
    let repo = Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap());
    let events = EventBus::new();
    let mut rx = events.subscribe();
    let (harness, daemon, card_id, _track_id) = seed_harness(repo.clone(), events).await;
    wait_for_notification_receiver(&daemon).await;
    record_outcome_rows_at_each_event(&repo).await;

    daemon.emit_notification_for_test(Notification::TurnStarted {
        thread_id: SEED_THREAD_ID.into(),
        turn: json!({ "id": "turn-order" }),
    });
    let running = recv_phase_event_into(&mut rx, HarnessPhaseTag::TurnRunning).await;
    daemon.emit_notification_for_test(Notification::TurnCompleted {
        thread_id: SEED_THREAD_ID.into(),
        turn: json!({ "id": "turn-order", "status": "completed", "items": [] }),
    });
    let completed = recv_phase_event_into(&mut rx, HarnessPhaseTag::TurnCompleted).await;
    assert!(matches!(
        completed.event,
        Event::HarnessPhaseChanged {
            old_phase: HarnessPhaseTag::TurnRunning,
            ..
        }
    ));

    // The probe is live and discriminating: nothing at the turn's start …
    assert_eq!(outcome_rows_at_event(&repo, running.id).await, 0);
    // … and the row already there when the turn's end is made durable.
    assert_eq!(
        outcome_rows_at_event(&repo, completed.id).await,
        1,
        "the turn/completed row must be durable before the TurnRunning -> TurnCompleted event \
         is written: that event is the only thing telling a client to fetch the row"
    );
    let rows = wait_for_rows(&repo, &card_id, 1).await;
    assert_eq!(rows[0].turn_id.as_deref(), Some("turn-order"));

    harness.shutdown().await.unwrap();
}

/// codex answers an interrupt with `turn/completed` carrying `status: "interrupted"`, a different branch of the
/// `TurnCompleted` arm. Notifications are handled in order by one loop, so once the target's row exists the
/// non-target frame has been fully processed.
#[tokio::test]
async fn interrupt_target_completion_writes_outcome_row() {
    let repo = Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap());
    let events = EventBus::new();
    let mut rx = events.subscribe();
    let (harness, daemon, card_id, _track_id) = seed_harness(repo.clone(), events).await;
    wait_for_notification_receiver(&daemon).await;
    record_outcome_rows_at_each_event(&repo).await;

    daemon.emit_notification_for_test(Notification::TurnStarted {
        thread_id: SEED_THREAD_ID.into(),
        turn: json!({ "id": "turn-int" }),
    });
    recv_phase_event_into(&mut rx, HarnessPhaseTag::TurnRunning).await;
    harness.interrupt("user".into()).await.unwrap();
    recv_phase_event_into(&mut rx, HarnessPhaseTag::IssuingInterrupt).await;
    assert_eq!(
        daemon.interrupted_turns_for_test(),
        vec![(SEED_THREAD_ID.to_string(), "turn-int".to_string())],
        "the Stop path must have asked codex to interrupt the running turn"
    );

    // A non-target completion while the interrupt is pending must leave no row.
    daemon.emit_notification_for_test(Notification::TurnCompleted {
        thread_id: SEED_THREAD_ID.into(),
        turn: json!({ "id": "turn-other", "status": "completed", "items": [] }),
    });
    daemon.emit_notification_for_test(Notification::TurnCompleted {
        thread_id: SEED_THREAD_ID.into(),
        turn: json!({ "id": "turn-int", "status": "interrupted", "items": [] }),
    });
    let completed = recv_phase_event_into(&mut rx, HarnessPhaseTag::TurnCompleted).await;
    assert!(matches!(
        completed.event,
        Event::HarnessPhaseChanged {
            old_phase: HarnessPhaseTag::IssuingInterrupt,
            ..
        }
    ));

    let rows = wait_for_rows(&repo, &card_id, 1).await;
    assert_eq!(rows[0].method, "turn/completed");
    assert_eq!(rows[0].turn_id.as_deref(), Some("turn-int"));
    assert!(
        rows.iter()
            .all(|row| row.turn_id.as_deref() != Some("turn-other")),
        "the non-target completion must not have written a row"
    );
    let stored: Value = serde_json::from_str(&rows[0].params).unwrap();
    assert_eq!(stored["status"], "interrupted");
    assert!(stored.get("error").is_none());
    assert_eq!(
        outcome_rows_at_event(&repo, completed.id).await,
        1,
        "the interrupted turn's row must be durable before the IssuingInterrupt -> TurnCompleted \
         event is written"
    );
    assert_eq!(
        harness.snapshot().await.phase,
        HarnessPhaseTag::TurnCompleted
    );

    harness.shutdown().await.unwrap();
}

/// A completion frame without an `id` is accepted under `last_turn_id` (the arm's fallback), and the row carries that id.
#[tokio::test]
async fn turn_completed_without_id_writes_the_row_under_the_accepted_turn_id() {
    let repo = Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap());
    let events = EventBus::new();
    let (harness, daemon, card_id, _track_id) = seed_harness(repo.clone(), events).await;
    wait_for_notification_receiver(&daemon).await;

    daemon.emit_notification_for_test(Notification::TurnStarted {
        thread_id: SEED_THREAD_ID.into(),
        turn: json!({ "id": "turn-no-id" }),
    });
    daemon.emit_notification_for_test(Notification::TurnCompleted {
        thread_id: SEED_THREAD_ID.into(),
        turn: json!({ "status": "completed", "items": [] }),
    });

    let rows = wait_for_rows(&repo, &card_id, 1).await;
    assert_eq!(rows[0].method, "turn/completed");
    assert_eq!(
        rows[0].turn_id.as_deref(),
        Some("turn-no-id"),
        "the row names the turn the FSM accepted the completion for"
    );
    let stored: Value = serde_json::from_str(&rows[0].params).unwrap();
    assert_eq!(stored["status"], "completed");
    assert!(
        stored.get("id").is_none(),
        "params stay the frame as codex sent it; the id lives in the row's column"
    );
    assert_eq!(
        harness.snapshot().await.phase,
        HarnessPhaseTag::TurnCompleted
    );

    harness.shutdown().await.unwrap();
}
