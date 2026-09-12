//! #1625 P2 (#1475) — the person's sentence is on the transcript from the
//! moment the queue drains, not from the moment codex echoes it.
//!
//! Driven through the production drain (`maybe_issue_turn` →
//! `write_projection_row`), the production echo arm (`on_notification`'s
//! `item/*` arm) and the production REST read
//! (`GET /api/cards/{id}/harness/items`), over one sqlite repo. The fake
//! daemon accepts `turn/start` and echoes nothing until the test says so,
//! which is exactly the window #1475 is about.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::body::Body;
use axum::http::{Request, StatusCode};
use calm_server::card_role_cache::CardRoleCache;
use calm_server::codex_appserver::Notification;
use calm_server::db::prelude::*;
use calm_server::db::sqlite::{SqlxRepo, session_start_runtime_tx};
use calm_server::event::EventBus;
use calm_server::harness::{
    HarnessConfig, HarnessPhaseTag, HarnessSnapshot, Observation, PlannerHarness,
    PlannerHarnessParams, QueueEntry,
};
use calm_server::model::{
    CardRole, HarnessInputPresentation, NewArea, NewCard, NewTrack, new_id, now_ms,
};
use calm_server::plugin_host::{PluginHost, PluginRegistry};
use calm_server::routes;
use calm_server::session_projection_repo::{
    AgentProvider, WorkerSessionInit, WorkerSessionKind, WorkerSessionState,
};
use calm_server::shared_codex_appserver::SharedCodexAppServer;
use calm_server::state::{AppState, CodexClient, DaemonClient};
use calm_server::track_area_cache::TrackAreaCache;
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tower::ServiceExt;

/// See `planner_harness_items_persist.rs` for why every frame must carry it.
const SEED_THREAD_ID: &str = "thread-items-projection";

struct Boot {
    app: axum::Router,
    repo: Arc<SqlxRepo>,
    daemon: Arc<SharedCodexAppServer>,
    harness: PlannerHarness,
    card_id: String,
}

/// One planner card with `pending` already on its harness queue, a fake
/// daemon that answers `turn/start` (or refuses it, when `fail_turn_start`),
/// and the REST router over the same repo.
async fn boot(pending: Vec<QueueEntry>, fail_turn_start: bool) -> Boot {
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

    let runtime_id = new_id();
    let mut snapshot = HarnessSnapshot::initial(0, pending);
    snapshot.phase = HarnessPhaseTag::Idle;
    snapshot.last_thread_id = Some(SEED_THREAD_ID.to_string());
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
            thread_id: Some(SEED_THREAD_ID.to_string()),
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
    if fail_turn_start {
        daemon.fail_turn_start_for_test();
    }
    let repo_dyn: Arc<dyn Repo> = repo.clone();
    let harness = PlannerHarness::run(PlannerHarnessParams {
        worker_session_id: runtime_id,
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

/// The #1475 window, closed: the sentence is readable over REST as soon as
/// the queue drains, before any echo; and the echo — `item/started` then
/// `item/completed`, both naming the projection by `clientId` — leaves
/// exactly the one row, upgraded in place.
#[tokio::test]
async fn drained_user_message_is_readable_before_the_echo_and_upgraded_by_it() {
    let entries = QueueEntry::entries_from_observations_for_test(vec![Observation::UserMessage {
        text: "hello from the queue".into(),
    }]);
    let entry_id = entries[0].id().expect("a user entry has an id").to_string();
    let boot = boot(entries, false).await;
    wait_for_turn_start(&boot).await;

    // Before any echo: one row, the person's words, keyed by the entry id.
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

    // `turn/start` carried the same id as `clientUserMessageId`.
    assert_eq!(
        boot.daemon.started_turn_client_ids_for_test(),
        vec![Some(entry_id.clone())]
    );

    // The echo. Started first, as codex sends it; then completed.
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
    // Still one row — the started echo stored nothing, the completed echo
    // upgraded rather than appended — and it is the same row.
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

/// A batch whose first entry has no id (a system observation) still gets one
/// key — the first entry that HAS an id — and the projection's segments
/// carry every entry, in order.
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

/// `turn/start` refused: the row that said "sent" goes, and the entry is
/// back on the queue where the queue region lists it.
#[tokio::test]
async fn refused_turn_start_deletes_the_projection_and_rebuffers_the_entry() {
    let entries = QueueEntry::entries_from_observations_for_test(vec![Observation::UserMessage {
        text: "never sent".into(),
    }]);
    let entry_id = entries[0].id().unwrap().clone();
    let boot = boot(entries, true).await;
    wait_until("the first refusal", || {
        boot.harness.refused_issuances_for_test() >= 1
    })
    .await;
    // The refusal is counted before the row is deleted and the batch put
    // back; the retry is paced (2s), so this settles well inside the window.
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
