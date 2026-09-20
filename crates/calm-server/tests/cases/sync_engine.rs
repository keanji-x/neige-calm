//! Sync engine server-side tests: `write_with_event` atomicity, replay correctness,
//! the replay-then-live boundary, and event version / scope round-trips.

use std::sync::Arc;

use calm_server::db::prelude::*;
use calm_server::db::sqlite::{
    SqlxRepo, area_create_tx, card_create_tx, overlay_upsert_tx, track_create_tx,
};
use calm_server::db::write_with_event_typed;
use calm_server::error::CalmError;
use calm_server::event::{Event, EventBus, EventScope, SYNC_EVENT_VERSION};
use calm_server::ids::ActorId;
use calm_server::model::{NewArea, NewCard, NewOverlay, NewTrack, Track};

/// Boot an in-memory `SqlxRepo` and a fresh `EventBus`.
async fn boot() -> (Arc<dyn Repo>, Arc<SqlxRepo>, EventBus) {
    let concrete = Arc::new(
        SqlxRepo::open("sqlite::memory:")
            .await
            .expect("open in-memory repo"),
    );
    let r: Arc<dyn Repo> = concrete.clone();
    (r, concrete, EventBus::new())
}

#[tokio::test]
async fn write_with_event_persists_entity_and_event_in_one_txn() {
    let (repo, concrete, bus) = boot().await;
    let mut sub = bus.subscribe();

    let p = NewArea {
        name: "c".into(),
        color: "#000".into(),
        sort: None,
    };
    let (area, event_id) = write_with_event_typed(
        repo.as_ref(),
        ActorId::User,
        EventScope::System,
        None,
        &bus,
        &calm_server::state::WriteContext::new(
            calm_server::card_role_cache::CardRoleCache::new(),
            calm_server::track_area_cache::TrackAreaCache::new(),
        ),
        move |tx| {
            Box::pin(async move {
                let area = area_create_tx(tx, p).await?;
                Ok((area.clone(), Event::AreaUpdated(area)))
            })
        },
    )
    .await
    .expect("write_with_event ok");

    let fetched = repo.area_get(area.id.as_str()).await.unwrap();
    assert_eq!(fetched.map(|c| c.id), Some(area.id.clone()));

    let row: (i64, String, String) =
        sqlx::query_as("SELECT id, kind, actor FROM events WHERE id = ?1")
            .bind(event_id)
            .fetch_one(concrete.pool())
            .await
            .unwrap();
    assert_eq!(row.0, event_id);
    assert_eq!(row.1, "area.updated");
    let actor_json: serde_json::Value = serde_json::from_str(&row.2).unwrap();
    assert_eq!(actor_json, serde_json::json!({"kind": "User"}));

    let env = sub.try_recv().expect("envelope delivered");
    assert_eq!(env.id, event_id);
    match env.event {
        Event::AreaUpdated(c) => assert_eq!(c.id, area.id),
        _ => panic!("wrong event"),
    }
}

#[tokio::test]
async fn closure_error_rolls_back_entity_and_event_rows() {
    let (repo, concrete, bus) = boot().await;
    let mut sub = bus.subscribe();

    // Seed an area so the track_create_tx step inside the closure succeeds; only the closure-level error should fail the txn.
    let area = repo
        .area_create(NewArea {
            name: "c".into(),
            color: "#000".into(),
            sort: None,
        })
        .await
        .unwrap();
    let tracks_before: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM tracks")
        .fetch_one(concrete.pool())
        .await
        .unwrap();
    let events_before: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM events")
        .fetch_one(concrete.pool())
        .await
        .unwrap();

    let area_id = area.id.clone();
    let err = write_with_event_typed(
        repo.as_ref(),
        ActorId::User,
        EventScope::System,
        None,
        &bus,
        &calm_server::state::WriteContext::new(
            calm_server::card_role_cache::CardRoleCache::new(),
            calm_server::track_area_cache::TrackAreaCache::new(),
        ),
        move |tx| {
            Box::pin(async move {
                let _w = track_create_tx(
                    tx,
                    NewTrack {
                        template_input: None,
                        area_id,
                        title: "doomed".into(),
                        sort: None,
                        cwd: String::new(),
                        template_id: None,
                        plugin_scope: None,
                        attach_folder: false,
                        theme: calm_server::routes::theme::RequestTheme::default_dark(),
                    },
                    None,
                    &calm_server::db::sqlite::TrackWorkspacePlan::AttachedFromCwd,
                    None,
                    &calm_server::track_area_cache::TrackAreaCache::new(),
                )
                .await?;
                Err::<(Track, Event), CalmError>(CalmError::Internal("simulated".into()))
            })
        },
    )
    .await
    .expect_err("closure failure must bubble");
    assert!(matches!(err, CalmError::Internal(ref m) if m == "simulated"));

    let tracks_after: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM tracks")
        .fetch_one(concrete.pool())
        .await
        .unwrap();
    assert_eq!(tracks_after.0, tracks_before.0);

    let events_after: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM events")
        .fetch_one(concrete.pool())
        .await
        .unwrap();
    assert_eq!(events_after.0, events_before.0);

    // No broadcast fired: `repo.area_create` does not emit on the bus, so the subscriber's queue is empty.
    assert!(sub.try_recv().is_err());
}

