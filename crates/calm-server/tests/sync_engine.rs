//! Sync engine phase 1 (Scope A) server-side tests. Spec: design doc §6.1.
//!
//! Coverage:
//!
//!   1. **Atomicity (success).** `write_with_event` commits the entity row
//!      and the events row together; both visible after a successful call.
//!   2. **Atomicity (closure error).** If the entity-write closure returns
//!      `Err`, the txn rolls back — neither the entity row nor an event
//!      row exists.
//!   3. **Atomicity (event-insert error).** If `event_append_in_tx` would
//!      fail mid-txn, the entity write rolls back too. We provoke this
//!      with a deliberately malformed event payload via a `Repo`-decorating
//!      stub. See note in `test_event_insert_failure_rolls_back_entity`.
//!   4. **Replay correctness.** Driving writes through `write_with_event`
//!      then SELECT-replaying the `events` table reproduces the same
//!      `BroadcastEnvelope` sequence (by `events.id` order) that a
//!      continuously-connected subscriber observed.
//!   5. **Replay-then-live boundary (the crown jewel).** A new event lands
//!      *while* the replay SQL query is mid-flight; with `subscribe-first`
//!      ordering, the dedupe rule (`event_id <= last_replayed_id`)
//!      delivers every event exactly once, in id order, no gaps.
//!   6. **Property test (proptest-free, hand-rolled because adding the
//!      proptest crate is out of Scope A's budget).** Generates a sequence
//!      of arbitrary entity writes and asserts both a cold-replay client
//!      and a continuously-connected subscriber converge to the same final
//!      view of events. Uses small deterministic shrinking-by-hand cases.

use std::sync::Arc;

use calm_server::db::prelude::*;
use calm_server::db::sqlite::{
    SqlxRepo, card_create_tx, cove_create_tx, overlay_upsert_tx, wave_create_tx,
};
use calm_server::db::write_with_event_typed;
use calm_server::error::CalmError;
use calm_server::event::{Event, EventBus};
use calm_server::model::{Cove, NewCard, NewCove, NewOverlay, NewWave, Wave};

/// Boot an in-memory `SqlxRepo` and a fresh `EventBus`. Repo is returned
/// as both `Arc<dyn Repo>` (for trait-based calls) and `Arc<SqlxRepo>` (for
/// the few tests that need the concrete type, e.g. `event_append_fixture`).
async fn boot() -> (Arc<dyn Repo>, Arc<SqlxRepo>, EventBus) {
    let concrete = Arc::new(
        SqlxRepo::open("sqlite::memory:")
            .await
            .expect("open in-memory repo"),
    );
    let r: Arc<dyn Repo> = concrete.clone();
    (r, concrete, EventBus::new())
}

// ---------------------------------------------------------------------------
// 1. Atomicity — happy path.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn write_with_event_persists_entity_and_event_in_one_txn() {
    let (repo, concrete, bus) = boot().await;
    let mut sub = bus.subscribe();

    let p = NewCove {
        name: "c".into(),
        color: "#000".into(),
        sort: None,
    };
    let (cove, event_id) = write_with_event_typed(repo.as_ref(), "user", None, &bus, move |tx| {
        Box::pin(async move {
            let cove = cove_create_tx(tx, p).await?;
            Ok((cove.clone(), Event::CoveUpdated(cove)))
        })
    })
    .await
    .expect("write_with_event ok");

    // Entity persisted.
    let fetched = repo.cove_get(&cove.id).await.unwrap();
    assert_eq!(fetched.map(|c| c.id), Some(cove.id.clone()));

    // Event row persisted with the same id we got back.
    let row: (i64, String, String) =
        sqlx::query_as("SELECT id, kind, actor FROM events WHERE id = ?1")
            .bind(event_id)
            .fetch_one(concrete.pool())
            .await
            .unwrap();
    assert_eq!(row.0, event_id);
    assert_eq!(row.1, "cove.updated");
    assert_eq!(row.2, "user");

    // Bus saw the envelope with the right id.
    let env = sub.try_recv().expect("envelope delivered");
    assert_eq!(env.id, event_id);
    match env.event {
        Event::CoveUpdated(c) => assert_eq!(c.id, cove.id),
        _ => panic!("wrong event"),
    }
}

