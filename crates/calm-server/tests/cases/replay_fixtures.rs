//! Replay-based regression tests: fixtures under `tests/fixtures/events/` are
//! raw-inserted into a fresh in-memory server and drained over WS with `since=0`.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use calm_server::db::prelude::*;
use calm_server::db::sqlite::{SqlxRepo, session_start_runtime_tx};
use calm_server::event::{Event, EventBus, EventScope};
use calm_server::ids::ActorId;
use calm_server::model::{
    NewArea, NewCard, NewTrack, Overlay, Task, TaskKind, TaskStatus, TrackLifecycle,
};
use calm_server::model::{TrackPatch, new_id, now_ms};
use calm_server::replay::{self, Fixture};
use calm_server::routes;
use calm_server::session_projection_repo::{
    AgentProvider, WorkerSessionInit, WorkerSessionKind, WorkerSessionState,
};
use calm_server::ws;
use futures_util::{SinkExt, StreamExt};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tokio::net::TcpListener;
use tokio::time::timeout;
use tokio_tungstenite::tungstenite::Message as TMessage;
use tower::ServiceExt;

fn load_fixture(name: &str) -> Fixture {
    let mut path = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    path.push("tests");
    path.push("fixtures");
    path.push("events");
    path.push(name);
    replay::load_fixture_from_path(&path).expect("load fixture")
}

async fn seed_rooted_track(repo: &SqlxRepo) {
    let area = repo
        .area_create(NewArea {
            name: "reset-rooted".into(),
            color: "#123456".into(),
            sort: None,
        })
        .await
        .expect("create reset area");
    let track = repo
        .track_create(NewTrack {
            template_input: None,
            area_id: area.id,
            title: "reset rooted track".into(),
            sort: None,
            cwd: String::new(),
            template_id: None,
            plugin_scope: None,
            attach_folder: false,
            theme: calm_server::routes::theme::RequestTheme::default_dark(),
        })
        .await
        .expect("create reset track");
    let card = repo
        .card_create(NewCard {
            track_id: track.id.clone(),
            title: None,
            kind: "codex".into(),
            sort: None,
            payload: json!({"schemaVersion": 1}),
        })
        .await
        .expect("create reset root card");
    let mut tx = repo.pool().begin().await.expect("begin runtime tx");
    let runtime = session_start_runtime_tx(
        &mut tx,
        WorkerSessionInit {
            id: new_id(),
            card_id: card.id.to_string(),
            kind: WorkerSessionKind::SharedPlanner,
            agent_provider: Some(AgentProvider::Codex),
            status: WorkerSessionState::Running,
            terminal_run_id: None,
            thread_id: None,
            session_id: None,
            active_turn_id: None,
            handle_state_json: None,
            spawn_op_id: None,
            now_ms: now_ms(),
        },
    )
    .await
    .expect("start reset root runtime");
    tx.commit().await.expect("commit runtime tx");

    let root: Option<String> =
        sqlx::query_scalar("SELECT root_session_id FROM tracks WHERE id = ?1")
            .bind(track.id.as_str())
            .fetch_one(repo.pool())
            .await
            .expect("read reset root");
    assert_eq!(root.as_deref(), Some(runtime.id.as_str()));
}

async fn boot() -> (std::net::SocketAddr, Arc<SqlxRepo>, EventBus) {
    let (repo, events, state) = replay::boot_in_memory()
        .await
        .expect("boot in-memory replay state");
    let app = axum::Router::new().merge(ws::router()).with_state(state);

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(
            listener,
            app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
        )
        .await
        .unwrap();
    });
    // Tiny grace for the listener task to start accepting before we open a WS.
    tokio::time::sleep(Duration::from_millis(50)).await;
    (addr, repo, events)
}

async fn raw_insert_fixture_events(repo: &SqlxRepo, bus: &EventBus, fixture: &Fixture) -> Vec<i64> {
    replay::seed_events(repo, bus, fixture)
        .await
        .expect("seed fixture events")
}