// `DROP TABLE events` inside the closure makes the wrapper's subsequent INSERT fail in the same txn,
// which must roll back the entity write the closure did before the drop.

#[tokio::test]
async fn event_insert_failure_rolls_back_entity_write() {
    let (repo, concrete, bus) = boot().await;

    let areas_before: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM areas")
        .fetch_one(concrete.pool())
        .await
        .unwrap();

    let res = write_with_event_typed(
        repo.as_ref(),
        ActorId::User,
        EventScope::System,
        None,
        &bus,
        &calm_server::state::WriteContext::new(
            calm_server::card_role_cache::CardRoleCache::new(),
            calm_server::track_area_cache::TrackAreaCache::new(),
        ),
        |tx| {
            Box::pin(async move {
                let area = area_create_tx(
                    tx,
                    NewArea {
                        name: "c".into(),
                        color: "#000".into(),
                        sort: None,
                    },
                )
                .await?;
                sqlx::query("DROP TABLE events").execute(&mut **tx).await?;
                Ok((area.clone(), Event::AreaUpdated(area)))
            })
        },
    )
    .await;
    assert!(
        res.is_err(),
        "expected event-insert failure to bubble, got {:?}",
        res
    );

    let areas_after: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM areas")
        .fetch_one(concrete.pool())
        .await
        .unwrap();
    assert_eq!(
        areas_after.0, areas_before.0,
        "entity write must roll back when event-insert fails"
    );
}

