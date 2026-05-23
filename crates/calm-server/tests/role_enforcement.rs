//! PR3 (#136) end-to-end role-gate coverage.
//!
//! These tests exercise the `enforce_role` gate from the public write
//! surface (`Repo::write_with_event` / `Repo::log_pure_event`) — not the
//! pure-function unit tests in `crate::role_gate::tests`, which sit one
//! layer below the SQL. We want to pin:
//!
//!   * a `spec`-roled card can update its wave through the audited write
//!     path, the events row lands, and the bus broadcast fires;
//!   * an `AiCodex(other_card)` attempting the same write is refused
//!     before the event row is appended — neither the events table
//!     gains a row nor the broadcast goes out;
//!   * the migration 0008 backfill (every existing card → 'plain') is
//!     observable at the database level after `SqlxRepo::open` runs.

use std::sync::Arc;

use calm_server::card_role_cache::CardRoleCache;
use calm_server::db::prelude::*;
use calm_server::db::sqlite::{SqlxRepo, wave_update_tx};
use calm_server::db::write_with_event_typed;
use calm_server::event::{Event, EventBus, EventScope};
use calm_server::ids::{ActorId, CardId};
use calm_server::model::{CardRole, NewCove, NewWave, WavePatch};
use calm_server::wave_cove_cache::WaveCoveCache;

async fn boot() -> (Arc<SqlxRepo>, EventBus) {
    let repo = Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap());
    let bus = EventBus::new();
    (repo, bus)
}

/// PR3 happy path: a card whose `cards.role = 'spec'` (we set this via
/// direct SQL today — PR6 will mint spec cards from the wave-create
/// path) is permitted to emit `WaveUpdated` through the audited write
/// surface. The events row lands and the bus broadcast fires.
#[tokio::test]
async fn spec_card_can_update_wave() {
    let (repo, bus) = boot().await;
    let mut sub = bus.subscribe();

    let cove = repo
        .cove_create(NewCove {
            name: "c".into(),
            color: "#fff".into(),
            sort: None,
        })
        .await
        .unwrap();
    let wave = repo
        .wave_create(NewWave {
            cove_id: cove.id.clone(),
            title: "w".into(),
            sort: None,
        })
        .await
        .unwrap();
    let card = repo
        .card_create(calm_server::model::NewCard {
            wave_id: wave.id.clone(),
            kind: "spec".into(),
            sort: None,
            payload: serde_json::json!({}),
        })
        .await
        .unwrap();
    // Promote to spec role at the SQL layer (PR6 territory — PR3 just
    // wires the gate).
    sqlx::query("UPDATE cards SET role = 'spec' WHERE id = ?1")
        .bind(card.id.as_str())
        .execute(repo.pool())
        .await
        .unwrap();

    // Re-seed the role cache so it sees the new spec role.
    let cache = CardRoleCache::new();
    let wcc = WaveCoveCache::new();
    repo.seed_wave_cove_cache(&wcc).await.unwrap();
    repo.seed_card_role_cache(&cache).await.unwrap();
    assert_eq!(cache.get(&card.id), Some(CardRole::Spec));

    let scope = EventScope::Wave {
        wave: wave.id.clone(),
        cove: cove.id.clone(),
    };
    let wave_id_for_tx = wave.id.clone();
    let res = write_with_event_typed(
        repo.as_ref(),
        ActorId::AiSpec(card.id.clone()),
        scope,
        None,
        &bus,
        &cache,
        &wcc,
        move |tx| {
            Box::pin(async move {
                let w = wave_update_tx(
                    tx,
                    wave_id_for_tx.as_str(),
                    WavePatch {
                        title: Some("renamed".into()),
                        ..Default::default()
                    },
                )
                .await?;
                Ok((w.clone(), Event::WaveUpdated(w)))
            })
        },
    )
    .await;
    assert!(res.is_ok(), "spec-card wave update should succeed: {res:?}");

    // Confirm the event row landed.
    let row: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM events WHERE kind = 'wave.updated'")
        .fetch_one(repo.pool())
        .await
        .unwrap();
    assert_eq!(row.0, 1, "exactly one wave.updated row");

    // Bus saw the envelope.
    let env = sub.try_recv().expect("envelope on bus");
    matches!(env.event, Event::WaveUpdated(_));
}

