//! PR3 (#136) end-to-end role-gate coverage.
//!
//! These tests exercise the `enforce_role` gate from the public write
//! surface (`Repo::write_with_event` / `Repo::log_pure_event`) — not the
//! pure-function unit tests in `crate::role_gate::tests`, which sit one
//! layer below the SQL. We want to pin:
//!
//!   * a `planner`-roled card can update its track through the audited write
//!     path, the events row lands, and the bus broadcast fires;
//!   * an `AiCodex(other_card)` attempting the same write is refused
//!     before the event row is appended — neither the events table
//!     gains a row nor the broadcast goes out;
//!   * the public card-create path writes the current default `worker`
//!     role instead of relying on the legacy SQLite default.

use std::sync::Arc;

use calm_server::card_role_cache::CardRoleCache;
use calm_server::db::prelude::*;
use calm_server::db::sqlite::{SqlxRepo, track_update_tx};
use calm_server::db::write_with_event_typed;
use calm_server::event::{Event, EventBus, EventScope};
use calm_server::ids::{ActorId, CardId};
use calm_server::model::{CardRole, NewArea, NewTrack, TrackPatch};
use calm_server::track_area_cache::TrackAreaCache;

async fn boot() -> (Arc<SqlxRepo>, EventBus) {
    let repo = Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap());
    let bus = EventBus::new();
    (repo, bus)
}

/// PR3 happy path: a card whose `cards.role = 'planner'` (we set this via
/// direct SQL today — PR6 will mint planner cards from the track-create
/// path) is permitted to emit `TrackUpdated` through the audited write
/// surface. The events row lands and the bus broadcast fires.
#[tokio::test]
async fn planner_card_can_update_track() {
    let (repo, bus) = boot().await;
    let mut sub = bus.subscribe();

    let area = repo
        .area_create(NewArea {
            name: "c".into(),
            color: "#fff".into(),
            sort: None,
        })
        .await
        .unwrap();
    let track = repo
        .track_create(NewTrack {
            template_input: None,
            area_id: area.id.clone(),
            title: "w".into(),
            sort: None,
            cwd: String::new(),
            template_id: None,
            plugin_scope: None,
            attach_folder: false,
            theme: calm_server::routes::theme::RequestTheme::default_dark(),
        })
        .await
        .unwrap();
    let card = repo
        .card_create(calm_server::model::NewCard {
            track_id: track.id.clone(),
            title: None,
            kind: "planner".into(),
            sort: None,
            payload: serde_json::json!({}),
        })
        .await
        .unwrap();
    // Promote to planner role at the SQL layer (PR6 territory — PR3 just
    // wires the gate).
    sqlx::query("UPDATE cards SET role = 'planner' WHERE id = ?1")
        .bind(card.id.as_str())
        .execute(repo.pool())
        .await
        .unwrap();

    // Re-seed the role cache so it sees the new planner role.
    let cache = CardRoleCache::new();
    let wcc = TrackAreaCache::new();
    repo.seed_track_area_cache(&wcc).await.unwrap();
    repo.seed_card_role_cache(&cache).await.unwrap();
    assert_eq!(cache.get(&card.id), Some(CardRole::Planner));

    let scope = EventScope::Track {
        track: track.id.clone(),
        area: area.id.clone(),
    };
    let track_id_for_tx = track.id.clone();
    let res = write_with_event_typed(
        repo.as_ref(),
        ActorId::AiPlanner(card.id.clone()),
        scope,
        None,
        &bus,
        &calm_server::state::WriteContext::new(cache.clone(), wcc.clone()),
        move |tx| {
            Box::pin(async move {
                let w = track_update_tx(
                    tx,
                    track_id_for_tx.as_str(),
                    TrackPatch {
                        title: Some("renamed".into()),
                        ..Default::default()
                    },
                )
                .await?;
                Ok((
                    w.clone(),
                    Event::TrackUpdated(calm_server::event::TrackUpdatedPayload::new(w, None)),
                ))
            })
        },
    )
    .await;
    assert!(
        res.is_ok(),
        "planner-card track update should succeed: {res:?}"
    );

    // Confirm the event row landed.
    let row: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM events WHERE kind = 'track.updated'")
        .fetch_one(repo.pool())
        .await
        .unwrap();
    assert_eq!(row.0, 1, "exactly one track.updated row");

    // Bus saw the envelope.
    let env = sub.try_recv().expect("envelope on bus");
    matches!(env.event, Event::TrackUpdated(_));
}

