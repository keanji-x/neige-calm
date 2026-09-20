//! The person's sentence is on the transcript from the moment the queue
//! drains, not from the moment codex echoes it.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::body::Body;
use axum::http::{Request, StatusCode};
use calm_server::card_role_cache::CardRoleCache;
use calm_server::codex_appserver::Notification;
use calm_server::db::prelude::*;
use calm_server::db::sqlite::{SqlxRepo, session_start_runtime_tx};
use calm_server::event::{BroadcastEnvelope, Event, EventBus};
use calm_server::harness::{
    HarnessConfig, HarnessPhaseTag, HarnessSnapshot, Observation, PlannerHarness,
    PlannerHarnessParams, QueueEntry, QueueEntryId,
};
use calm_server::model::{
    CardRole, HarnessInputPresentation, NewArea, NewCard, NewTrack, new_id, now_ms,
};
use calm_server::plugin_host::{PluginHost, PluginRegistry};
use calm_server::routes;
use calm_server::session_projection_repo::{
    AgentProvider, WorkerSessionInit, WorkerSessionKind, WorkerSessionState,
};
use calm_server::shared_codex_appserver::{SharedCodexAppServer, TurnStartReturnHook};
use calm_server::state::{AppState, CodexClient, DaemonClient};
use calm_server::track_area_cache::TrackAreaCache;
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tower::ServiceExt;

const SEED_THREAD_ID: &str = "thread-items-projection";

struct Boot {
    app: axum::Router,
    repo: Arc<SqlxRepo>,
    daemon: Arc<SharedCodexAppServer>,
    harness: PlannerHarness,
    card_id: String,
    worker_session_id: String,
    /// Subscribed BEFORE the harness runs, so the first phase event is in it.
    events_rx: tokio::sync::broadcast::Receiver<BroadcastEnvelope>,
}

/// A transcript row already on the table when the harness boots.
struct SeededProjection {
    client_id: String,
    text: String,
}

struct BootPlan {
    pending: Vec<QueueEntry>,
    fail_turn_start: bool,
    projection_client_id: Option<QueueEntryId>,
    seeded: Vec<SeededProjection>,
    /// Installed before the harness runs, so the first `turn/start` is held until the test releases it.
    turn_start_hook: Option<TurnStartReturnHook>,
    /// A hand-written literal: this binary cannot serialize the key it carries.
    pre_p2_snapshot_json: Option<Value>,
}

impl BootPlan {
    fn new(pending: Vec<QueueEntry>) -> Self {
        Self {
            pending,
            fail_turn_start: false,
            projection_client_id: None,
            seeded: Vec::new(),
            turn_start_hook: None,
            pre_p2_snapshot_json: None,
        }
    }
}

async fn boot(pending: Vec<QueueEntry>, fail_turn_start: bool) -> Boot {
    boot_with(BootPlan {
        fail_turn_start,
        ..BootPlan::new(pending)
    })
    .await
}