#[tokio::test]
async fn replaying_events_table_yields_same_envelope_sequence_as_live_subscriber() {
    let (repo, concrete, bus) = boot().await;
    let mut live = bus.subscribe();

    let (area, _) = write_with_event_typed(
        repo.as_ref(),
        ActorId::User,
        EventScope::System,
        None,
        &bus,
        &calm_server::state::WriteContext::new(
            calm_server::card_role_cache::CardRoleCache::new(),
            calm_server::track_area_cache::TrackAreaCache::new(),
        ),
        move |tx| {
            Box::pin(async move {
                let area = area_create_tx(
                    tx,
                    NewArea {
                        name: "c1".into(),
                        color: "#000".into(),
                        sort: None,
                    },
                )
                .await?;
                Ok((area.clone(), Event::AreaUpdated(area)))
            })
        },
    )
    .await
    .unwrap();
    let area_id = area.id.clone();

    let (track, _) = write_with_event_typed(
        repo.as_ref(),
        ActorId::User,
        EventScope::System,
        None,
        &bus,
        &calm_server::state::WriteContext::new(
            calm_server::card_role_cache::CardRoleCache::new(),
            calm_server::track_area_cache::TrackAreaCache::new(),
        ),
        move |tx| {
            Box::pin(async move {
                let track = track_create_tx(
                    tx,
                    NewTrack {
                        template_input: None,
                        area_id,
                        title: "w1".into(),
                        sort: None,
                        cwd: String::new(),
                        template_id: None,
                        plugin_scope: None,
                        attach_folder: false,
                        theme: calm_server::routes::theme::RequestTheme::default_dark(),
                    },
                    None,
                    &calm_server::db::sqlite::TrackWorkspacePlan::AttachedFromCwd,
                    None,
                    &calm_server::track_area_cache::TrackAreaCache::new(),
                )
                .await?;
                Ok((
                    track.clone(),
                    Event::TrackUpdated(calm_server::event::TrackUpdatedPayload::new(track, None)),
                ))
            })
        },
    )
    .await
    .unwrap();

    let track_id = track.id.clone();
    let (_card, _) = write_with_event_typed(
        repo.as_ref(),
        ActorId::User,
        EventScope::System,
        None,
        &bus,
        &calm_server::state::WriteContext::new(
            calm_server::card_role_cache::CardRoleCache::new(),
            calm_server::track_area_cache::TrackAreaCache::new(),
        ),
        move |tx| {
            Box::pin(async move {
                let card = card_create_tx(
                    tx,
                    NewCard {
                        track_id,
                        title: None,
                        kind: "terminal".into(),
                        sort: None,
                        payload: serde_json::json!({}),
                    },
                    &calm_server::card_role_cache::CardRoleCache::new(),
                )
                .await?;
                Ok((card.clone(), Event::CardAdded(card)))
            })
        },
    )
    .await
    .unwrap();

    let mut live_envelopes = Vec::new();
    for _ in 0..3 {
        live_envelopes.push(live.recv().await.expect("live env"));
    }

    let rows: Vec<(i64, String, String)> =
        sqlx::query_as("SELECT id, kind, payload FROM events ORDER BY id ASC")
            .fetch_all(concrete.pool())
            .await
            .unwrap();
    assert_eq!(rows.len(), 3);

    for (live, replay) in live_envelopes.iter().zip(rows.iter()) {
        assert_eq!(live.id, replay.0, "id matches");
        assert_eq!(live.event.kind_tag(), replay.1, "kind matches");
    }
    assert!(rows[0].0 < rows[1].0);
    assert!(rows[1].0 < rows[2].0);
}

#[tokio::test]
async fn replay_then_live_dedup_under_concurrent_write() {
    let (repo, concrete, bus) = boot().await;

    let area = repo
        .area_create(NewArea {
            name: "c".into(),
            color: "#000".into(),
            sort: None,
        })
        .await
        .unwrap();

    for i in 0..3 {
        let area_id = area.id.clone();
        let title = format!("w{}", i);
        write_with_event_typed(
            repo.as_ref(),
            ActorId::User,
            EventScope::System,
            None,
            &bus,
            &calm_server::state::WriteContext::new(
                calm_server::card_role_cache::CardRoleCache::new(),
                calm_server::track_area_cache::TrackAreaCache::new(),
            ),
            move |tx| {
                Box::pin(async move {
                    let w = track_create_tx(
                        tx,
                        NewTrack {
                            template_input: None,
                            area_id,
                            title,
                            sort: None,
                            cwd: String::new(),
                            template_id: None,
                            plugin_scope: None,
                            attach_folder: false,
                            theme: calm_server::routes::theme::RequestTheme::default_dark(),
                        },
                        None,
                        &calm_server::db::sqlite::TrackWorkspacePlan::AttachedFromCwd,
                        None,
                        &calm_server::track_area_cache::TrackAreaCache::new(),
                    )
                    .await?;
                    Ok((
                        w.clone(),
                        Event::TrackUpdated(calm_server::event::TrackUpdatedPayload::new(w, None)),
                    ))
                })
            },
        )
        .await
        .unwrap();
    }

    // Subscribe-first ordering: subscribe, SELECT history, race a new write, then drain the live receiver
    // skipping any envelope whose id is <= `last_replayed_id`.

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

    {
        let area_id = area.id.clone();
        write_with_event_typed(
            repo.as_ref(),
            ActorId::User,
            EventScope::System,
            None,
            &bus,
            &calm_server::state::WriteContext::new(
                calm_server::card_role_cache::CardRoleCache::new(),
                calm_server::track_area_cache::TrackAreaCache::new(),
            ),
            move |tx| {
                Box::pin(async move {
                    let w = track_create_tx(
                        tx,
                        NewTrack {
                            template_input: None,
                            area_id,
                            title: "during-replay".into(),
                            sort: None,
                            cwd: String::new(),
                            template_id: None,
                            plugin_scope: None,
                            attach_folder: false,
                            theme: calm_server::routes::theme::RequestTheme::default_dark(),
                        },
                        None,
                        &calm_server::db::sqlite::TrackWorkspacePlan::AttachedFromCwd,
                        None,
                        &calm_server::track_area_cache::TrackAreaCache::new(),
                    )
                    .await?;
                    Ok((
                        w.clone(),
                        Event::TrackUpdated(calm_server::event::TrackUpdatedPayload::new(w, None)),
                    ))
                })
            },
        )
        .await
        .unwrap();
    }

    sem.add_permits(1);
    let replay_ids = replay_task.await.expect("replay task ok");

    // The replay set spans the during-replay event too: the SELECT runs against the table state at SELECT time.
    assert_eq!(replay_ids.len(), 4, "expected 4 historical rows");
    let last_replayed_id = *replay_ids.last().unwrap();

    // The during-replay event is in the live buffer (subscribed before it fired); the dedupe rule must drop it.
    let mut live_forwarded = Vec::new();
    while let Ok(env) = live.try_recv() {
        if env.id <= last_replayed_id {
            continue;
        }
        live_forwarded.push(env.id);
    }
    assert!(
        live_forwarded.is_empty(),
        "no event should survive dedup; got {:?}",
        live_forwarded
    );

    let mut all_ids = replay_ids.clone();
    all_ids.extend(live_forwarded);
    let unique: std::collections::BTreeSet<i64> = all_ids.iter().copied().collect();
    assert_eq!(
        unique.len(),
        all_ids.len(),
        "each event delivered exactly once across replay + live"
    );
    for w in all_ids.windows(2) {
        assert!(w[0] < w[1], "ids stay in order");
    }
}