/// PR3 deny path: an `AiCodex(other_card)` actor attempting a
/// `TrackUpdated` write is refused by the gate. Events table holds no
/// new row; no broadcast fires.
#[tokio::test]
async fn ai_codex_cannot_update_track() {
    let (repo, bus) = boot().await;
    let mut sub = bus.subscribe();

    let area = repo
        .area_create(NewArea {
            name: "c".into(),
            color: "#fff".into(),
            sort: None,
        })
        .await
        .unwrap();
    let track = repo
        .track_create(NewTrack {
            template_input: None,
            area_id: area.id.clone(),
            title: "w".into(),
            sort: None,
            cwd: String::new(),
            template_id: None,
            plugin_scope: None,
            attach_folder: false,
            theme: calm_server::routes::theme::RequestTheme::default_dark(),
        })
        .await
        .unwrap();
    let card = repo
        .card_create(calm_server::model::NewCard {
            track_id: track.id.clone(),
            title: None,
            kind: "codex".into(),
            sort: None,
            payload: serde_json::json!({}),
        })
        .await
        .unwrap();
    // Worker codex cards are denied for track.updated.

    let cache = CardRoleCache::new();
    let wcc = TrackAreaCache::new();
    repo.seed_track_area_cache(&wcc).await.unwrap();
    repo.seed_card_role_cache(&cache).await.unwrap();

    let baseline_events: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM events")
        .fetch_one(repo.pool())
        .await
        .unwrap();

    let scope = EventScope::Track {
        track: track.id.clone(),
        area: area.id.clone(),
    };
    let track_id_for_tx = track.id.clone();
    let title_before = track.title.clone();
    let res = write_with_event_typed(
        repo.as_ref(),
        ActorId::AiCodex(card.id.clone()),
        scope,
        None,
        &bus,
        &calm_server::state::WriteContext::new(cache.clone(), wcc.clone()),
        move |tx| {
            Box::pin(async move {
                let w = track_update_tx(
                    tx,
                    track_id_for_tx.as_str(),
                    TrackPatch {
                        title: Some("hijack".into()),
                        ..Default::default()
                    },
                )
                .await?;
                Ok((
                    w.clone(),
                    Event::TrackUpdated(calm_server::event::TrackUpdatedPayload::new(w, None)),
                ))
            })
        },
    )
    .await;
    assert!(
        matches!(
            res,
            Err(calm_server::error::CalmError::Forbidden(ref msg))
                if msg.contains("only planner cards")
        ),
        "AiCodex should be refused with Forbidden: {res:?}"
    );

    // Events table unchanged.
    let after_events: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM events")
        .fetch_one(repo.pool())
        .await
        .unwrap();
    assert_eq!(
        after_events.0, baseline_events.0,
        "events table must not gain rows on a denied write"
    );

    // The track's title is unchanged in the database — the rolled-back
    // transaction took the UPDATE with it.
    let fetched = repo.track_get(track.id.as_str()).await.unwrap().unwrap();
    assert_eq!(fetched.title, title_before, "track row not mutated");

    // Bus saw nothing for this attempt.
    assert!(
        sub.try_recv().is_err(),
        "no broadcast should fire for denied write"
    );
}