async fn boot_with(plan: BootPlan) -> Boot {
    let BootPlan {
        pending,
        fail_turn_start,
        projection_client_id,
        seeded,
        turn_start_hook,
        pre_p2_snapshot_json,
    } = plan;
    let repo = Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap());
    let events = EventBus::new();
    let area = repo
        .area_create(NewArea {
            name: "items-projection".into(),
            color: "#111111".into(),
            sort: None,
        })
        .await
        .unwrap();
    let track = repo
        .track_create(NewTrack {
            template_input: None,
            area_id: area.id.clone(),
            title: "items projection".into(),
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
            payload: json!({"schemaVersion": 1, "planner_harness": true}),
        })
        .await
        .unwrap();
    let role_cache = CardRoleCache::new();
    role_cache.insert(card.id.clone(), CardRole::Planner, track.id.clone());
    let track_area_cache = TrackAreaCache::new();
    track_area_cache.insert(track.id.clone(), area.id);

    let worker_session_id = new_id();
    for row in seeded {
        let params = json!({
            "item": {
                "id": row.client_id,
                "clientId": row.client_id,
                "type": "userMessage",
                "content": [{ "type": "text", "text": row.text }],
            },
            "_projection": true,
        });
        let segments = json!([{ "presentation": "user", "text": row.text, "attachments": [] }]);
        repo.harness_item_insert(
            "predecessor-session",
            card.id.as_str(),
            card.track_id.as_str(),
            SEED_THREAD_ID,
            None,
            Some(row.client_id.as_str()),
            Some("userMessage"),
            "item/completed",
            &params.to_string(),
            Some(&segments.to_string()),
        )
        .await
        .unwrap();
    }
    let stored = match pre_p2_snapshot_json {
        Some(stored) => stored,
        None => {
            let mut snapshot = HarnessSnapshot::initial(0, pending);
            snapshot.phase = HarnessPhaseTag::Idle;
            snapshot.last_thread_id = Some(SEED_THREAD_ID.to_string());
            snapshot.projection_client_id = projection_client_id;
            serde_json::to_value(&snapshot).unwrap()
        }
    };
    let snapshot = HarnessSnapshot::from_value_strict(stored.clone());
    let mut tx = repo.pool().begin().await.unwrap();
    session_start_runtime_tx(
        &mut tx,
        WorkerSessionInit {
            id: worker_session_id.clone(),
            card_id: card.id.to_string(),
            kind: WorkerSessionKind::SharedPlanner,
            agent_provider: Some(AgentProvider::Codex),
            status: WorkerSessionState::Idle,
            terminal_run_id: None,
            thread_id: Some(SEED_THREAD_ID.to_string()),
            session_id: None,
            active_turn_id: None,
            handle_state_json: Some(stored),
            spawn_op_id: None,
            now_ms: now_ms(),
        },
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();

    let daemon = SharedCodexAppServer::new_fake_running_with_pending(repo.clone(), None);
    if fail_turn_start {
        daemon.fail_turn_start_for_test();
    }
    if let Some(hook) = turn_start_hook {
        daemon.install_turn_start_return_hook_for_test(hook);
    }
    let events_rx = events.subscribe();
    let repo_dyn: Arc<dyn Repo> = repo.clone();
    let harness = PlannerHarness::run(PlannerHarnessParams {
        worker_session_id: worker_session_id.clone(),
        track_id: card.track_id.clone(),
        card_id: card.id.clone(),
        thread_id: Some(SEED_THREAD_ID.to_string()),
        repo: repo_dyn.clone(),
        events: events.clone(),
        card_role_cache: role_cache.clone(),
        track_area_cache: track_area_cache.clone(),
        daemon: daemon.clone(),
        config: HarnessConfig {
            debounce_min_idle: Duration::from_secs(60),
            debounce_max_wait: Duration::from_secs(60),
            // A `turn_running` snapshot restores as `Resumed`; the watchdog
            // must not flip it to `Idle` under a test that is still reading.
            resumed_reconcile_budget: Duration::from_secs(60),
            ..HarnessConfig::default()
        },
        snapshot,
    });

    let state = AppState::from_parts(
        repo_dyn,
        events.clone(),
        Arc::new(DaemonClient::new_stub()),
        Arc::new(PluginHost::new_full(
            Arc::new(PluginRegistry::empty()),
            repo.clone(),
            PathBuf::new(),
            std::env::temp_dir().join("calm-plugins-data-items-projection"),
            Vec::new(),
            events,
            calm_server::state::WriteContext::new(role_cache.clone(), track_area_cache.clone()),
        )),
        Arc::new(CodexClient::new_stub()),
        Some(role_cache),
        Some(track_area_cache),
    );
    let app = routes::router()
        .layer(axum::middleware::from_fn(
            calm_server::actor::actor_middleware,
        ))
        .with_state(state);

    Boot {
        app,
        repo,
        daemon,
        harness,
        card_id: card.id.to_string(),
        worker_session_id,
        events_rx,
    }
}