#[derive(Clone, Debug)]
enum Op {
    CreateArea(String),
    CreateTrackInLastArea(String),
    CreateCardInLastTrack(String),
    SetOverlayOnLastCard(String),
}

async fn apply_op(repo: &dyn Repo, bus: &EventBus, state: &mut PropState, op: &Op) -> bool {
    match op {
        Op::CreateArea(name) => {
            let p = NewArea {
                name: name.clone(),
                color: "#abc".into(),
                sort: None,
            };
            let (area, _) = write_with_event_typed(
                repo,
                ActorId::User,
                EventScope::System,
                None,
                bus,
                &calm_server::state::WriteContext::new(
                    calm_server::card_role_cache::CardRoleCache::new(),
                    calm_server::track_area_cache::TrackAreaCache::new(),
                ),
                move |tx| {
                    Box::pin(async move {
                        let c = area_create_tx(tx, p).await?;
                        Ok((c.clone(), Event::AreaUpdated(c)))
                    })
                },
            )
            .await
            .unwrap();
            state.last_area = Some(area.id);
            true
        }
        Op::CreateTrackInLastArea(title) => {
            let Some(area_id) = state.last_area.clone() else {
                return false;
            };
            let title = title.clone();
            let (track, _) = write_with_event_typed(
                repo,
                ActorId::User,
                EventScope::System,
                None,
                bus,
                &calm_server::state::WriteContext::new(
                    calm_server::card_role_cache::CardRoleCache::new(),
                    calm_server::track_area_cache::TrackAreaCache::new(),
                ),
                move |tx| {
                    Box::pin(async move {
                        let w = track_create_tx(
                            tx,
                            NewTrack {
                                template_input: None,
                                area_id,
                                title,
                                sort: None,
                                cwd: String::new(),
                                template_id: None,
                                plugin_scope: None,
                                attach_folder: false,
                                theme: calm_server::routes::theme::RequestTheme::default_dark(),
                            },
                            None,
                            &calm_server::db::sqlite::TrackWorkspacePlan::AttachedFromCwd,
                            None,
                            &calm_server::track_area_cache::TrackAreaCache::new(),
                        )
                        .await?;
                        Ok((
                            w.clone(),
                            Event::TrackUpdated(calm_server::event::TrackUpdatedPayload::new(
                                w, None,
                            )),
                        ))
                    })
                },
            )
            .await
            .unwrap();
            state.last_track = Some(track.id);
            true
        }
        Op::CreateCardInLastTrack(_label) => {
            let Some(track_id) = state.last_track.clone() else {
                return false;
            };
            let (card, _) = write_with_event_typed(
                repo,
                ActorId::User,
                EventScope::System,
                None,
                bus,
                &calm_server::state::WriteContext::new(
                    calm_server::card_role_cache::CardRoleCache::new(),
                    calm_server::track_area_cache::TrackAreaCache::new(),
                ),
                move |tx| {
                    Box::pin(async move {
                        let c = card_create_tx(
                            tx,
                            NewCard {
                                track_id,
                                title: None,
                                kind: "terminal".into(),
                                sort: None,
                                payload: serde_json::json!({}),
                            },
                            &calm_server::card_role_cache::CardRoleCache::new(),
                        )
                        .await?;
                        Ok((c.clone(), Event::CardAdded(c)))
                    })
                },
            )
            .await
            .unwrap();
            state.last_card = Some(card.id);
            true
        }
        Op::SetOverlayOnLastCard(_label) => {
            let Some(card_id) = state.last_card.clone() else {
                return false;
            };
            // Kernel-owned `status` overlay; payload `{state: "Idle"}` matches `validate_overlay_payload` rules.
            let new_overlay = NewOverlay {
                plugin_id: "kernel".into(),
                entity_kind: "card".into(),
                entity_id: card_id.to_string(),
                kind: "status".into(),
                payload: serde_json::json!({ "state": "Idle" }),
            };
            let (_o, _) = write_with_event_typed(
                repo,
                ActorId::Kernel,
                EventScope::System,
                None,
                bus,
                &calm_server::state::WriteContext::new(
                    calm_server::card_role_cache::CardRoleCache::new(),
                    calm_server::track_area_cache::TrackAreaCache::new(),
                ),
                move |tx| {
                    Box::pin(async move {
                        let o = overlay_upsert_tx(tx, new_overlay).await?;
                        Ok((o.clone(), Event::OverlaySet(o)))
                    })
                },
            )
            .await
            .unwrap();
            true
        }
    }
}

