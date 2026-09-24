//! Atomic planner card binding on track create: a real Axum router via `AppState::from_parts`, driven with
//! `tower::ServiceExt`, asserting persisted state + the event broadcast stream.

#![cfg(unix)]

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use calm_server::card_role_cache::CardRoleCache;
use calm_server::db::prelude::*;
use calm_server::db::sqlite::SqlxRepo;
use calm_server::event::{BroadcastEnvelope, Event, EventBus, EventScope};
use calm_server::ids::{ActorId, CardId, TrackId};
use calm_server::model::{CardRole, NewArea};
use calm_server::plugin_host::{PluginHost, PluginRegistry};
use calm_server::role_gate::enforce_role;
use calm_server::routes;
use calm_server::state::{AppState, CodexClient, DaemonClient};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tempfile::TempDir;
use tower::ServiceExt;

use crate::support::git_helpers::attached_repo_fixture;

struct Boot {
    app: axum::Router,
    area_id: String,
    events: EventBus,
    repo: Arc<dyn Repo>,
    card_role_cache: CardRoleCache,
    _tmp: TempDir,
}

/// Boot a router pointing at a non-existent codex bin: the shared daemon start fails, but `POST /api/tracks` still commits and returns 201 with an inert planner card.
async fn boot() -> Boot {
    let tmp = TempDir::new().expect("tempdir for daemon sockets");
    let repo: Arc<dyn Repo> = Arc::new(
        SqlxRepo::open("sqlite::memory:")
            .await
            .expect("open in-memory sqlite"),
    );
    let area = repo
        .area_create(NewArea {
            name: "planner-card-test".into(),
            color: "#000".into(),
            sort: None,
        })
        .await
        .unwrap();

    let daemon = Arc::new(DaemonClient {
        data_dir: tmp.path().to_path_buf(),
        proc_supervisor_sock: None,
    });
    let events = EventBus::new();
    let card_role_cache = CardRoleCache::new();
    let track_area_cache = calm_server::track_area_cache::TrackAreaCache::new();
    repo.seed_track_area_cache(&track_area_cache).await.unwrap();
    let state = AppState::from_parts(
        repo.clone(),
        events.clone(),
        daemon,
        Arc::new(PluginHost::new_full(
            Arc::new(PluginRegistry::empty()),
            repo.clone(),
            PathBuf::new(),
            std::env::temp_dir().join("calm-plugins-data-planner-test"),
            Vec::new(),
            EventBus::new(),
            calm_server::state::WriteContext::new(
                card_role_cache.clone(),
                track_area_cache.clone(),
            ),
        )),
        {
            // Deterministically-broken codex bin (absolute, absent) so the app-server boot fails fast regardless of PATH.
            let mut codex = CodexClient::new_stub();
            codex.codex_bin = "/nonexistent-codex-bin-planner-card-test".into();
            Arc::new(codex)
        },
        Some(card_role_cache.clone()),
        Some(track_area_cache.clone()),
    );

    let app = routes::router()
        .layer(axum::middleware::from_fn(
            calm_server::actor::actor_middleware,
        ))
        .with_state(state);

    Boot {
        app,
        area_id: area.id.to_string(),
        events,
        repo,
        card_role_cache,
        _tmp: tmp,
    }
}

async fn post(app: axum::Router, uri: &str, body: Value) -> (StatusCode, Value) {
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(uri)
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let json: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, json)
}

/// Drain at least `n` envelopes from a broadcast subscriber, panicking if the deadline elapses.
async fn collect_envelopes(events: &EventBus, n: usize) -> Vec<BroadcastEnvelope> {
    let mut rx = events.subscribe_filtered();
    let mut out = Vec::with_capacity(n);
    let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
    while out.len() < n {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            panic!(
                "expected {n} envelopes within deadline; got {} so far: {:?}",
                out.len(),
                out
            );
        }
        match tokio::time::timeout(remaining, rx.recv()).await {
            Ok(Ok(env)) => out.push(env),
            Ok(Err(e)) => panic!("broadcast recv error: {e:?}"),
            Err(_) => continue,
        }
    }
    out
}