/// The next `harness.phase.changed` on the bus, as `(old, new)`.
async fn next_phase_change(boot: &mut Boot) -> (HarnessPhaseTag, HarnessPhaseTag) {
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        assert!(!remaining.is_zero(), "timed out waiting for a phase event");
        let env = tokio::time::timeout(remaining, boot.events_rx.recv())
            .await
            .expect("event timeout")
            .expect("event receive");
        if let Event::HarnessPhaseChanged {
            old_phase,
            new_phase,
            ..
        } = &env.event
        {
            assert_eq!(
                env.event.kind_tag(),
                "harness.phase.changed",
                "the wire kind `fe/core/events/invalidation-plan.ts` keys its plan on"
            );
            return (*old_phase, *new_phase);
        }
    }
}

async fn get_items(boot: &Boot) -> Vec<Value> {
    let resp = boot
        .app
        .clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(format!("/api/cards/{}/harness/items", boot.card_id))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    serde_json::from_slice::<Vec<Value>>(&bytes).unwrap()
}

async fn wait_until<F: FnMut() -> bool>(what: &str, mut done: F) {
    let deadline = Instant::now() + Duration::from_secs(2);
    while !done() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

async fn wait_for_turn_start(boot: &Boot) {
    wait_until("turn/start", || {
        boot.daemon.turn_start_count_for_test() >= 1
    })
    .await;
}

async fn wait_for_row_count(boot: &Boot, count: usize) -> Vec<Value> {
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        let rows = get_items(boot).await;
        if rows.len() == count {
            return rows;
        }
        assert!(
            Instant::now() < deadline,
            "timed out waiting for {count} transcript rows; got {}",
            rows.len()
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

fn user_texts(rows: &[Value]) -> Vec<String> {
    rows.iter()
        .filter(|row| row["item_type"] == "userMessage")
        .map(|row| {
            let params: Value = serde_json::from_str(row["params"].as_str().unwrap()).unwrap();
            params["item"]["content"][0]["text"]
                .as_str()
                .unwrap()
                .to_string()
        })
        .collect()
}

#[tokio::test]
async fn drained_user_message_is_readable_before_the_echo_and_upgraded_by_it() {
    let entries = QueueEntry::entries_from_observations_for_test(vec![Observation::UserMessage {
        text: "hello from the queue".into(),
    }]);
    let entry_id = entries[0].id().expect("a user entry has an id").to_string();
    let boot = boot(entries, false).await;
    wait_for_turn_start(&boot).await;

    let rows = wait_for_row_count(&boot, 1).await;
    let projection = &rows[0];
    assert_eq!(projection["item_type"], "userMessage");
    assert_eq!(projection["method"], "item/completed");
    assert_eq!(projection["turn_id"], Value::Null);
    assert_eq!(projection["item_uuid"], entry_id);
    assert_eq!(
        projection["input_segments"][0]["presentation"],
        serde_json::to_value(HarnessInputPresentation::User).unwrap()
    );
    assert!(
        projection["input_segments"][0]["text"]
            .as_str()
            .unwrap()
            .contains("hello from the queue")
    );
    let params: Value = serde_json::from_str(projection["params"].as_str().unwrap()).unwrap();
    assert_eq!(params["_projection"], true);
    assert_eq!(params["item"]["clientId"], entry_id);
    assert_eq!(params["item"]["type"], "userMessage");
    assert!(
        params["item"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("hello from the queue")
    );
    let projection_db_id = projection["id"].as_i64().unwrap();

    assert_eq!(
        boot.daemon.started_turn_client_ids_for_test(),
        vec![Some(entry_id.clone())]
    );

    let echo_item = json!({
        "id": "item-user-codex-1",
        "clientId": entry_id,
        "type": "userMessage",
        "content": [{ "type": "text", "text": "User says:\nhello from the queue" }]
    });
    boot.daemon.emit_notification_for_test(Notification::Item {
        method: "item/started".into(),
        params: json!({
            "threadId": SEED_THREAD_ID,
            "turn": { "id": "fake-turn-0001" },
            "item": echo_item.clone()
        }),
    });
    boot.daemon.emit_notification_for_test(Notification::Item {
        method: "item/completed".into(),
        params: json!({
            "threadId": SEED_THREAD_ID,
            "turn": { "id": "fake-turn-0001" },
            "item": echo_item
        }),
    });
    let deadline = Instant::now() + Duration::from_secs(2);
    let rows = loop {
        let rows = get_items(&boot).await;
        if rows.iter().any(|row| row["turn_id"] == "fake-turn-0001") {
            break rows;
        }
        assert!(
            Instant::now() < deadline,
            "timed out waiting for the completed echo to upgrade the projection"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    };
    assert_eq!(
        rows.len(),
        1,
        "exactly one renderable row per drained turn; got {rows:?}"
    );
    assert_eq!(user_texts(&rows).len(), 1);
    let upgraded = &rows[0];
    assert_eq!(upgraded["id"].as_i64().unwrap(), projection_db_id);
    assert_eq!(upgraded["turn_id"], "fake-turn-0001");
    assert_eq!(upgraded["item_uuid"], "item-user-codex-1");
    assert_eq!(
        upgraded["input_segments"], projection["input_segments"],
        "the drain's segments are kept through the upgrade"
    );
    let params: Value = serde_json::from_str(upgraded["params"].as_str().unwrap()).unwrap();
    assert_eq!(params.get("_projection"), None);
    assert_eq!(params["item"]["id"], "item-user-codex-1");

    boot.harness.shutdown().await.unwrap();
}

#[tokio::test]
async fn projection_key_is_the_first_entry_with_an_id_and_segments_carry_every_entry() {
    let entries = QueueEntry::entries_from_observations_for_test(vec![
        Observation::TaskCompleted {
            idempotency_key: "task-completed".into(),
            result: json!({"status": "ok"}),
        },
        Observation::UserMessage {
            text: "and then?".into(),
        },
    ]);
    assert_eq!(entries[0].id(), None, "a system entry carries no id");
    let user_entry_id = entries[1].id().unwrap().to_string();
    let boot = boot(entries, false).await;
    wait_for_turn_start(&boot).await;

    assert_eq!(
        boot.daemon.started_turn_client_ids_for_test(),
        vec![Some(user_entry_id.clone())]
    );
    let rows = wait_for_row_count(&boot, 1).await;
    assert_eq!(rows[0]["item_uuid"], user_entry_id);
    let presentations = rows[0]["input_segments"]
        .as_array()
        .unwrap()
        .iter()
        .map(|segment| segment["presentation"].clone())
        .collect::<Vec<_>>();
    assert_eq!(
        presentations,
        vec![
            serde_json::to_value(HarnessInputPresentation::SystemTaskCompleted).unwrap(),
            serde_json::to_value(HarnessInputPresentation::User).unwrap(),
        ]
    );

    boot.harness.shutdown().await.unwrap();
}

#[tokio::test]
async fn refused_turn_start_deletes_the_projection_and_rebuffers_the_entry() {
    let entries = QueueEntry::entries_from_observations_for_test(vec![Observation::UserMessage {
        text: "never sent".into(),
    }]);
    let entry_id = entries[0].id().unwrap().clone();
    let mut boot = boot(entries, true).await;
    wait_until("the first refusal", || {
        boot.harness.refused_issuances_for_test() >= 1
    })
    .await;
    // The delete emits no event of its own; the phase change after the re-buffer is the signal.
    let first = next_phase_change(&mut boot).await;
    assert_eq!(first, (HarnessPhaseTag::Idle, HarnessPhaseTag::IssuingTurn));
    let after_rebuffer = next_phase_change(&mut boot).await;
    assert_eq!(
        after_rebuffer,
        (HarnessPhaseTag::IssuingTurn, HarnessPhaseTag::TurnCompleted),
        "the re-buffer's snapshot emits the phase change a client refetches on"
    );
    assert_eq!(
        get_items(&boot).await,
        Vec::<Value>::new(),
        "by the time that event is on the bus, the row is already gone"
    );
    // The refusal is counted before the row is deleted; the retry is paced (2s).
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        let rows = get_items(&boot).await;
        let pending = boot.harness.snapshot().await.pending_entries();
        if rows.is_empty() && pending.len() == 1 {
            assert_eq!(pending[0].id(), Some(&entry_id));
            break;
        }
        assert!(
            Instant::now() < deadline,
            "timed out: rows={rows:?} pending={pending:?}"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(
        boot.repo
            .harness_item_list_by_card(&boot.card_id, 0, 100, false)
            .await
            .unwrap()
            .len(),
        0,
        "no projection survives a refused turn/start"
    );

    boot.harness.shutdown().await.unwrap();
}

#[tokio::test]
async fn a_restarted_harness_replaces_the_stale_projection_of_a_user_entry() {
    let entries = QueueEntry::entries_from_observations_for_test(vec![Observation::UserMessage {
        text: "said once, drained twice".into(),
    }]);
    let entry_id = entries[0].id().unwrap().to_string();
    let boot = boot_with(BootPlan {
        seeded: vec![SeededProjection {
            client_id: entry_id.clone(),
            text: "said once, drained twice".into(),
        }],
        ..BootPlan::new(entries)
    })
    .await;
    assert_eq!(
        get_items(&boot).await.len(),
        1,
        "the predecessor's row is on the table before the successor drains"
    );
    wait_for_turn_start(&boot).await;

    let rows = wait_for_row_count(&boot, 1).await;
    assert_eq!(rows[0]["item_uuid"], entry_id);
    assert_eq!(rows[0]["turn_id"], Value::Null);
    assert_ne!(
        rows[0]["worker_session_id"], "predecessor-session",
        "the surviving row is the successor's write, not the leftover"
    );
    assert_eq!(
        boot.daemon.started_turn_client_ids_for_test(),
        vec![Some(entry_id)]
    );
    // Give a late duplicate every chance to appear.
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(get_items(&boot).await.len(), 1);

    boot.harness.shutdown().await.unwrap();
}

#[tokio::test]
async fn a_restarted_harness_replaces_the_stale_projection_of_a_system_only_batch() {
    let entries =
        QueueEntry::entries_from_observations_for_test(vec![Observation::TaskCompleted {
            idempotency_key: "task-done".into(),
            result: json!({"status": "ok"}),
        }]);
    assert_eq!(entries[0].id(), None, "a system entry carries no id");
    let persisted_key = QueueEntryId::from_wire("minted-by-the-predecessor".into());
    let boot = boot_with(BootPlan {
        projection_client_id: Some(persisted_key.clone()),
        seeded: vec![SeededProjection {
            client_id: persisted_key.to_string(),
            text: "task task-done completed".into(),
        }],
        ..BootPlan::new(entries)
    })
    .await;
    assert_eq!(get_items(&boot).await.len(), 1);
    wait_for_turn_start(&boot).await;

    assert_eq!(
        boot.daemon.started_turn_client_ids_for_test(),
        vec![Some(persisted_key.to_string())],
        "the successor keys the re-drain by the persisted mint, not a fresh one"
    );
    let rows = wait_for_row_count(&boot, 1).await;
    assert_eq!(rows[0]["item_uuid"], persisted_key.to_string());
    assert_ne!(rows[0]["worker_session_id"], "predecessor-session");
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(get_items(&boot).await.len(), 1);
    let deadline = Instant::now() + Duration::from_secs(2);
    while boot.harness.snapshot().await.projection_client_id.is_some() {
        assert!(
            Instant::now() < deadline,
            "timed out waiting for the issuance outcome to clear the key"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    boot.harness.shutdown().await.unwrap();
}

/// The queue is seeded as `[system, U]` rather than U being enqueued live: every
/// enqueue runs on the harness task, so nothing outside it can order one before the first drain.
#[tokio::test]
async fn a_recovered_key_outranks_a_sentence_enqueued_before_the_re_drain() {
    let entries = QueueEntry::entries_from_observations_for_test(vec![
        Observation::TaskCompleted {
            idempotency_key: "task-done".into(),
            result: json!({"status": "ok"}),
        },
        Observation::UserMessage {
            text: "typed after the restart".into(),
        },
    ]);
    assert_eq!(entries[0].id(), None);
    let sentence_id = entries[1].id().expect("a user entry has an id").to_string();
    let persisted_key = QueueEntryId::from_wire("minted-by-the-predecessor".into());
    let boot = boot_with(BootPlan {
        projection_client_id: Some(persisted_key.clone()),
        seeded: vec![SeededProjection {
            client_id: persisted_key.to_string(),
            text: "task task-done completed".into(),
        }],
        ..BootPlan::new(entries)
    })
    .await;
    assert_eq!(get_items(&boot).await.len(), 1);
    wait_for_turn_start(&boot).await;

    assert_eq!(
        boot.daemon.started_turn_client_ids_for_test(),
        vec![Some(persisted_key.to_string())],
        "the re-drain is keyed by the recovered key, not by the sentence that arrived after it"
    );
    let rows = wait_for_row_count(&boot, 1).await;
    assert_eq!(rows[0]["item_uuid"], persisted_key.to_string());
    assert_ne!(
        rows[0]["worker_session_id"], "predecessor-session",
        "the predecessor's row was replaced, not joined"
    );
    let segments = rows[0]["input_segments"]
        .as_array()
        .expect("the projection carries its segments");
    assert!(
        segments.iter().any(|segment| segment["text"]
            .as_str()
            .unwrap()
            .contains("typed after the restart")),
        "the sentence rides in the recovered batch's row: {segments:?}"
    );
    // Give a second row every chance to appear.
    tokio::time::sleep(Duration::from_millis(200)).await;
    let rows = get_items(&boot).await;
    assert_eq!(rows.len(), 1, "one row, not one per key: {rows:?}");
    assert!(
        rows.iter().all(|row| row["item_uuid"] != sentence_id),
        "nothing stands under the sentence's own id"
    );

    boot.harness.shutdown().await.unwrap();
}

/// Read inside `turn/start`, held open by the fake daemon: after the row, before the issuance outcome.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_system_only_batch_persists_its_minted_key_before_the_row() {
    let entries =
        QueueEntry::entries_from_observations_for_test(vec![Observation::TaskCompleted {
            idempotency_key: "task-first".into(),
            result: json!({"status": "ok"}),
        }]);
    let entered = Arc::new(tokio::sync::Notify::new());
    let release = Arc::new(tokio::sync::Notify::new());
    let boot = boot_with(BootPlan {
        turn_start_hook: Some(TurnStartReturnHook {
            entered: entered.clone(),
            release: release.clone(),
        }),
        ..BootPlan::new(entries)
    })
    .await;
    entered.notified().await;

    let rows = boot
        .repo
        .harness_item_list_by_card(&boot.card_id, 0, 10, false)
        .await
        .unwrap();
    assert_eq!(rows.len(), 1, "the projection row precedes turn/start");
    let key = rows[0]
        .item_uuid
        .clone()
        .expect("a projection row is keyed");
    let stored = boot
        .repo
        .session_projection_by_id(&boot.worker_session_id)
        .await
        .unwrap()
        .unwrap()
        .handle_state_json
        .unwrap();
    assert_eq!(stored["projection_client_id"], key);
    let persisted = HarnessSnapshot::from_value_strict(stored);
    assert_eq!(
        persisted.pending_entries().len(),
        1,
        "the pre-drain snapshot still lists the batch"
    );
    assert_eq!(
        boot.daemon.started_turn_client_ids_for_test(),
        vec![Some(key.clone())],
        "and it is the key codex is being handed"
    );

    release.notify_one();
    let deadline = Instant::now() + Duration::from_secs(2);
    while boot.harness.snapshot().await.projection_client_id.is_some() {
        assert!(
            Instant::now() < deadline,
            "timed out waiting for the issuance outcome to clear the key"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    boot.harness.shutdown().await.unwrap();
}

#[tokio::test]
async fn an_echo_upgrades_the_projection_of_its_own_card_only() {
    let repo = Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap());
    let area = repo
        .area_create(NewArea {
            name: "two-cards".into(),
            color: "#111111".into(),
            sort: None,
        })
        .await
        .unwrap();
    let track = repo
        .track_create(NewTrack {
            template_input: None,
            area_id: area.id.clone(),
            title: "two cards".into(),
            sort: None,
            cwd: "/tmp".into(),
            template_id: None,
            plugin_scope: None,
            attach_folder: false,
            theme: calm_server::routes::theme::RequestTheme::default_dark(),
        })
        .await
        .unwrap();
    let mut card_ids = Vec::new();
    for _ in 0..2 {
        let card = repo
            .card_create(NewCard {
                track_id: track.id.clone(),
                title: None,
                kind: "codex".into(),
                sort: None,
                payload: json!({"schemaVersion": 1, "planner_harness": true}),
            })
            .await
            .unwrap();
        card_ids.push(card.id.to_string());
    }
    let (card_a, card_b) = (card_ids[0].clone(), card_ids[1].clone());
    let client_id = "shared-client-id";
    let mut row_ids = Vec::new();
    for card in [&card_a, &card_b] {
        let id = repo
            .harness_item_insert(
                "session",
                card,
                track.id.as_str(),
                SEED_THREAD_ID,
                None,
                Some(client_id),
                Some("userMessage"),
                "item/completed",
                r#"{"item":{"type":"userMessage"},"_projection":true}"#,
                Some(r#"[{"presentation":"user","text":"shared words","attachments":[]}]"#),
            )
            .await
            .unwrap();
        row_ids.push(id);
    }
    let (row_a, row_b) = (row_ids[0], row_ids[1]);

    let upgraded = repo
        .transcript_projection_upgrade(
            &card_a,
            client_id,
            Some("turn-a"),
            "codex-item-a",
            r#"{"item":{"id":"codex-item-a","type":"userMessage"}}"#,
        )
        .await
        .unwrap();
    assert_eq!(upgraded, Some(row_a), "A's echo upgrades A's row");
    assert_eq!(
        repo.transcript_projection_id(&card_a, client_id)
            .await
            .unwrap(),
        None,
        "A no longer holds a projection under that key"
    );
    assert_eq!(
        repo.transcript_projection_id(&card_b, client_id)
            .await
            .unwrap(),
        Some(row_b),
        "B's projection is untouched"
    );
    let b_rows = repo
        .harness_item_list_by_card(&card_b, 0, 10, false)
        .await
        .unwrap();
    assert_eq!(b_rows.len(), 1);
    assert_eq!(b_rows[0].turn_id, None);
    assert_eq!(b_rows[0].item_uuid.as_deref(), Some(client_id));

    assert_eq!(
        repo.transcript_projection_delete(&card_a, client_id)
            .await
            .unwrap(),
        0,
        "nothing left to delete on A"
    );
    assert_eq!(
        repo.transcript_projection_delete(&card_b, client_id)
            .await
            .unwrap(),
        1
    );
}

/// The stored snapshot is a hand-written literal in the pre-P2 shape, because
/// this binary cannot serialize the key.
#[tokio::test]
async fn an_echo_of_a_turn_issued_before_the_upgrade_takes_the_snapshots_segments() {
    let turn = "turn-issued-by-the-pre-p2-binary";
    let attachment_id = "0f9c2a4e-5b6d-4c7e-8a9b-0c1d2e3f4a5b.png";
    let segments = json!([
        {
            "presentation": "system_task_completed",
            "text": "task task-done completed",
            "attachments": []
        },
        {
            "presentation": "user",
            "text": "look at this",
            "attachments": [{
                "id": attachment_id,
                "contentType": "image/png",
                "size": 1234,
                "url": format!("/api/cards/pre-p2-card/planner/attachments/{attachment_id}")
            }]
        }
    ]);
    let stored = json!({
        "schema_version": 1,
        "mode": "harness",
        "phase": "turn_running",
        "push_watermark": 0,
        "pending_queue": [],
        "pending_envelope_ids": [],
        "pending_entry_meta": [],
        "pending_message_ids": [],
        "last_thread_id": SEED_THREAD_ID,
        "last_turn_id": turn,
        "last_report_body_sha256": null,
        "last_seen_head": null,
        "issued_turn_head": null,
        "issued_input_segments": { "turn_id": turn, "segments": segments },
        "wedged_reason": null,
        "token_usage": null
    });
    let boot = boot_with(BootPlan {
        pre_p2_snapshot_json: Some(stored),
        ..BootPlan::new(Vec::new())
    })
    .await;
    assert!(
        get_items(&boot).await.is_empty(),
        "no projection row: the old drain wrote none"
    );

    let echo_item = json!({
        "id": "item-user-codex-pre-p2",
        "type": "userMessage",
        "content": [{ "type": "text", "text": "task task-done completed\nUser says:\nlook at this" }]
    });
    for method in ["item/started", "item/completed"] {
        boot.daemon.emit_notification_for_test(Notification::Item {
            method: method.into(),
            params: json!({
                "threadId": SEED_THREAD_ID,
                "turn": { "id": turn },
                "item": echo_item.clone()
            }),
        });
    }
    let rows = wait_for_row_count(&boot, 2).await;
    let completed = rows
        .iter()
        .find(|row| row["method"] == "item/completed")
        .expect("the completed echo is stored");
    assert_eq!(completed["turn_id"], turn);
    assert_eq!(completed["item_uuid"], "item-user-codex-pre-p2");
    assert_eq!(
        completed["input_segments"], segments,
        "the row carries the segments the previous binary persisted, attachment included"
    );

    // Codex sends no second completed `userMessage` on a turn; this is the cheapest probe of "read once".
    boot.daemon.emit_notification_for_test(Notification::Item {
        method: "item/completed".into(),
        params: json!({
            "threadId": SEED_THREAD_ID,
            "turn": { "id": turn },
            "item": {
                "id": "item-user-codex-pre-p2-again",
                "type": "userMessage",
                "content": [{ "type": "text", "text": "again" }]
            }
        }),
    });
    let rows = wait_for_row_count(&boot, 3).await;
    let again = rows
        .iter()
        .find(|row| row["item_uuid"] == "item-user-codex-pre-p2-again")
        .unwrap();
    assert_eq!(
        again.get("input_segments"),
        None,
        "the slot was consumed by the first completed echo"
    );

    let persisted = boot
        .repo
        .session_projection_by_id(&boot.worker_session_id)
        .await
        .unwrap()
        .unwrap()
        .handle_state_json
        .unwrap();
    assert_eq!(persisted.get("issued_input_segments"), None);
    assert_eq!(
        persisted["last_turn_id"], turn,
        "the rest of the snapshot round-tripped"
    );

    boot.harness.shutdown().await.unwrap();
}