#[derive(Default)]
struct PropState {
    last_area: Option<calm_server::ids::AreaId>,
    last_track: Option<calm_server::ids::TrackId>,
    last_card: Option<calm_server::ids::CardId>,
}

#[tokio::test]
async fn property_cold_replay_converges_with_continuous_subscriber() {
    let ops = vec![
        Op::CreateTrackInLastArea("skip-me".into()), // no area yet → skipped
        Op::CreateArea("alpha".into()),
        Op::CreateTrackInLastArea("aw1".into()),
        Op::CreateCardInLastTrack("ac1".into()),
        Op::SetOverlayOnLastCard("ao1".into()),
        Op::CreateArea("beta".into()),
        Op::CreateTrackInLastArea("bw1".into()),
        Op::CreateCardInLastTrack("bc1".into()),
        Op::CreateCardInLastTrack("bc2".into()),
        Op::SetOverlayOnLastCard("bo1".into()),
        Op::SetOverlayOnLastCard("bo2".into()), // overlay upsert — same key, second write
    ];

    let (repo, concrete, bus) = boot().await;
    let mut continuous = bus.subscribe();

    let mut state = PropState::default();
    let mut expected_committed = 0usize;
    for op in &ops {
        if apply_op(repo.as_ref(), &bus, &mut state, op).await {
            expected_committed += 1;
        }
    }

    let mut continuous_envelopes = Vec::new();
    while let Ok(env) = continuous.try_recv() {
        continuous_envelopes.push((env.id, env.event.kind_tag().to_string()));
    }
    assert_eq!(
        continuous_envelopes.len(),
        expected_committed,
        "continuous subscriber saw every committed event"
    );

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

#[tokio::test]
async fn event_version_round_trips_from_write_to_replay() {
    let (repo, concrete, bus) = boot().await;

    let (_area, event_id) = write_with_event_typed(
        repo.as_ref(),
        ActorId::User,
        EventScope::System,
        None,
        &bus,
        &calm_server::state::WriteContext::new(
            calm_server::card_role_cache::CardRoleCache::new(),
            calm_server::track_area_cache::TrackAreaCache::new(),
        ),
        move |tx| {
            Box::pin(async move {
                let area = area_create_tx(
                    tx,
                    NewArea {
                        name: "version-rt".into(),
                        color: "#000".into(),
                        sort: None,
                    },
                )
                .await?;
                Ok((area.clone(), Event::AreaUpdated(area)))
            })
        },
    )
    .await
    .expect("write_with_event ok");

    // Read the raw column directly so the test fails clearly if the INSERT forgot to bind it.
    let row: (u32,) = sqlx::query_as("SELECT event_version FROM events WHERE id = ?1")
        .bind(event_id)
        .fetch_one(concrete.pool())
        .await
        .unwrap();
    assert_eq!(
        row.0, SYNC_EVENT_VERSION,
        "row's event_version column must match the kernel's constant"
    );

    let log = repo.events_since(0, i64::MAX).await.unwrap();
    let (replayed_id, replayed_version, _scope, _ev) = log
        .into_iter()
        .find(|(id, _, _, _)| *id == event_id)
        .expect("replayed event present");
    assert_eq!(replayed_id, event_id);
    assert_eq!(
        replayed_version, SYNC_EVENT_VERSION,
        "events_since must propagate the row's event_version"
    );
}

// Rows that leave `event_version` to the column default come back as `1` from the replay path.

#[tokio::test]
async fn replay_treats_unstamped_row_as_version_one() {
    let (repo, concrete, _bus) = boot().await;

    // Insert a row that does not bind `event_version`, relying on the column default.
    sqlx::query(
        r##"INSERT INTO events (kind, payload, actor, at, correlation)
           VALUES ('area.updated', '{"id":"c","name":"n","color":"#000","sort":0,"created_at":0,"updated_at":0}', 'user', 0, NULL)"##,
    )
    .execute(concrete.pool())
    .await
    .unwrap();

    let log = repo.events_since(0, i64::MAX).await.unwrap();
    assert_eq!(log.len(), 1);
    let (_id, version, _scope, _ev) = &log[0];
    assert_eq!(
        *version, 1,
        "post-migration default backfills unstamped rows to version 1"
    );
}

// A row whose `scope_*` columns are NULL must load back as `EventScope::System`.

#[tokio::test]
async fn replay_falls_back_to_system_scope_on_null_columns() {
    let (repo, concrete, _bus) = boot().await;

    // Only the pre-scope columns are bound: `scope_kind` backfills from its default, the ancestor cols stay NULL.
    sqlx::query(
        r##"INSERT INTO events (kind, payload, actor, at, correlation, event_version)
           VALUES ('area.updated', '{"id":"c","name":"n","color":"#000","sort":0,"created_at":0,"updated_at":0}',
                   '"user"', 0, NULL, 1)"##,
    )
    .execute(concrete.pool())
    .await
    .unwrap();

    sqlx::query(
        r##"INSERT INTO events (kind, payload, actor, at, correlation, event_version,
                                 scope_kind, scope_area, scope_track, scope_card)
           VALUES ('area.updated', '{"id":"c2","name":"n2","color":"#000","sort":0,"created_at":0,"updated_at":0}',
                   '"user"', 0, NULL, 1, 'area', 'c2', NULL, NULL)"##,
    )
    .execute(concrete.pool())
    .await
    .unwrap();

    let log = repo.events_since(0, i64::MAX).await.unwrap();
    assert_eq!(log.len(), 2);

    let (_, _, scope, _) = &log[0];
    assert_eq!(
        *scope,
        EventScope::System,
        "NULL scope_* falls back to System"
    );

    let (_, _, scope, _) = &log[1];
    assert_eq!(
        *scope,
        EventScope::Area { area: "c2".into() },
        "post-PR2 row reconstructs typed scope"
    );
}