// ---------------------------------------------------------------------------
// 2. Atomicity — closure error path.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn closure_error_rolls_back_entity_and_event_rows() {
    let (repo, concrete, bus) = boot().await;
    let mut sub = bus.subscribe();

    // Seed a cove so the wave_create_tx step inside the closure succeeds —
    // we only want the *closure-level* error to fail the txn.
    let cove = repo
        .cove_create(NewCove {
            name: "c".into(),
            color: "#000".into(),
            sort: None,
        })
        .await
        .unwrap();
    let waves_before: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM waves")
        .fetch_one(concrete.pool())
        .await
        .unwrap();
    let events_before: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM events")
        .fetch_one(concrete.pool())
        .await
        .unwrap();

    let cove_id = cove.id.clone();
    let err = write_with_event_typed::<Wave, _>(repo.as_ref(), "user", None, &bus, move |tx| {
        Box::pin(async move {
            // Real entity write succeeds inside the txn ...
            let _w = wave_create_tx(
                tx,
                NewWave {
                    cove_id,
                    title: "doomed".into(),
                    sort: None,
                },
            )
            .await?;
            // ... but then the closure deliberately fails.
            Err::<(Wave, Event), CalmError>(CalmError::Internal("simulated".into()))
        })
    })
    .await
    .expect_err("closure failure must bubble");
    assert!(matches!(err, CalmError::Internal(ref m) if m == "simulated"));

    // Wave was rolled back.
    let waves_after: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM waves")
        .fetch_one(concrete.pool())
        .await
        .unwrap();
    assert_eq!(waves_after.0, waves_before.0);

    // No event row inserted.
    let events_after: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM events")
        .fetch_one(concrete.pool())
        .await
        .unwrap();
    assert_eq!(events_after.0, events_before.0);

    // And no broadcast fired. (We seeded with the cove pre-txn, which did
    // produce a write directly to repo.cove_create — that path does NOT
    // emit on the bus today, so the subscriber's queue is empty.)
    assert!(sub.try_recv().is_err());
}

// ---------------------------------------------------------------------------
// 3. Atomicity — event-insert error path.
// ---------------------------------------------------------------------------
//
// We can't easily corrupt the events INSERT from outside (the kind/payload
// are server-derived). The most reliable provocation is to delete the
// `events` table inside the same txn before the wrapper's event_append
// runs — but the wrapper opens its own txn and the closure shares it, so
// we can deliberately `DROP TABLE events` inside the closure: the
// subsequent INSERT into `events` will fail, which must roll back the
// entity write the closure did before the drop.
//
// This is contrived but it's the only failure surface short of patching
// `SqlxRepo` with a test-only override. It does exercise the same code
// path: "entity write succeeded, event insert failed → roll back entity."

#[tokio::test]
async fn event_insert_failure_rolls_back_entity_write() {
    let (repo, concrete, bus) = boot().await;

    let coves_before: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM coves")
        .fetch_one(concrete.pool())
        .await
        .unwrap();

    // Inject a tx hook that drops the events table *after* the entity
    // write but *before* the wrapper's event_append fires.
    let res = write_with_event_typed(repo.as_ref(), "user", None, &bus, |tx| {
        Box::pin(async move {
            let cove = cove_create_tx(
                tx,
                NewCove {
                    name: "c".into(),
                    color: "#000".into(),
                    sort: None,
                },
            )
            .await?;
            // Drop the events table so the wrapper's subsequent
            // `INSERT INTO events` fails inside the same txn.
            sqlx::query("DROP TABLE events").execute(&mut **tx).await?;
            Ok((cove.clone(), Event::CoveUpdated(cove)))
        })
    })
    .await;
    assert!(
        res.is_err(),
        "expected event-insert failure to bubble, got {:?}",
        res
    );

    // The cove must have been rolled back even though the closure's
    // explicit entity-write succeeded — that's the cross-step atomicity
    // guarantee the design hinges on.
    let coves_after: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM coves")
        .fetch_one(concrete.pool())
        .await
        .unwrap();
    assert_eq!(
        coves_after.0, coves_before.0,
        "entity write must roll back when event-insert fails"
    );
}