#[tokio::test]
async fn post_api_tracks_mints_planner_card_atomically() {
    let boot = boot().await;

    // Subscribe before firing: the daemon spawn fails *after* the events already broadcast. The two CardAdded
    // envelopes are distinguishable by `card.kind` ("codex" vs "track-report").
    let subscription = {
        let events = boot.events.clone();
        tokio::spawn(async move { collect_envelopes(&events, 4).await })
    };
    tokio::time::sleep(Duration::from_millis(20)).await;

    let (status, body) = post(
        boot.app.clone(),
        "/api/tracks",
        json!({"planner_provider": "codex", "area_id": boot.area_id, "title": "first track", "cwd": attached_repo_fixture("issue-250-pr2-test"), "attach_folder": true, "theme": {"fg": [216,219,226], "bg": [15,20,24]} }),
    )
    .await;
    // The app-server boot is non-fatal: with a broken codex bin the route still returns 201.
    assert_eq!(
        status,
        StatusCode::CREATED,
        "broken codex bin → 201 (boot is non-fatal, #293/#311); persisted rows + events still survive; body={body}",
    );

    let envelopes = tokio::time::timeout(Duration::from_secs(3), subscription)
        .await
        .expect("collector finished")
        .expect("collector task ok");

    assert!(
        matches!(&envelopes[0].event, Event::TrackUpdated(_)),
        "first envelope must be TrackUpdated; got: {:?}",
        envelopes[0].event,
    );
    assert!(
        matches!(&envelopes[0].scope, EventScope::Track { .. }),
        "TrackUpdated must be track-scoped; got: {:?}",
        envelopes[0].scope,
    );
    assert_eq!(envelopes[0].actor, ActorId::User);

    assert!(
        matches!(&envelopes[1].event, Event::CardAdded(_)),
        "second envelope must be CardAdded(planner); got: {:?}",
        envelopes[1].event,
    );
    assert!(
        matches!(&envelopes[1].scope, EventScope::Card { .. }),
        "CardAdded must be card-scoped; got: {:?}",
        envelopes[1].scope,
    );
    assert_eq!(envelopes[1].actor, ActorId::User);

    assert!(
        matches!(&envelopes[2].event, Event::CardAdded(_)),
        "third envelope must be CardAdded(track-report); got: {:?}",
        envelopes[2].event,
    );
    let planner_card_id = match &envelopes[1].event {
        Event::CardAdded(c) => {
            assert_eq!(c.kind, "codex", "second envelope is the planner card");
            c.id.clone()
        }
        _ => unreachable!(),
    };
    match &envelopes[2].event {
        Event::CardAdded(c) => {
            assert_eq!(
                c.kind, "track-report",
                "third envelope is the track-report card"
            );
            assert!(!c.deletable, "track-report card is kernel-owned");
        }
        _ => unreachable!(),
    }

    assert!(
        matches!(&envelopes[3].event, Event::OverlaySet(_)),
        "fourth envelope must be OverlaySet(layout); got: {:?}",
        envelopes[3].event,
    );

    assert_eq!(
        boot.card_role_cache.get(&planner_card_id),
        Some(CardRole::Planner),
        "planner card's role must be Planner in the cache",
    );

    let track_id = match &envelopes[0].event {
        Event::TrackUpdated(w) => w.id.clone(),
        _ => unreachable!(),
    };
    let cards = boot.repo.cards_by_track(track_id.as_str()).await.unwrap();
    assert_eq!(
        cards.len(),
        2,
        "planner + track-report card per track at create",
    );
    let planner_in_db = cards
        .iter()
        .find(|c| c.kind == "codex")
        .expect("planner card in db");
    assert_eq!(planner_in_db.id, planner_card_id);
    assert!(
        cards.iter().any(|c| c.kind == "track-report"),
        "track-report card in db",
    );
    let term = boot
        .repo
        .terminal_get_by_card(planner_card_id.as_str())
        .await
        .unwrap();
    assert!(
        term.is_none(),
        "inert planner card should not have a terminal row"
    );
}