/// PR3 deny path: empty CardId on the actor (the PR2 stopgap from the
/// `X-Calm-Actor: ai:codex` header) is caught before any SQL runs.
#[tokio::test]
async fn empty_codex_card_id_rejected() {
    let (repo, bus) = boot().await;
    let cache = CardRoleCache::new();
    let wcc = TrackAreaCache::new();
    repo.seed_track_area_cache(&wcc).await.unwrap();
    repo.seed_card_role_cache(&cache).await.unwrap();
    // Pure-event path with an empty CardId actor.
    let res = repo
        .log_pure_event(
            ActorId::AiCodex(CardId::from("")),
            EventScope::System,
            None,
            &bus,
            &cache,
            &wcc,
            Event::PluginState {
                id: "plug".into(),
                state: "Running".into(),
                last_error: None,
            },
        )
        .await;
    assert!(
        matches!(
            res,
            Err(calm_server::error::CalmError::Forbidden(ref msg))
                if msg.contains("empty card id")
        ),
        "empty CardId should be refused with Forbidden: {res:?}"
    );
}

/// Public create smoke test: user-facing cards bind the current role
/// explicitly instead of relying on the legacy SQLite DEFAULT.
#[tokio::test]
async fn public_card_create_writes_worker_role() {
    let repo = SqlxRepo::open("sqlite::memory:").await.unwrap();
    // Seed a card via the public API (uses the migrated column).
    let area = repo
        .area_create(NewArea {
            name: "c".into(),
            color: "#fff".into(),
            sort: None,
        })
        .await
        .unwrap();
    let track = repo
        .track_create(NewTrack {
            template_input: None,
            area_id: area.id,
            title: "w".into(),
            sort: None,
            cwd: String::new(),
            template_id: None,
            plugin_scope: None,
            attach_folder: false,
            theme: calm_server::routes::theme::RequestTheme::default_dark(),
        })
        .await
        .unwrap();
    let card = repo
        .card_create(calm_server::model::NewCard {
            track_id: track.id,
            title: None,
            kind: "terminal".into(),
            sort: None,
            payload: serde_json::json!({}),
        })
        .await
        .unwrap();
    let row: (String,) = sqlx::query_as("SELECT role FROM cards WHERE id = ?1")
        .bind(card.id.as_str())
        .fetch_one(repo.pool())
        .await
        .unwrap();
    assert_eq!(row.0, "worker");
}

/// Migration smoke test: the partial unique index that constrains "one
/// planner card per track" actually rejects duplicates. PR6 will rely on
/// this as a backstop in case the application-level mint races itself.
#[tokio::test]
async fn unique_planner_card_per_track_index_enforced() {
    let repo = SqlxRepo::open("sqlite::memory:").await.unwrap();
    let area = repo
        .area_create(NewArea {
            name: "c".into(),
            color: "#fff".into(),
            sort: None,
        })
        .await
        .unwrap();
    let track = repo
        .track_create(NewTrack {
            template_input: None,
            area_id: area.id,
            title: "w".into(),
            sort: None,
            cwd: String::new(),
            template_id: None,
            plugin_scope: None,
            attach_folder: false,
            theme: calm_server::routes::theme::RequestTheme::default_dark(),
        })
        .await
        .unwrap();
    // Two cards, both role=planner.
    let c1 = repo
        .card_create(calm_server::model::NewCard {
            track_id: track.id.clone(),
            title: None,
            kind: "planner".into(),
            sort: None,
            payload: serde_json::json!({}),
        })
        .await
        .unwrap();
    let c2 = repo
        .card_create(calm_server::model::NewCard {
            track_id: track.id.clone(),
            title: None,
            kind: "planner".into(),
            sort: None,
            payload: serde_json::json!({}),
        })
        .await
        .unwrap();
    // Promote c1 — fine.
    sqlx::query("UPDATE cards SET role = 'planner' WHERE id = ?1")
        .bind(c1.id.as_str())
        .execute(repo.pool())
        .await
        .unwrap();
    // Promote c2 — must violate the partial unique index.
    let err = sqlx::query("UPDATE cards SET role = 'planner' WHERE id = ?1")
        .bind(c2.id.as_str())
        .execute(repo.pool())
        .await
        .expect_err("second planner card must violate unique index");
    let msg = err.to_string();
    assert!(
        msg.contains("UNIQUE")
            || msg.contains("constraint")
            || msg.contains("idx_cards_one_planner_per_track"),
        "expected unique-index violation, got: {msg}"
    );
}
