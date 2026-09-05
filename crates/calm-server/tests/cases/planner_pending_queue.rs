//! #1505 PR1 — queue identity end to end.
//!
//! The unit layer (`harness::queue`, `harness::snapshot`) pins the shapes. What
//! this file pins is the wiring those tests cannot see: that a `POST
//! /planner/input` hands back the id of the entry the text actually landed in,
//! that `GET /planner/run` shows the same id while the entry is still queued,
//! that the id survives a restart, and — the one that matters most — that a
//! queue entry written before this slice is never silently given a new id on
//! the way through.

use std::path::PathBuf;
use std::sync::Arc;

use axum::http::StatusCode;
use calm_server::card_role_cache::CardRoleCache;
use calm_server::db::prelude::*;
use calm_server::db::sqlite::{SqlxRepo, card_create_with_id_tx};
use calm_server::event::EventBus;
use calm_server::harness::{
    HARNESS_MODE, HarnessSnapshot, MAX_PENDING_QUEUE_LEN, Observation, QueueEntry,
};
use calm_server::model::{CardRole, NewArea, NewCard, NewTrack, new_id};
use calm_server::plugin_host::{PluginHost, PluginRegistry};
use calm_server::routes;
use calm_server::state::{AppState, CodexClient, DaemonClient};
use calm_server::track_area_cache::TrackAreaCache;
use serde_json::{Value, json};

use crate::support::planner_queue_fixture::{
    SEED_THREAD_ID, boot_with, get, idle_snapshot, post_input,
};