#[tokio::test]
async fn planner_card_can_emit_track_updated_via_enforce_role() {
    // The card is minted directly via the cache and the gate is called, without going through the route.
    let cache = CardRoleCache::new();
    let planner_id = CardId::from("planner-card-pr6");
    cache.insert(planner_id.clone(), CardRole::Planner, TrackId::from("w"));

    let evt = Event::TrackUpdated(calm_server::event::TrackUpdatedPayload::new(
        calm_server::model::Track {
            id: "w".into(),
            area_id: "c".into(),
            title: "t".into(),
            sort: 1.0,
            archived_at: None,
            pinned_at: None,
            lifecycle: calm_server::model::TrackLifecycle::Draft,
            cwd_wire_alias: String::new(),
            template_id: None,
            plugin_scope: None,
            purpose: None,
            template_input: None,
            terminal_at: None,
            recipe_id: None,
            recipe_revision: None,
            claude_permissions_policy: None,
            workspace: Default::default(),
            created_at: 0,
            updated_at: 0,
        },
        None,
    ));
    let scope = EventScope::Track {
        track: "w".into(),
        area: "c".into(),
    };
    let wcc = calm_server::track_area_cache::TrackAreaCache::new();
    let res = enforce_role(
        &ActorId::AiPlanner(planner_id.clone()),
        &evt,
        &scope,
        &cache,
        &wcc,
    );
    assert!(
        res.is_ok(),
        "AiPlanner(planner-card) must be permitted to emit TrackUpdated: {res:?}",
    );
}