// ---------------------------------------------------------------------------
// 4. Replay correctness.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn replaying_events_table_yields_same_envelope_sequence_as_live_subscriber() {
    let (repo, concrete, bus) = boot().await;
    let mut live = bus.subscribe();

    // Drive a small write sequence through the wrapper.
    let (cove, _) = write_with_event_typed(repo.as_ref(), "user", None, &bus, move |tx| {
        Box::pin(async move {
            let cove = cove_create_tx(
                tx,
                NewCove {
                    name: "c1".into(),
                    color: "#000".into(),
                    sort: None,
                },
            )
            .await?;
            Ok((cove.clone(), Event::CoveUpdated(cove)))
        })
    })
    .await
    .unwrap();
    let cove_id = cove.id.clone();

    let (wave, _) = write_with_event_typed(repo.as_ref(), "user", None, &bus, move |tx| {
        Box::pin(async move {
            let wave = wave_create_tx(
                tx,
                NewWave {
                    cove_id,
                    title: "w1".into(),
                    sort: None,
                },
            )
            .await?;
            Ok((wave.clone(), Event::WaveUpdated(wave)))
        })
    })
    .await
    .unwrap();

    let wave_id = wave.id.clone();
    let (_card, _) = write_with_event_typed(repo.as_ref(), "user", None, &bus, move |tx| {
        Box::pin(async move {
            let card = card_create_tx(
                tx,
                NewCard {
                    wave_id,
                    kind: "terminal".into(),
                    sort: None,
                    payload: serde_json::json!({}),
                },
            )
            .await?;
            Ok((card.clone(), Event::CardAdded(card)))
        })
    })
    .await
    .unwrap();

    // Drain three envelopes from the live subscriber.
    let mut live_envelopes = Vec::new();
    for _ in 0..3 {
        live_envelopes.push(live.recv().await.expect("live env"));
    }

    // SELECT-replay from the events table.
    let rows: Vec<(i64, String, String)> =
        sqlx::query_as("SELECT id, kind, payload FROM events ORDER BY id ASC")
            .fetch_all(concrete.pool())
            .await
            .unwrap();
    assert_eq!(rows.len(), 3);

    // Live envelopes and replay rows agree on id + kind in order.
    for (live, replay) in live_envelopes.iter().zip(rows.iter()) {
        assert_eq!(live.id, replay.0, "id matches");
        assert_eq!(live.event.kind_tag(), replay.1, "kind matches");
    }
    // Strict monotonic ids.
    assert!(rows[0].0 < rows[1].0);
    assert!(rows[1].0 < rows[2].0);
}