/// The main acceptance path: the id the sender is handed is the id the queue
/// shows, and the entry carries its complete text.
#[tokio::test]
async fn posted_input_returns_the_entry_id_that_planner_run_then_lists() {
    let boot = boot_with(idle_snapshot(vec![])).await;
    let card_id = boot.planner_card.id.as_str().to_string();

    let (status, posted) =
        post_input(boot.app.clone(), &card_id, "please look at the report").await;
    assert_eq!(status, StatusCode::OK, "body={posted}");
    let entry_id = posted["entry_id"]
        .as_str()
        .expect("a live harness always acks with an id")
        .to_string();
    assert!(!entry_id.is_empty());

    let (status, run) = get(
        boot.app.clone(),
        format!("/api/cards/{card_id}/planner/run"),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body={run}");
    let pending = run["pending"].as_array().expect("pending is an array");
    assert_eq!(pending.len(), 1, "run={run}");
    assert_eq!(pending[0]["entry_id"], json!(entry_id));
    assert_eq!(pending[0]["text"], json!("please look at the report"));
    assert_eq!(pending[0]["rev"], json!(0));
    assert!(
        pending[0]["queued_at_ms"].as_i64().unwrap_or(0) > 0,
        "a queued entry is stamped with a real wall clock"
    );
    assert_eq!(run["pending_overflow"], json!(0));
}

/// The id is minted once and persisted, not re-derived per read: it survives
/// the snapshot round trip that a restart replays.
#[tokio::test]
async fn a_queue_entry_id_survives_a_snapshot_round_trip() {
    let boot = boot_with(idle_snapshot(vec![])).await;
    let card_id = boot.planner_card.id.as_str().to_string();
    let (_, posted) = post_input(boot.app.clone(), &card_id, "outlive the restart").await;
    let entry_id = posted["entry_id"].as_str().expect("id").to_string();

    // What boot recovery does: serialize the live snapshot, read it back
    // strictly, and hand it to a fresh harness.
    let persisted = serde_json::to_value(boot.harness.snapshot().await).unwrap();
    let restored = HarnessSnapshot::from_value_strict(persisted);
    let entries = restored.pending_entries();

    assert_eq!(entries.len(), 1);
    assert_eq!(
        entries[0].id().map(|id| id.as_str().to_string()),
        Some(entry_id),
        "a restart must not re-mint the id the client is already holding"
    );
}

/// System observations are the user's neither to see in this list nor to
/// delete, so they are omitted — and, unlike a legacy user entry, they are NOT
/// counted as something withheld.
#[tokio::test]
async fn dispatcher_observations_are_neither_listed_nor_counted_as_overflow() {
    let boot = boot_with(idle_snapshot(vec![
        QueueEntry::system(
            Observation::TrackGoal {
                text: "the track goal".into(),
            },
            None,
        )
        .expect("a track goal is a system entry"),
    ]))
    .await;
    let card_id = boot.planner_card.id.as_str().to_string();

    let (status, run) = get(
        boot.app.clone(),
        format!("/api/cards/{card_id}/planner/run"),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body={run}");
    assert_eq!(run["pending"], json!([]));
    assert_eq!(
        run["pending_overflow"],
        json!(0),
        "a system observation is not a user entry being withheld"
    );
}

/// §11.1 #3 at the HTTP layer: an entry from a snapshot written before this
/// slice is neither shown nor addressable, and it is honestly counted.
#[tokio::test]
async fn pre_1505_queue_entries_are_withheld_and_counted_not_minted() {
    let legacy = json!({
        "schema_version": 1,
        "mode": HARNESS_MODE,
        "phase": "idle",
        "push_watermark": 0,
        "pending_queue": [{"type": "user_message", "text": "sent before PR1"}],
        "pending_envelope_ids": [null],
        "last_thread_id": SEED_THREAD_ID,
    });
    assert!(
        legacy.get("pending_entry_meta").is_none(),
        "the point of this literal is the ABSENT key"
    );
    let boot = boot_with(HarnessSnapshot::from_value_strict(legacy)).await;
    let card_id = boot.planner_card.id.as_str().to_string();

    let (status, run) = get(
        boot.app.clone(),
        format!("/api/cards/{card_id}/planner/run"),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body={run}");
    assert_eq!(
        run["pending"],
        json!([]),
        "an entry with no id cannot be offered edit/delete buttons"
    );
    assert_eq!(
        run["pending_overflow"],
        json!(1),
        "but the user is told something is queued that cannot be shown"
    );

    // And it is still legacy in the live queue — nothing on the read path
    // repaired it.
    let entries = boot.harness.snapshot().await.pending_entries();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].id(), None, "no id was minted on the way through");
}

/// A dormant card answers the read with an empty page rather than an error or
/// a null: the queue UI has nothing to draw, which is different from "we do
/// not know".
#[tokio::test]
async fn a_card_with_no_runtime_reports_an_empty_pending_page() {
    let repo = Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap());
    let area = repo
        .area_create(NewArea {
            name: "dormant".into(),
            color: "#222222".into(),
            sort: None,
        })
        .await
        .unwrap();
    let track = repo
        .track_create(NewTrack {
            template_input: None,
            area_id: area.id.clone(),
            title: "dormant".into(),
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
    track_area_cache.insert(track.id.clone(), area.id);
    let mut tx = repo.pool().begin().await.unwrap();
    let card = card_create_with_id_tx(
        &mut tx,
        new_id(),
        NewCard {
            track_id: track.id.clone(),
            title: None,
            kind: "codex".into(),
            sort: None,
            payload: json!({"schemaVersion": 1, "planner_harness": true}),
        },
        CardRole::Planner,
        false,
        &role_cache,
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();

    let state = AppState::from_parts(
        repo.clone(),
        EventBus::new(),
        Arc::new(DaemonClient::new_stub()),
        Arc::new(PluginHost::new_full(
            Arc::new(PluginRegistry::empty()),
            repo.clone(),
            PathBuf::new(),
            std::env::temp_dir().join("calm-plugins-data-dormant-queue"),
            Vec::new(),
            EventBus::new(),
            calm_server::state::WriteContext::new(role_cache.clone(), track_area_cache.clone()),
        )),
        Arc::new(CodexClient::new_stub()),
        Some(role_cache.clone()),
        Some(track_area_cache),
    );
    let app = routes::router()
        .layer(axum::middleware::from_fn(
            calm_server::actor::actor_middleware,
        ))
        .with_state(state);

    let (status, run) = get(app, format!("/api/cards/{}/planner/run", card.id.as_str())).await;
    assert_eq!(status, StatusCode::OK, "body={run}");
    assert_eq!(run["worker_session_id"], Value::Null);
    assert_eq!(run["phase"], Value::Null);
    assert_eq!(run["pending"], json!([]));
    assert_eq!(run["pending_overflow"], json!(0));
}

/// Two sends, two ids, both listed in the order they were queued.
#[tokio::test]
async fn two_sends_get_distinct_ids_in_queue_order() {
    let boot = boot_with(idle_snapshot(vec![])).await;
    let card_id = boot.planner_card.id.as_str().to_string();

    let (_, first) = post_input(boot.app.clone(), &card_id, "first").await;
    let (_, second) = post_input(boot.app.clone(), &card_id, "second").await;
    let first_id = first["entry_id"].as_str().expect("id").to_string();
    let second_id = second["entry_id"].as_str().expect("id").to_string();
    assert_ne!(first_id, second_id);

    let (_, run) = get(
        boot.app.clone(),
        format!("/api/cards/{card_id}/planner/run"),
    )
    .await;
    let pending = run["pending"].as_array().expect("array");
    assert_eq!(pending.len(), 2, "run={run}");
    assert_eq!(pending[0]["entry_id"], json!(first_id));
    assert_eq!(pending[1]["entry_id"], json!(second_id));
    assert_eq!(run["worker_session_id"], json!(boot.worker_session_id));
}

/// §11.5 #17 at the wire, not just in the unit tests — the one accepted way a
/// caller is told `entry_id: null`.
///
/// PR4's placeholder rule keys on exactly this value, so where `null` can come
/// from is an input to that design rather than an implementation detail: a
/// dormant harness, a 503, a 409, and this — a send that folded into a queue
/// entry written before #1505 PR1, which has no id and never gains one. The
/// first three are refusals with their own status codes; this is the only
/// `null` on a 200.
#[tokio::test]
async fn folding_onto_a_pre_1505_tail_answers_with_a_null_entry_id() {
    // A full queue whose tail is a legacy entry: `pending_queue` holds user
    // messages and `pending_entry_meta` is absent, exactly as a pre-PR1 binary
    // wrote it.
    let queued: Vec<Value> = (0..MAX_PENDING_QUEUE_LEN)
        .map(|i| json!({"type": "user_message", "text": format!("queued before PR1 #{i}")}))
        .collect();
    let legacy = json!({
        "schema_version": 1,
        "mode": HARNESS_MODE,
        "phase": "idle",
        "push_watermark": 0,
        "pending_queue": queued,
        "pending_envelope_ids": vec![Value::Null; MAX_PENDING_QUEUE_LEN],
        "last_thread_id": SEED_THREAD_ID,
    });
    let boot = boot_with(HarnessSnapshot::from_value_strict(legacy)).await;
    let card_id = boot.planner_card.id.as_str().to_string();

    let (status, posted) = post_input(boot.app.clone(), &card_id, "folded onto an old entry").await;

    assert_eq!(status, StatusCode::OK, "body={posted}");
    assert_eq!(
        posted["entry_id"],
        Value::Null,
        "the surviving entry has no id to name, and naming the discarded one \
         would point the client at an entry that does not exist"
    );

    let entries = boot.harness.snapshot().await.pending_entries();
    assert_eq!(
        entries.len(),
        MAX_PENDING_QUEUE_LEN,
        "the send folded into the tail rather than taking a slot of its own"
    );
    let tail = entries.last().expect("a full queue has a tail");
    assert_eq!(tail.id(), None, "and the tail did not become addressable");
    assert_eq!(
        tail.observation(),
        Observation::UserMessage {
            text: format!(
                "queued before PR1 #{}\n\nfolded onto an old entry",
                MAX_PENDING_QUEUE_LEN - 1
            )
        },
        "the text is preserved on both sides of the fold"
    );

    let (_, run) = get(
        boot.app.clone(),
        format!("/api/cards/{card_id}/planner/run"),
    )
    .await;
    assert_eq!(run["pending"], json!([]), "none of them are addressable");
    assert_eq!(
        run["pending_overflow"],
        json!(MAX_PENDING_QUEUE_LEN),
        "but every one of them is counted"
    );
}