#[tokio::test]
async fn write_with_events_typed_persists_and_broadcasts_multiple_in_order() {
    use calm_server::db::sqlite::{area_create_tx, track_create_tx};
    use calm_server::db::write_with_events_typed;
    use calm_server::model::{NewArea, NewTrack};

    let repo: Arc<dyn Repo> = Arc::new(
        SqlxRepo::open("sqlite::memory:")
            .await
            .expect("open in-memory sqlite"),
    );
    let events = EventBus::new();
    let cache = CardRoleCache::new();
    let wcc = calm_server::track_area_cache::TrackAreaCache::new();

    let mut rx = events.subscribe_filtered();

    let event_ids: Vec<i64> = write_with_events_typed(
        repo.as_ref(),
        ActorId::User,
        None,
        &events,
        &calm_server::state::WriteContext::new(cache.clone(), wcc.clone()),
        |tx| {
            Box::pin(async move {
                let area = area_create_tx(
                    tx,
                    NewArea {
                        name: "plural".into(),
                        color: "#fff".into(),
                        sort: None,
                    },
                )
                .await?;
                let track = track_create_tx(
                    tx,
                    NewTrack {
                        template_input: None,
                        area_id: area.id.clone(),
                        title: "plural-track".into(),
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
                let area_scope = EventScope::Area {
                    area: area.id.clone(),
                };
                let track_scope = EventScope::Track {
                    track: track.id.clone(),
                    area: area.id.clone(),
                };
                Ok((
                    (),
                    vec![
                        (area_scope, Event::AreaUpdated(area)),
                        (
                            track_scope,
                            Event::TrackUpdated(calm_server::event::TrackUpdatedPayload::new(
                                track, None,
                            )),
                        ),
                    ],
                ))
            })
        },
    )
    .await
    .expect("plural write succeeds")
    .1;

    assert_eq!(event_ids.len(), 2, "two event ids returned");
    assert!(
        event_ids[1] > event_ids[0],
        "event ids monotonically increasing: {event_ids:?}",
    );

    let env1 = tokio::time::timeout(Duration::from_secs(1), rx.recv())
        .await
        .expect("first envelope arrives")
        .unwrap();
    let env2 = tokio::time::timeout(Duration::from_secs(1), rx.recv())
        .await
        .expect("second envelope arrives")
        .unwrap();
    assert!(matches!(env1.event, Event::AreaUpdated(_)));
    assert!(matches!(env1.scope, EventScope::Area { .. }));
    assert!(matches!(env2.event, Event::TrackUpdated(_)));
    assert!(matches!(env2.scope, EventScope::Track { .. }));
}

#[tokio::test]
async fn write_with_events_typed_rolls_back_when_closure_errors() {
    use calm_server::db::sqlite::area_create_tx;
    use calm_server::db::write_with_events_typed;
    use calm_server::error::CalmError;
    use calm_server::model::NewArea;

    let repo: Arc<dyn Repo> = Arc::new(
        SqlxRepo::open("sqlite::memory:")
            .await
            .expect("open in-memory sqlite"),
    );
    let events = EventBus::new();
    let cache = CardRoleCache::new();
    let wcc = calm_server::track_area_cache::TrackAreaCache::new();

    assert!(repo.areas_list().await.unwrap().is_empty());

    let mut rx = events.subscribe_filtered();
    let res = write_with_events_typed::<(), _>(
        repo.as_ref(),
        ActorId::User,
        None,
        &events,
        &calm_server::state::WriteContext::new(cache.clone(), wcc.clone()),
        |tx| {
            Box::pin(async move {
                let _area = area_create_tx(
                    tx,
                    NewArea {
                        name: "doomed".into(),
                        color: "#000".into(),
                        sort: None,
                    },
                )
                .await?;
                Err(CalmError::Internal("closure aborts after writing".into()))
            })
        },
    )
    .await;

    assert!(res.is_err(), "closure error must bubble");
    assert!(
        repo.areas_list().await.unwrap().is_empty(),
        "area row must be rolled back",
    );
    assert!(
        rx.try_recv().is_err(),
        "rolled-back tx must not broadcast any envelope",
    );
}

#[tokio::test]
async fn write_with_events_typed_rolls_back_on_enforce_role_violation() {
    use calm_server::db::sqlite::{area_create_tx, track_create_tx};
    use calm_server::db::write_with_events_typed;
    use calm_server::model::{NewArea, NewTrack};

    let repo: Arc<dyn Repo> = Arc::new(
        SqlxRepo::open("sqlite::memory:")
            .await
            .expect("open in-memory sqlite"),
    );
    let events = EventBus::new();
    let cache = CardRoleCache::new();

    // `AiCodex(known-worker)` cannot emit TrackUpdated per enforce_role; the second event trips the gate and everything must roll back.
    let worker_id = CardId::from("worker-card-id");
    cache.insert(
        worker_id.clone(),
        CardRole::Worker,
        TrackId::from("worker-track"),
    );
    let wcc = calm_server::track_area_cache::TrackAreaCache::new();

    assert!(repo.areas_list().await.unwrap().is_empty());

    let mut rx = events.subscribe_filtered();
    let wcc_for_tx = wcc.clone();
    let res = write_with_events_typed::<(), _>(
        repo.as_ref(),
        ActorId::AiCodex(worker_id),
        None,
        &events,
        &calm_server::state::WriteContext::new(cache.clone(), wcc.clone()),
        move |tx| {
            Box::pin(async move {
                let area = area_create_tx(
                    tx,
                    NewArea {
                        name: "gated".into(),
                        color: "#000".into(),
                        sort: None,
                    },
                )
                .await?;
                let track = track_create_tx(
                    tx,
                    NewTrack {
                        template_input: None,
                        area_id: area.id.clone(),
                        title: "gated-track".into(),
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
                    &wcc_for_tx,
                )
                .await?;
                let area_scope = EventScope::Area {
                    area: area.id.clone(),
                };
                let track_scope = EventScope::Track {
                    track: track.id.clone(),
                    area: area.id.clone(),
                };
                Ok((
                    (),
                    vec![
                        // The first event passes the gate (enforce_role only gates TrackUpdated); the second violates.
                        (area_scope, Event::AreaUpdated(area)),
                        (
                            track_scope,
                            Event::TrackUpdated(calm_server::event::TrackUpdatedPayload::new(
                                track, None,
                            )),
                        ),
                    ],
                ))
            })
        },
    )
    .await;

    assert!(res.is_err(), "role violation must surface as Err");
    assert!(
        repo.areas_list().await.unwrap().is_empty(),
        "area must be rolled back when any later event in the batch trips the gate",
    );
    // Commit-then-emit means the rollback suppresses every event, not just the violating one.
    assert!(
        rx.try_recv().is_err(),
        "rolled-back tx must not broadcast any envelope",
    );
}