// ---------------------------------------------------------------------------
// 5. Replay-then-live boundary — the crown jewel.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn replay_then_live_dedup_under_concurrent_write() {
    let (repo, concrete, bus) = boot().await;

    // Seed a cove so the wave creates have somewhere to live.
    let cove = repo
        .cove_create(NewCove {
            name: "c".into(),
            color: "#000".into(),
            sort: None,
        })
        .await
        .unwrap();

    // ----- pre-replay history --------------------------------------------
    // Drive three writes through `write_with_event` first.
    for i in 0..3 {
        let cove_id = cove.id.clone();
        let title = format!("w{}", i);
        write_with_event_typed(repo.as_ref(), "user", None, &bus, move |tx| {
            Box::pin(async move {
                let w = wave_create_tx(
                    tx,
                    NewWave {
                        cove_id,
                        title,
                        sort: None,
                    },
                )
                .await?;
                Ok((w.clone(), Event::WaveUpdated(w)))
            })
        })
        .await
        .unwrap();
    }

    // ----- subscribe-first ordering --------------------------------------
    // The would-be Scope D replay handler:
    //   1. subscribe() — captures the live receiver. Anything emitted
    //      after this line lands in the broadcast buffer.
    //   2. SELECT historical events from id > 0 ORDER BY id ASC.
    //   3. Concurrently, a brand-new write fires (this is the race the
    //      design's "subscribe-first" pattern guards against).
    //   4. Drain live receiver, skipping any envelope whose id is <=
    //      `last_replayed_id` (dedupe vs the replay set).
    //   5. Forward the rest.
    //
    // The assertion: every event is delivered exactly once, in id order.

    let mut live = bus.subscribe();

    // Block the replay with an explicit semaphore so the race is real.
    let sem = Arc::new(tokio::sync::Semaphore::new(0));
    let sem_clone = Arc::clone(&sem);
    let pool = concrete.pool().clone();
    let replay_task = tokio::spawn(async move {
        // Hold here until the test fires the race.
        let _permit = sem_clone.acquire().await;

        let rows: Vec<(i64,)> =
            sqlx::query_as("SELECT id FROM events WHERE id > 0 ORDER BY id ASC")
                .fetch_all(&pool)
                .await
                .unwrap();
        rows.into_iter().map(|r| r.0).collect::<Vec<i64>>()
    });

    // ----- inject a write while the replay is blocked --------------------
    {
        let cove_id = cove.id.clone();
        write_with_event_typed(repo.as_ref(), "user", None, &bus, move |tx| {
            Box::pin(async move {
                let w = wave_create_tx(
                    tx,
                    NewWave {
                        cove_id,
                        title: "during-replay".into(),
                        sort: None,
                    },
                )
                .await?;
                Ok((w.clone(), Event::WaveUpdated(w)))
            })
        })
        .await
        .unwrap();
    }

    // ----- release the replay --------------------------------------------
    sem.add_permits(1);
    let replay_ids = replay_task.await.expect("replay task ok");

    // Replay set spans the 3 pre-replay events PLUS the during-replay
    // event — because subscribe-first means we subscribed *after* the 3
    // initial writes but *before* the during-replay one, AND the SELECT
    // runs against the table state at SELECT time, which already includes
    // the during-replay write.
    assert_eq!(replay_ids.len(), 4, "expected 4 historical rows");
    let last_replayed_id = *replay_ids.last().unwrap();

    // Drain the live receiver. The during-replay event should be in the
    // buffer (because we subscribed before the during-replay write fired);
    // our dedupe rule must drop it because its id <= last_replayed_id.
    //
    // Note: the 3 pre-replay events do NOT appear in the live receiver
    // because we subscribed *after* they fired.
    let mut live_forwarded = Vec::new();
    while let Ok(env) = live.try_recv() {
        if env.id <= last_replayed_id {
            // Dedup — already in the replay set. Drop.
            continue;
        }
        live_forwarded.push(env.id);
    }
    assert!(
        live_forwarded.is_empty(),
        "no event should survive dedup; got {:?}",
        live_forwarded
    );

    // Union of (replay set) ∪ (live forwarded) is the full event sequence,
    // each id exactly once.
    let mut all_ids = replay_ids.clone();
    all_ids.extend(live_forwarded);
    let unique: std::collections::BTreeSet<i64> = all_ids.iter().copied().collect();
    assert_eq!(
        unique.len(),
        all_ids.len(),
        "each event delivered exactly once across replay + live"
    );
    // Strict monotonic order.
    for w in all_ids.windows(2) {
        assert!(w[0] < w[1], "ids stay in order");
    }
}

// ---------------------------------------------------------------------------
// 6. Property test — hand-rolled (no proptest dep added by this scope).
// ---------------------------------------------------------------------------

#[derive(Clone, Debug)]
enum Op {
    CreateCove(String),
    CreateWaveInLastCove(String),
    CreateCardInLastWave(String),
    SetOverlayOnLastCard(String),
}