async fn route_json(app: axum::Router, req: Request<Body>) -> (StatusCode, Value) {
    let resp = app.oneshot(req).await.expect("route request");
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let json: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, json)
}

async fn recv_json<S>(ws: &mut S) -> serde_json::Value
where
    S: futures_util::Stream<
            Item = std::result::Result<TMessage, tokio_tungstenite::tungstenite::Error>,
        > + Unpin,
{
    let msg = timeout(Duration::from_secs(2), ws.next())
        .await
        .expect("ws recv timeout")
        .expect("ws closed under us")
        .expect("ws transport error");
    let t = match msg {
        TMessage::Text(t) => t.to_string(),
        other => panic!("expected text frame, got {:?}", other),
    };
    serde_json::from_str(&t).expect("non-JSON frame")
}

#[tokio::test]
async fn replay_track_grid_layout_trace() {
    let fixture = load_fixture("track-grid-layout-trace.events.json");
    let (addr, repo, bus) = boot().await;
    let ids = raw_insert_fixture_events(&repo, &bus, &fixture).await;
    assert_eq!(
        ids.len(),
        fixture.events.len(),
        "all fixture events inserted"
    );

    let url = format!("ws://{}/api/events", addr);
    let (mut ws, _) = tokio_tungstenite::connect_async(&url)
        .await
        .expect("connect ws");
    ws.send(TMessage::Text(r#"{"sub":["*"], "since": 0}"#.to_string()))
        .await
        .expect("send sub");

    // The fixture lays down the layout overlay twice (initial + move); the second is the canonical post-replay state.
    let mut received: Vec<(i64, String)> = Vec::new();
    let mut last_layout_payload: Option<serde_json::Value> = None;

    loop {
        let frame = recv_json(&mut ws).await;
        if frame["ev"] == "_replay_complete" {
            break;
        }
        let id = frame["_id"].as_i64().expect("_id present");
        let kind = frame["ev"].as_str().expect("ev present").to_string();
        if kind == "overlay.set"
            && frame["data"]["entity_kind"] == "view"
            && frame["data"]["kind"] == "layout"
        {
            last_layout_payload = Some(frame["data"]["payload"].clone());
        }
        received.push((id, kind));
    }

    assert_eq!(received.len(), fixture.events.len(), "frame count");
    for w in received.windows(2) {
        assert!(w[0].0 < w[1].0, "monotonic ids: {:?}", received);
    }
    for ((_id, kind), fix_ev) in received.iter().zip(fixture.events.iter()) {
        assert_eq!(kind, &fix_ev.kind, "frame kind matches fixture order");
    }

    let last_kind = &received.last().expect("at least one frame").1;
    let expected_last_kind = fixture
        .expected
        .last_event_kind
        .as_ref()
        .expect("fixture sets last_event_kind");
    assert_eq!(last_kind, expected_last_kind, "last event kind");

    let last_layout = last_layout_payload.expect("layout overlay present in replay");
    let actual_positions = last_layout
        .get("positions")
        .and_then(|v| v.as_object())
        .expect("positions object")
        .clone();
    for (card_id, expected) in &fixture.expected.layout_positions {
        let actual = actual_positions
            .get(card_id)
            .unwrap_or_else(|| panic!("missing card_id {} in replayed layout", card_id));
        assert_eq!(
            actual, expected,
            "card_id {} position mismatch — replay diverged from fixture",
            card_id
        );
    }
    assert_eq!(
        actual_positions.len(),
        fixture.expected.layout_positions.len(),
        "positions cardinality must match — no extras",
    );
}

#[tokio::test]
async fn replay_router_terminal_card_create_persists_without_supervisor() {
    let (repo, _events, state) = replay::boot_in_memory()
        .await
        .expect("boot in-memory replay state");
    let area = repo
        .area_create(NewArea {
            name: "replay-terminal".into(),
            color: "#000".into(),
            sort: None,
        })
        .await
        .expect("create area");
    let track = repo
        .track_create(NewTrack {
            template_input: None,
            area_id: area.id.clone(),
            title: "replay-terminal".into(),
            sort: None,
            // The kernel refuses to open a terminal in an empty cwd; production tracks always have a path.
            cwd: "/neige-fixture-workspace".into(),
            template_id: None,
            plugin_scope: None,
            attach_folder: false,
            theme: calm_server::routes::theme::RequestTheme::default_dark(),
        })
        .await
        .expect("create track");
    state
        .track_area_cache
        .insert(track.id.clone(), area.id.clone());
    repo.track_update(
        track.id.as_str(),
        TrackPatch {
            lifecycle: Some(TrackLifecycle::Dispatching),
            ..Default::default()
        },
    )
    .await
    .expect("open scheduler lifecycle");
    let track_id = track.id.to_string();

    let worker_key = "replay-terminal-worker-hook";
    let worker_idempotency_key = format!("{track_id}:{worker_key}");
    let now = now_ms();
    let task = Task {
        id: worker_idempotency_key.clone(),
        track_id: track_id.clone(),
        key: worker_key.into(),
        kind: TaskKind::Terminal,
        goal: "printf replay-worker".into(),
        context_json: "null".into(),
        acceptance_criteria: None,
        cwd: None,
        depends_on_json: "[]".into(),
        priority: 0,
        gate_json: None,
        status: TaskStatus::Pending,
        status_detail: None,
        worker_card_id: None,
        gate_result_json: None,
        gate_attempt: 0,
        gate_pid: None,
        gate_pid_starttime: None,
        gate_pid_boot_id: None,
        running_deadline_ms: None,
        context_stale_at_ms: None,
        declared_by: "spec".into(),
        spawn: "in-wave".into(),
        created_at_ms: now,
        updated_at_ms: now,
        finished_at_ms: None,
    };
    crate::support::task::project_task(repo.pool(), &task)
        .await
        .expect("project task block");

    state
        .dispatcher
        .scheduler()
        .schedule_track(track.id.clone())
        .await;

    let worker_card = timeout(Duration::from_secs(2), async {
        loop {
            let cards = repo
                .cards_by_track(&track_id)
                .await
                .expect("list track cards");
            if let Some(card) = cards.into_iter().find(|card| {
                card.payload.get("idempotency_key").and_then(Value::as_str)
                    == Some(worker_idempotency_key.as_str())
            }) {
                break card;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .expect("dispatcher terminal worker should use replay spawn hook");
    assert_eq!(
        worker_card.kind, "terminal",
        "dispatcher should persist the replay terminal worker card"
    );
    let worker_terminal = repo
        .terminal_get_by_card(worker_card.id.as_ref())
        .await
        .expect("lookup worker terminal")
        .expect("worker terminal row");
    assert!(
        worker_terminal.exit_code.is_none() && !worker_terminal.signal_killed,
        "replay dispatcher terminal worker should not record a spawn failure: {worker_terminal:?}"
    );

    let app = routes::router()
        .layer(axum::middleware::from_fn(
            calm_server::actor::actor_middleware,
        ))
        .with_state(state);

    let body = json!({
        "program": "/bin/sh",
        "cwd": "",
        "env": {},
        "theme": {"fg": [216,219,226], "bg": [15,20,24]},
    });
    let (post_status, card) = route_json(
        app.clone(),
        Request::builder()
            .method("POST")
            .uri(format!("/api/tracks/{track_id}/terminal-cards"))
            .header("content-type", "application/json")
            .body(Body::from(body.to_string()))
            .expect("build terminal-card request"),
    )
    .await;
    assert!(
        post_status.is_success(),
        "replay terminal-card POST should be 2xx: {post_status}, body={card:?}"
    );
    assert_eq!(card["kind"], "terminal", "created card body: {card:?}");
    let card_id = card["id"].as_str().expect("created card id").to_string();

    let (get_status, detail) = route_json(
        app,
        Request::builder()
            .method("GET")
            .uri(format!("/api/tracks/{track_id}"))
            .body(Body::empty())
            .expect("build track detail request"),
    )
    .await;
    assert_eq!(get_status, StatusCode::OK, "track detail: {detail:?}");
    let cards = detail["cards"]
        .as_array()
        .expect("track detail cards array");
    assert!(
        cards.iter().any(|card| card["id"] == card_id),
        "created terminal card row should survive replay no-op spawn: {detail:?}"
    );
}

#[tokio::test]
async fn record_session_roundtrips_through_loader() {
    let (repo, bus, _state) = replay::boot_in_memory()
        .await
        .expect("boot in-memory replay state");

    let tmpdir = tempfile::tempdir().expect("tempdir");
    let session_path = tmpdir.path().join("recorded.events.json");
    replay::spawn_session_recorder(&bus, session_path.clone());

    // The recorder subscribes before the `tokio::spawn`, so there is no race against the recorder task starting.
    let events: Vec<Event> = vec![
        Event::OverlaySet(Overlay {
            id: "ov-1".into(),
            plugin_id: "core".into(),
            entity_kind: "view".into(),
            entity_id: "track-1".into(),
            kind: "layout".into(),
            payload: serde_json::json!({"positions": {"card_1": {"x": 0, "y": 0, "w": 4, "h": 3}}}),
            updated_at: 1,
        }),
        Event::OverlayDeleted {
            plugin_id: "core".into(),
            entity_kind: "view".into(),
            entity_id: "track-1".into(),
            kind: "layout".into(),
        },
    ];
    let mut want_kinds: Vec<&str> = Vec::new();
    for ev in events {
        want_kinds.push(ev.kind_tag());
        repo.log_pure_event(
            ActorId::User,
            EventScope::System,
            None,
            &bus,
            &calm_server::card_role_cache::CardRoleCache::new(),
            &calm_server::track_area_cache::TrackAreaCache::new(),
            ev,
        )
        .await
        .expect("log_pure_event");
    }

    // The write happens off the broadcast task; poll the file until it has the expected line count.
    let deadline = std::time::Instant::now() + Duration::from_secs(2);
    loop {
        if let Ok(text) = std::fs::read_to_string(&session_path)
            && text.lines().filter(|l| !l.trim().is_empty()).count() >= want_kinds.len()
        {
            break;
        }
        if std::time::Instant::now() >= deadline {
            panic!(
                "recorder never flushed {} events to {}",
                want_kinds.len(),
                session_path.display()
            );
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }

    let fixture =
        replay::load_fixture_from_path(&session_path).expect("loader accepts recorded NDJSON");
    assert_eq!(
        fixture.events.len(),
        want_kinds.len(),
        "round-trip preserves event count"
    );
    for (got, want) in fixture.events.iter().zip(want_kinds.iter()) {
        assert_eq!(&got.kind, *want, "round-trip preserves event kind order");
    }
    // The loader synthesizes a `name` from the filename stem and an empty `expected` block for NDJSON.
    assert_eq!(fixture.name, "recorded.events");
    assert!(
        fixture.expected.last_event_kind.is_none() && fixture.expected.layout_positions.is_empty(),
        "NDJSON branch produces an empty expected block"
    );
}

#[test]
fn fold_layout_positions_respects_overlay_deleted() {
    let track_id = "track-1";

    let set_a = Event::OverlaySet(Overlay {
        id: "ov-a".into(),
        plugin_id: "core".into(),
        entity_kind: "view".into(),
        entity_id: track_id.into(),
        kind: "layout".into(),
        payload: serde_json::json!({"positions": {"card_1": {"x": 0, "y": 0, "w": 4, "h": 3}}}),
        updated_at: 1,
    });
    let delete = Event::OverlayDeleted {
        plugin_id: "core".into(),
        entity_kind: "view".into(),
        entity_id: track_id.into(),
        kind: "layout".into(),
    };
    let set_b = Event::OverlaySet(Overlay {
        id: "ov-b".into(),
        plugin_id: "core".into(),
        entity_kind: "view".into(),
        entity_id: track_id.into(),
        kind: "layout".into(),
        payload: serde_json::json!({"positions": {"card_9": {"x": 8, "y": 0, "w": 4, "h": 3}}}),
        updated_at: 3,
    });

    let got =
        replay::fold_layout_positions([set_a.clone(), delete.clone(), set_b.clone()], track_id)
            .expect("set after delete still produces Some");
    assert_eq!(got.len(), 1, "delete cleared set_a before set_b");
    assert!(got.contains_key("card_9"));
    assert!(!got.contains_key("card_1"));

    let got = replay::fold_layout_positions([set_a.clone(), delete.clone()], track_id);
    assert!(got.is_none(), "delete after lone set yields None");

    let got = replay::fold_layout_positions([delete], track_id);
    assert!(got.is_none(), "lone delete yields None");

    let delete_other = Event::OverlayDeleted {
        plugin_id: "core".into(),
        entity_kind: "view".into(),
        entity_id: "track-other".into(),
        kind: "layout".into(),
    };
    let got = replay::fold_layout_positions([set_a.clone(), delete_other, set_b.clone()], track_id)
        .expect("unrelated delete must not clear");
    assert!(got.contains_key("card_9"));
}

#[tokio::test]
async fn reset_from_fixture_wipes_and_reseeds() {
    let fixture = load_fixture("track-grid-layout-trace.events.json");
    let (repo, bus, _state) = replay::boot_in_memory()
        .await
        .expect("boot in-memory replay state");

    let initial_ids = replay::seed_events(&repo, &bus, &fixture)
        .await
        .expect("initial seed");
    let n = fixture.events.len() as i64;
    assert_eq!(initial_ids.len() as i64, n);
    assert_eq!(
        *initial_ids.last().expect("non-empty"),
        n,
        "initial seed assigns ids 1..=N because sqlite_sequence starts fresh"
    );

    let extra = Event::OverlaySet(Overlay {
        id: "ov-extra".into(),
        plugin_id: "core".into(),
        entity_kind: "view".into(),
        entity_id: "track-extra".into(),
        kind: "layout".into(),
        payload: serde_json::json!({"positions": {}}),
        updated_at: 99,
    });
    let extra_id = repo
        .log_pure_event(
            ActorId::User,
            EventScope::System,
            None,
            &bus,
            &calm_server::card_role_cache::CardRoleCache::new(),
            &calm_server::track_area_cache::TrackAreaCache::new(),
            extra,
        )
        .await
        .expect("log extra event");
    assert_eq!(extra_id, n + 1, "extra event sits at id=N+1");

    // `tasks` has no FK to `tracks`, so a track wipe alone would never cascade here.
    sqlx::query(
        "INSERT INTO tasks (id, track_id, key, kind, goal, context_json, \
         created_at_ms, updated_at_ms) \
         VALUES ('wv-x:t1', 'wv-x', 't1', 'codex', 'leftover goal', '{}', 1, 1)",
    )
    .execute(repo.pool())
    .await
    .expect("seed leftover tasks row");

    // A rooted track: `DELETE FROM worker_sessions` must cope with `tracks.root_session_id` still pointing at the root session.
    seed_rooted_track(&repo).await;

    let reseeded = replay::reset_from_fixture(&repo, &bus, &fixture)
        .await
        .expect("reset succeeds");
    assert_eq!(
        reseeded.len() as i64,
        n,
        "reseeded event count matches fixture"
    );
    assert_eq!(
        *reseeded.first().expect("non-empty"),
        1,
        "reset wipes sqlite_sequence — first event id is 1"
    );
    assert_eq!(
        *reseeded.last().expect("non-empty"),
        n,
        "reset wipes sqlite_sequence — last event id is N (no carry-over from the extra)"
    );

    let log = repo
        .events_since(0, i64::MAX)
        .await
        .expect("events_since after reset");
    assert_eq!(log.len() as i64, n, "log has only the reseeded events");

    let task_rows: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM tasks")
        .fetch_one(repo.pool())
        .await
        .expect("count tasks after reset");
    assert_eq!(task_rows, 0, "reset wipes tasks despite the missing FK");
    for ((_id, _ver, _scope, ev), fix_ev) in log.iter().zip(fixture.events.iter()) {
        assert_eq!(
            ev.kind_tag(),
            fix_ev.kind,
            "reseeded log preserves fixture event order"
        );
    }
}

/// The fence aborts any `DELETE` that reaches an `operations` row carrying an `idempotency_key`,
/// including a bare `DELETE FROM operations`. The planted row surviving is the correct outcome.
#[tokio::test]
async fn dev_reset_survives_the_keyed_operations_fence() {
    let fixture = load_fixture("track-grid-layout-trace.events.json");
    let (repo, bus, _state) = replay::boot_in_memory()
        .await
        .expect("boot in-memory replay state");
    replay::seed_events(&repo, &bus, &fixture)
        .await
        .expect("initial seed");

    // The only state that can trip the fence.
    sqlx::query(
        r#"INSERT INTO operations (
             id, operation_key, kind, idempotency_key, payload_hash,
             target_type, target_json, payload_json, phase,
             created_at_ms, updated_at_ms
           ) VALUES ('op-keyed', 'op-keyed', 'planner-harness-start', 'a-key', 'hash',
                     'track', '{}', '{}', 'succeeded', 0, 0)"#,
    )
    .execute(repo.pool())
    .await
    .expect("plant a keyed operations row");

    replay::reset_from_fixture(&repo, &bus, &fixture)
        .await
        .expect("/dev/reset must not abort on the keyed operations fence");

    let surviving: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM operations")
        .fetch_one(repo.pool())
        .await
        .expect("count operations after reset");
    assert_eq!(
        surviving, 1,
        "the keyed row is permanent and the reset never claimed to wipe it"
    );
}

#[tokio::test]
async fn schema_version_future_dropped_on_both_replay_and_rest_read() {
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use calm_server::model::NewOverlay;
    use calm_server::validation::{
        OVERLAY_STATUS_SCHEMA_VERSION, max_supported_overlay_schema_version, payload_schema_version,
    };
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    let fixture = load_fixture("schema_forward_compat.events.json");

    let v1_payload = &fixture.events[2].payload["payload"];
    let v999_payload = &fixture.events[3].payload["payload"];
    assert_eq!(
        payload_schema_version(v1_payload),
        1,
        "missing schemaVersion must read as 1 (backward-compat default)"
    );
    assert_eq!(
        payload_schema_version(v999_payload),
        999,
        "explicit schemaVersion=999 must round-trip through the helper"
    );
    assert_eq!(
        max_supported_overlay_schema_version("status"),
        Some(OVERLAY_STATUS_SCHEMA_VERSION),
        "the kernel's status overlay support ceiling drives what the read guard accepts"
    );

    let (addr, repo, bus) = boot().await;
    let ids = raw_insert_fixture_events(&repo, &bus, &fixture).await;
    assert_eq!(ids.len(), fixture.events.len(), "seed inserted all events");

    let db_rows = repo
        .events_since(0, i64::MAX)
        .await
        .expect("events_since after seed");
    assert_eq!(
        db_rows.len(),
        fixture.events.len(),
        "events_since(0) returns every seeded row; seed-layer drift"
    );
    let db_overlay_count = db_rows
        .iter()
        .filter(|(_, _, _, ev)| ev.kind_tag() == "overlay.set")
        .count();
    assert_eq!(
        db_overlay_count, 2,
        "both overlay.set rows are persisted in the events log; the v999 row \
         is expected to be dropped on the WS replay path (#220), not on the seed."
    );

    let url = format!("ws://{}/api/events", addr);
    let (mut ws, _) = tokio_tungstenite::connect_async(&url)
        .await
        .expect("connect ws");
    ws.send(TMessage::Text(r#"{"sub":["*"], "since": 0}"#.to_string()))
        .await
        .expect("send sub");

    let mut all_frames: Vec<serde_json::Value> = Vec::new();
    let mut overlay_frames: Vec<serde_json::Value> = Vec::new();
    loop {
        let frame = recv_json(&mut ws).await;
        all_frames.push(frame.clone());
        if frame["ev"] == "overlay.set" {
            overlay_frames.push(frame.clone());
        }
        if frame["ev"] == "_replay_complete" {
            break;
        }
    }
    assert_eq!(
        overlay_frames.len(),
        1,
        "post-#220: WS replay drops the v999 overlay.set; only the v1 frame \
         (ov-v1) should reach the client. Got {} overlay.set frames in the \
         replay window. All frames: {:#?}",
        overlay_frames.len(),
        all_frames,
    );
    let v1_frame = overlay_frames
        .iter()
        .find(|f| f["data"]["id"] == "ov-v1")
        .expect("v1 overlay frame present in replay");
    assert!(
        overlay_frames.iter().all(|f| f["data"]["id"] != "ov-v999"),
        "v999 overlay frame must NOT appear in replay — post-#220 the WS \
         path filters unsupported future-version `Event::OverlaySet` rows. \
         All frames: {:#?}",
        all_frames,
    );
    // The v1 row genuinely has no schemaVersion key: guards against a regression that coerces missing → 1 on the wire.
    assert!(
        v1_frame["data"]["payload"].get("schemaVersion").is_none(),
        "v1 frame retains its missing-field shape (not coerced to {{schemaVersion: 1}})"
    );

    // The write path's `validate_overlay_payload` would refuse the v999 payload, so go through the repo directly.
    // Two distinct overlay `kind`s so they coexist under the (plugin_id, entity_kind, entity_id, kind) unique key.
    repo.overlay_upsert(NewOverlay {
        plugin_id: "core".into(),
        entity_kind: "track".into(),
        entity_id: "track-fwd".into(),
        kind: "status".into(),
        payload: serde_json::json!({"state": "ok"}),
    })
    .await
    .expect("upsert v1 status overlay (no schemaVersion)");
    repo.overlay_upsert(NewOverlay {
        plugin_id: "core".into(),
        entity_kind: "track".into(),
        entity_id: "track-fwd".into(),
        kind: "progress".into(),
        payload: serde_json::json!({
            "value": 0.5,
            "schemaVersion": 999,
            "fromFuture": "yes",
        }),
    })
    .await
    .expect("upsert v999 progress overlay (future-kernel simulation)");

    // The raw repo returns BOTH rows: the filter is a route-layer concern.
    let raw = repo
        .overlays_for("track", "track-fwd")
        .await
        .expect("repo overlays_for");
    assert_eq!(
        raw.len(),
        2,
        "repo.overlays_for returns the raw row count (filter happens at the route layer)"
    );

    // `boot()` stands up a WS-only router; the REST arm needs the full stack over the same repo.
    let app = build_full_app(repo.clone(), bus.clone());

    let resp = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri("/api/overlays?entity_kind=track&entity_id=track-fwd")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let listed: Vec<serde_json::Value> = serde_json::from_slice(&bytes).expect("decode list");
    assert_eq!(
        listed.len(),
        1,
        "read-side guard must drop the v999 row (kernel-owned `progress` kind, future version); \
         got {} rows: {:?}",
        listed.len(),
        listed,
    );
    assert_eq!(
        listed[0]["kind"], "status",
        "the surviving row is the v1-shaped status overlay (no schemaVersion field)"
    );
    assert!(
        listed[0]["payload"].get("schemaVersion").is_none(),
        "the v1 row reaches the client without an added schemaVersion field"
    );
}

/// The full kernel HTTP router against an existing in-memory `SqlxRepo` and `EventBus`.
fn build_full_app(repo: Arc<calm_server::db::sqlite::SqlxRepo>, events: EventBus) -> axum::Router {
    use calm_server::card_role_cache::CardRoleCache;
    use calm_server::plugin_host::{PluginHost, PluginRegistry};
    use calm_server::routes;
    use calm_server::state::{AppState, CodexClient, DaemonClient};

    let card_role_cache = CardRoleCache::new();
    let track_area_cache = calm_server::track_area_cache::TrackAreaCache::new();
    let plugin = Arc::new(PluginHost::new_full(
        Arc::new(PluginRegistry::empty()),
        repo.clone(),
        std::path::PathBuf::new(),
        std::env::temp_dir().join("calm-plugins-data-schema-fwd"),
        Vec::new(),
        events.clone(),
        calm_server::state::WriteContext::new(card_role_cache.clone(), track_area_cache.clone()),
    ));
    let state = AppState::from_parts(
        repo,
        events,
        Arc::new(DaemonClient::new_stub()),
        plugin,
        Arc::new(CodexClient::new_stub()),
        Some(card_role_cache),
        Some(track_area_cache.clone()),
    );
    routes::router()
        .layer(axum::middleware::from_fn(
            calm_server::actor::actor_middleware,
        ))
        .with_state(state)
}

/// An event-linked receipt must leave before its event in the reset.
#[tokio::test]
async fn reset_from_fixture_with_candidate_decision_receipt() {
    let fixture = load_fixture("track-grid-layout-trace.events.json");
    let (repo, bus, _state) = replay::boot_in_memory().await.unwrap();
    let ids = replay::seed_events(&repo, &bus, &fixture).await.unwrap();
    seed_rooted_track(&repo).await;
    let track: String = sqlx::query_scalar("SELECT id FROM tracks LIMIT 1")
        .fetch_one(repo.pool())
        .await
        .unwrap();
    sqlx::query("INSERT INTO task_candidate_decisions(event_id,track_id,producer_attempt_id,event_json) VALUES(?1,?2,'retained-producer','{}')").bind(ids[0]).bind(track).execute(repo.pool()).await.unwrap();
    for (id, kind) in [
        ("reset-publication", "task-file-publication"),
        ("reset-verification", "candidate-verify"),
    ] {
        sqlx::query("INSERT INTO operations(id,operation_key,kind,payload_hash,target_type,target_json,payload_json,phase,created_at_ms,updated_at_ms) VALUES(?1,?1,?2,'fixture','card','{}','{}','succeeded',1,1)").bind(id).bind(kind).execute(repo.pool()).await.unwrap();
    }
    sqlx::query("INSERT INTO task_file_candidates(operation_id,track_id,producer_attempt_id,slot,candidate_json) SELECT 'reset-publication',track_id,'retained-producer','project','{}' FROM task_candidate_decisions").execute(repo.pool()).await.unwrap();
    sqlx::query("INSERT INTO task_candidate_input_bindings(attempt_id,track_id,publication_operation_id,verification_operation_id,binding_json,state) SELECT 'reset-consumer',track_id,'reset-publication','reset-verification','{}','bound' FROM task_candidate_decisions").execute(repo.pool()).await.unwrap();
    sqlx::query("INSERT INTO task_candidate_decision_bindings(attempt_id,decision_event_id) VALUES('reset-consumer',?1)").bind(ids[0]).execute(repo.pool()).await.unwrap();
    let reset = replay::reset_from_fixture(&repo, &bus, &fixture)
        .await
        .expect("production reset must delete candidate receipts before events");
    assert_eq!(reset[0], 1);
    let remaining: i64 = sqlx::query_scalar("SELECT count(*) FROM task_candidate_decisions")
        .fetch_one(repo.pool())
        .await
        .unwrap();
    assert_eq!(remaining, 0);
}