/// PR3 deny path: an `AiCodex(other_card)` actor attempting a
/// `WaveUpdated` write is refused by the gate. Events table holds no
/// new row; no broadcast fires.
#[tokio::test]
async fn ai_codex_cannot_update_wave() {
    let (repo, bus) = boot().await;
    let mut sub = bus.subscribe();

    let cove = repo
        .cove_create(NewCove {
            name: "c".into(),
            color: "#fff".into(),
            sort: None,
        })
        .await
        .unwrap();
    let wave = repo
        .wave_create(NewWave {
            cove_id: cove.id.clone(),
            title: "w".into(),
            sort: None,
        })
        .await
        .unwrap();
    let card = repo
        .card_create(calm_server::model::NewCard {
            wave_id: wave.id.clone(),
            kind: "codex".into(),
            sort: None,
            payload: serde_json::json!({}),
        })
        .await
        .unwrap();
    // Card stays Plain — PR3 never mints workers, but even a Plain
    // codex card is denied for wave.updated.

    let cache = CardRoleCache::new();
    let wcc = WaveCoveCache::new();
    repo.seed_wave_cove_cache(&wcc).await.unwrap();
    repo.seed_card_role_cache(&cache).await.unwrap();

    let baseline_events: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM events")
        .fetch_one(repo.pool())
        .await
        .unwrap();

    let scope = EventScope::Wave {
        wave: wave.id.clone(),
        cove: cove.id.clone(),
    };
    let wave_id_for_tx = wave.id.clone();
    let title_before = wave.title.clone();
    let res = write_with_event_typed(
        repo.as_ref(),
        ActorId::AiCodex(card.id.clone()),
        scope,
        None,
        &bus,
        &cache,
        &wcc,
        move |tx| {
            Box::pin(async move {
                let w = wave_update_tx(
                    tx,
                    wave_id_for_tx.as_str(),
                    WavePatch {
                        title: Some("hijack".into()),
                        ..Default::default()
                    },
                )
                .await?;
                Ok((w.clone(), Event::WaveUpdated(w)))
            })
        },
    )
    .await;
    assert!(
        matches!(
            res,
            Err(calm_server::error::CalmError::Forbidden(ref msg))
                if msg.contains("only spec cards")
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

    // The wave's title is unchanged in the database — the rolled-back
    // transaction took the UPDATE with it.
    let fetched = repo.wave_get(wave.id.as_str()).await.unwrap().unwrap();
    assert_eq!(fetched.title, title_before, "wave row not mutated");

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
    let wcc = WaveCoveCache::new();
    repo.seed_wave_cove_cache(&wcc).await.unwrap();
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

/// Migration smoke test: after `SqlxRepo::open` runs, every existing
/// `cards` row carries `role = 'plain'`. We can't truly simulate a
/// pre-PR3 database (the migration ladder must run forward); instead
/// the assertion is that the migration's `DEFAULT 'plain'` is honored
/// for every new card insert.
#[tokio::test]
async fn migration_backfills_role_plain_for_existing_cards() {
    let repo = SqlxRepo::open("sqlite::memory:").await.unwrap();
    // Seed a card via the public API (uses the migrated column).
    let cove = repo
        .cove_create(NewCove {
            name: "c".into(),
            color: "#fff".into(),
            sort: None,
        })
        .await
        .unwrap();
    let wave = repo
        .wave_create(NewWave {
            cove_id: cove.id,
            title: "w".into(),
            sort: None,
        })
        .await
        .unwrap();
    let card = repo
        .card_create(calm_server::model::NewCard {
            wave_id: wave.id,
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
    assert_eq!(row.0, "plain");
}

/// Migration smoke test: the partial unique index that constrains "one
/// spec card per wave" actually rejects duplicates. PR6 will rely on
/// this as a backstop in case the application-level mint races itself.
#[tokio::test]
async fn unique_spec_card_per_wave_index_enforced() {
    let repo = SqlxRepo::open("sqlite::memory:").await.unwrap();
    let cove = repo
        .cove_create(NewCove {
            name: "c".into(),
            color: "#fff".into(),
            sort: None,
        })
        .await
        .unwrap();
    let wave = repo
        .wave_create(NewWave {
            cove_id: cove.id,
            title: "w".into(),
            sort: None,
        })
        .await
        .unwrap();
    // Two cards, both role=spec.
    let c1 = repo
        .card_create(calm_server::model::NewCard {
            wave_id: wave.id.clone(),
            kind: "spec".into(),
            sort: None,
            payload: serde_json::json!({}),
        })
        .await
        .unwrap();
    let c2 = repo
        .card_create(calm_server::model::NewCard {
            wave_id: wave.id.clone(),
            kind: "spec".into(),
            sort: None,
            payload: serde_json::json!({}),
        })
        .await
        .unwrap();
    // Promote c1 — fine.
    sqlx::query("UPDATE cards SET role = 'spec' WHERE id = ?1")
        .bind(c1.id.as_str())
        .execute(repo.pool())
        .await
        .unwrap();
    // Promote c2 — must violate the partial unique index.
    let err = sqlx::query("UPDATE cards SET role = 'spec' WHERE id = ?1")
        .bind(c2.id.as_str())
        .execute(repo.pool())
        .await
        .expect_err("second spec card must violate unique index");
    let msg = err.to_string();
    assert!(
        msg.contains("UNIQUE")
            || msg.contains("constraint")
            || msg.contains("idx_cards_one_spec_per_wave"),
        "expected unique-index violation, got: {msg}"
    );
}