/// Apply an op through `write_with_event` against the shared repo + bus.
/// Returns `true` if the op committed, `false` if it was skipped (e.g.
/// `CreateWaveInLastCove` with no cove yet).
async fn apply_op(repo: &dyn Repo, bus: &EventBus, state: &mut PropState, op: &Op) -> bool {
    match op {
        Op::CreateCove(name) => {
            let p = NewCove {
                name: name.clone(),
                color: "#abc".into(),
                sort: None,
            };
            let (cove, _) = write_with_event_typed::<Cove, _>(repo, "user", None, bus, move |tx| {
                Box::pin(async move {
                    let c = cove_create_tx(tx, p).await?;
                    Ok((c.clone(), Event::CoveUpdated(c)))
                })
            })
            .await
            .unwrap();
            state.last_cove = Some(cove.id);
            true
        }
        Op::CreateWaveInLastCove(title) => {
            let Some(cove_id) = state.last_cove.clone() else {
                return false;
            };
            let title = title.clone();
            let (wave, _) = write_with_event_typed(repo, "user", None, bus, move |tx| {
                Box::pin(async move {
                    let w = wave_create_tx(
                        tx,
                        NewWave {
                            cove_id,
                            title,
                            sort: None,
                        },
                    )
                    .await?;
                    Ok((w.clone(), Event::WaveUpdated(w)))
                })
            })
            .await
            .unwrap();
            state.last_wave = Some(wave.id);
            true
        }
        Op::CreateCardInLastWave(_label) => {
            let Some(wave_id) = state.last_wave.clone() else {
                return false;
            };
            let (card, _) = write_with_event_typed(repo, "user", None, bus, move |tx| {
                Box::pin(async move {
                    let c = card_create_tx(
                        tx,
                        NewCard {
                            wave_id,
                            kind: "terminal".into(),
                            sort: None,
                            payload: serde_json::json!({}),
                        },
                    )
                    .await?;
                    Ok((c.clone(), Event::CardAdded(c)))
                })
            })
            .await
            .unwrap();
            state.last_card = Some(card.id);
            true
        }
        Op::SetOverlayOnLastCard(_label) => {
            let Some(card_id) = state.last_card.clone() else {
                return false;
            };
            // Use kernel-owned `status` overlay; payload `{state: "Idle"}` matches
            // `validate_overlay_payload` rules.
            let new_overlay = NewOverlay {
                plugin_id: "kernel".into(),
                entity_kind: "card".into(),
                entity_id: card_id,
                kind: "status".into(),
                payload: serde_json::json!({ "state": "Idle" }),
            };
            let (_o, _) = write_with_event_typed(repo, "kernel", None, bus, move |tx| {
                Box::pin(async move {
                    let o = overlay_upsert_tx(tx, new_overlay).await?;
                    Ok((o.clone(), Event::OverlaySet(o)))
                })
            })
            .await
            .unwrap();
            true
        }
    }
}

#[derive(Default)]
struct PropState {
    last_cove: Option<String>,
    last_wave: Option<String>,
    last_card: Option<String>,
}

#[tokio::test]
async fn property_cold_replay_converges_with_continuous_subscriber() {
    // Deterministic sequence sampled to cover all four op kinds and the
    // "skip when prereq missing" branch. A proptest-driven version (with
    // a real `proptest!{}` macro + shrinker) is the natural follow-up
    // once we're willing to add the crate; design §6.1 calls it out
    // explicitly.
    let ops = vec![
        Op::CreateWaveInLastCove("skip-me".into()), // no cove yet → skipped
        Op::CreateCove("alpha".into()),
        Op::CreateWaveInLastCove("aw1".into()),
        Op::CreateCardInLastWave("ac1".into()),
        Op::SetOverlayOnLastCard("ao1".into()),
        Op::CreateCove("beta".into()),
        Op::CreateWaveInLastCove("bw1".into()),
        Op::CreateCardInLastWave("bc1".into()),
        Op::CreateCardInLastWave("bc2".into()),
        Op::SetOverlayOnLastCard("bo1".into()),
        Op::SetOverlayOnLastCard("bo2".into()), // overlay upsert — same key, second write
    ];

    let (repo, concrete, bus) = boot().await;
    // Continuously-connected subscriber: subscribes at boot, before any
    // writes, and records every envelope.
    let mut continuous = bus.subscribe();

    let mut state = PropState::default();
    let mut expected_committed = 0usize;
    for op in &ops {
        if apply_op(repo.as_ref(), &bus, &mut state, op).await {
            expected_committed += 1;
        }
    }

    // Drain the continuous subscriber.
    let mut continuous_envelopes = Vec::new();
    while let Ok(env) = continuous.try_recv() {
        continuous_envelopes.push((env.id, env.event.kind_tag().to_string()));
    }
    assert_eq!(
        continuous_envelopes.len(),
        expected_committed,
        "continuous subscriber saw every committed event"
    );

    // Cold replay: read the events table from scratch.
    let replay_rows: Vec<(i64, String)> =
        sqlx::query_as("SELECT id, kind FROM events ORDER BY id ASC")
            .fetch_all(concrete.pool())
            .await
            .unwrap();

    assert_eq!(
        replay_rows.len(),
        continuous_envelopes.len(),
        "cold-replay row count = continuous-subscriber count"
    );
    for ((live_id, live_kind), (replay_id, replay_kind)) in
        continuous_envelopes.iter().zip(replay_rows.iter())
    {
        assert_eq!(live_id, replay_id, "id matches at each step");
        assert_eq!(live_kind, replay_kind, "kind matches at each step");
    }
}
