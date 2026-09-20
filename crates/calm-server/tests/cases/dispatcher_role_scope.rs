//! Cross-layer role-gate + scope coverage for the track-as-actor dispatcher pathway.

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use calm_server::actor::{Actor, actor_middleware};
use calm_server::card_role_cache::CardRoleCache;
use calm_server::db::prelude::*;
use calm_server::db::sqlite::SqlxRepo;
use calm_server::event::{Event, EventBus, EventScope};
use calm_server::ids::{ActorId, AreaId, CardId, TrackId};
use calm_server::model::{CardRole, NewArea, NewCard, NewTrack};
use calm_server::track_area_cache::TrackAreaCache;
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tower::ServiceExt;

async fn boot_repo() -> (Arc<SqlxRepo>, EventBus, CardRoleCache, TrackAreaCache) {
    let repo = Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap());
    let bus = EventBus::new();
    let cache = CardRoleCache::new();
    repo.seed_card_role_cache(&cache).await.unwrap();
    let wcc = TrackAreaCache::new();
    repo.seed_track_area_cache(&wcc).await.unwrap();
    (repo, bus, cache, wcc)
}

/// Seed an area + track + Worker-roled card. The role lands in both the cards row and the
/// in-memory cache; the track's area also lands in `wcc` so the gate's area check passes.
async fn seed_worker_in_track(
    repo: &SqlxRepo,
    cache: &CardRoleCache,
    wcc: &TrackAreaCache,
    area_name: &str,
    track_title: &str,
) -> (AreaId, TrackId, CardId) {
    let area = repo
        .area_create(NewArea {
            name: area_name.into(),
            color: "#000".into(),
            sort: None,
        })
        .await
        .unwrap();
    let track = repo
        .track_create(NewTrack {
            template_input: None,
            area_id: area.id.clone(),
            title: track_title.into(),
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
        .card_create(NewCard {
            track_id: track.id.clone(),
            title: None,
            kind: "codex".into(),
            sort: None,
            payload: json!({}),
        })
        .await
        .unwrap();
    sqlx::query("UPDATE cards SET role = 'worker' WHERE id = ?1")
        .bind(card.id.as_str())
        .execute(repo.pool())
        .await
        .unwrap();
    cache.insert(
        card.id.clone(),
        CardRole::Worker,
        TrackId::from(track.id.as_str()),
    );
    wcc.insert(
        TrackId::from(track.id.as_str()),
        AreaId::from(area.id.as_str()),
    );
    (
        AreaId::from(area.id.as_str()),
        TrackId::from(track.id.as_str()),
        CardId::from(card.id.as_str()),
    )
}

fn task_completed(idem: &str) -> Event {
    Event::TaskCompleted {
        idempotency_key: idem.into(),
        result: serde_json::Value::Null,
        artifacts: Vec::new(),
        agent_message: None,
    }
}

async fn count_events(repo: &SqlxRepo, kind: &str) -> i64 {
    let row: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM events WHERE kind = ?1")
        .bind(kind)
        .fetch_one(repo.pool())
        .await
        .unwrap();
    row.0
}

#[tokio::test]
async fn worker_emitting_track_scope_is_rejected() {
    let (repo, bus, cache, wcc) = boot_repo().await;
    let (area, track, worker) = seed_worker_in_track(&repo, &cache, &wcc, "c", "w").await;
    let mut sub = bus.subscribe();

    let baseline_total = count_events(&repo, "task.completed").await;

    let scope = EventScope::Track {
        track: track.clone(),
        area: area.clone(),
    };
    let res = repo
        .log_pure_event(
            ActorId::AiCodex(worker.clone()),
            scope,
            None,
            &bus,
            &cache,
            &wcc,
            task_completed("worker-track-1"),
        )
        .await;

    assert!(
        matches!(
            res,
            Err(calm_server::error::CalmError::Forbidden(ref msg))
                if msg.contains("out of scope")
        ),
        "Worker emitting track scope must be refused: {res:?}",
    );

    let after = count_events(&repo, "task.completed").await;
    assert_eq!(
        after, baseline_total,
        "rejected worker write must not append an event row",
    );

    // Broadcast-after-commit invariant.
    assert!(sub.try_recv().is_err(), "rejected write must not broadcast",);
}

#[tokio::test]
async fn worker_emitting_own_card_scope_is_accepted() {
    let (repo, bus, cache, wcc) = boot_repo().await;
    let (area, track, worker) = seed_worker_in_track(&repo, &cache, &wcc, "c", "w").await;
    let mut sub = bus.subscribe();

    let scope = EventScope::Card {
        card: worker.clone(),
        track: track.clone(),
        area: area.clone(),
    };
    let res = repo
        .log_pure_event(
            ActorId::AiCodex(worker.clone()),
            scope,
            None,
            &bus,
            &cache,
            &wcc,
            task_completed("worker-own-1"),
        )
        .await;
    assert!(
        res.is_ok(),
        "Worker emitting its own card scope must succeed: {res:?}",
    );

    let env = sub.try_recv().expect("envelope on bus");
    assert!(matches!(env.event, Event::TaskCompleted { .. }));
    assert!(matches!(
        env.actor,
        ActorId::AiCodex(ref c) if c == &worker,
    ));
}

#[tokio::test]
async fn worker_emitting_other_card_scope_is_rejected() {
    let (repo, bus, cache, wcc) = boot_repo().await;
    let (area, track, worker_a) = seed_worker_in_track(&repo, &cache, &wcc, "c", "w").await;

    // Also Worker-roled, so the refusal hinges on the scope.card mismatch, not on a role lookup failure.
    let card_b = repo
        .card_create(NewCard {
            track_id: track.as_str().into(),
            title: None,
            kind: "codex".into(),
            sort: None,
            payload: json!({}),
        })
        .await
        .unwrap();
    sqlx::query("UPDATE cards SET role = 'worker' WHERE id = ?1")
        .bind(card_b.id.as_str())
        .execute(repo.pool())
        .await
        .unwrap();
    cache.insert(card_b.id.clone(), CardRole::Worker, track.clone());

    let scope = EventScope::Card {
        card: CardId::from(card_b.id.as_str()),
        track: track.clone(),
        area: area.clone(),
    };
    let res = repo
        .log_pure_event(
            ActorId::AiCodex(worker_a.clone()),
            scope,
            None,
            &bus,
            &cache,
            &wcc,
            task_completed("worker-cross-card"),
        )
        .await;
    assert!(
        matches!(
            res,
            Err(calm_server::error::CalmError::Forbidden(ref msg))
                if msg.contains("out of scope")
        ),
        "Worker A emitting into Worker B's scope must be refused: {res:?}",
    );
}

// Exercised via a probe route through the real middleware so regressions in the wiring layer show.

#[tokio::test]
async fn missing_actor_header_defaults_to_user() {
    use axum::Router;
    use axum::extract::Extension;
    use axum::routing::get;

    async fn probe(Extension(actor): Extension<Actor>) -> String {
        actor.as_str().to_string()
    }

    let app = Router::new()
        .route("/probe", get(probe))
        .layer(axum::middleware::from_fn(actor_middleware));

    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/probe")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    assert_eq!(
        std::str::from_utf8(&body).unwrap(),
        Actor::DEFAULT,
        "missing X-Calm-Actor must default to `user` — the contract older bridges rely on",
    );

    // An empty-string header collapses to the same default.
    let resp = app
        .oneshot(
            Request::builder()
                .uri("/probe")
                .header(Actor::HEADER, "")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = resp.into_body().collect().await.unwrap().to_bytes();
    assert_eq!(std::str::from_utf8(&body).unwrap(), Actor::DEFAULT);
}

#[tokio::test]
async fn worker_with_mismatched_track_in_card_scope_is_rejected() {
    let (repo, bus, cache, wcc) = boot_repo().await;
    let (area_a, _track_a, worker_a) =
        seed_worker_in_track(&repo, &cache, &wcc, "area-a", "track-a").await;
    let (_area_b, track_b, _worker_b) =
        seed_worker_in_track(&repo, &cache, &wcc, "area-b", "track-b").await;

    let baseline_total = count_events(&repo, "task.completed").await;
    let mut sub = bus.subscribe();

    // Forge an `EventScope::Card` whose `card` is Worker A's id but whose `track` is Track B.
    let scope = EventScope::Card {
        card: worker_a.clone(),
        track: track_b.clone(),
        area: area_a.clone(),
    };
    let res = repo
        .log_pure_event(
            ActorId::AiCodex(worker_a.clone()),
            scope,
            None,
            &bus,
            &cache,
            &wcc,
            task_completed("worker-a-into-track-b"),
        )
        .await;
    assert!(
        matches!(
            res,
            Err(calm_server::error::CalmError::Forbidden(ref msg))
                if msg.contains("out of scope") && msg.contains("scope.track mismatch")
        ),
        "Worker A forging scope.track = Track B must be refused (#232): {res:?}",
    );

    let after = count_events(&repo, "task.completed").await;
    assert_eq!(
        after, baseline_total,
        "rejected worker write must not append an event row",
    );
    let forged_row: Option<(Option<String>,)> = sqlx::query_as(
        "SELECT scope_track FROM events \
         WHERE kind = 'task.completed' \
           AND json_extract(payload, '$.idempotency_key') = 'worker-a-into-track-b'",
    )
    .fetch_optional(repo.pool())
    .await
    .unwrap();
    assert!(
        forged_row.is_none(),
        "no event row should exist for the forged scope.track: {forged_row:?}",
    );

    // Broadcast-after-commit invariant.
    assert!(sub.try_recv().is_err(), "rejected write must not broadcast");
}

#[tokio::test]
async fn worker_with_mismatched_area_in_card_scope_is_rejected() {
    let (repo, bus, cache, wcc) = boot_repo().await;
    let (_area_a, track_a, worker_a) =
        seed_worker_in_track(&repo, &cache, &wcc, "area-a", "track-a").await;
    let (area_b, _track_b, _worker_b) =
        seed_worker_in_track(&repo, &cache, &wcc, "area-b", "track-b").await;

    let baseline_total = count_events(&repo, "task.completed").await;
    let mut sub = bus.subscribe();

    // Forge an `EventScope::Card` whose `card` and `track` match but whose `area` is Area B.
    let scope = EventScope::Card {
        card: worker_a.clone(),
        track: track_a.clone(),
        area: area_b.clone(),
    };
    let res = repo
        .log_pure_event(
            ActorId::AiCodex(worker_a.clone()),
            scope,
            None,
            &bus,
            &cache,
            &wcc,
            task_completed("worker-a-into-area-b"),
        )
        .await;
    assert!(
        matches!(
            res,
            Err(calm_server::error::CalmError::Forbidden(ref msg))
                if msg.contains("out of scope") && msg.contains("scope.area mismatch")
        ),
        "Worker A forging scope.area = Area B must be refused (#234): {res:?}",
    );

    let after = count_events(&repo, "task.completed").await;
    assert_eq!(
        after, baseline_total,
        "rejected worker write must not append an event row",
    );
    let forged_row: Option<(Option<String>,)> = sqlx::query_as(
        "SELECT scope_area FROM events \
         WHERE kind = 'task.completed' \
           AND json_extract(payload, '$.idempotency_key') = 'worker-a-into-area-b'",
    )
    .fetch_optional(repo.pool())
    .await
    .unwrap();
    assert!(
        forged_row.is_none(),
        "no event row should exist for the forged scope.area: {forged_row:?}",
    );

    // Broadcast-after-commit invariant.
    assert!(sub.try_recv().is_err(), "rejected write must not broadcast");
}

#[tokio::test]
async fn planner_emitting_track_scope_is_accepted() {
    let (repo, bus, cache, wcc) = boot_repo().await;
    let area = repo
        .area_create(NewArea {
            name: "c".into(),
            color: "#000".into(),
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
    let planner = repo
        .card_create(NewCard {
            track_id: track.id.clone(),
            title: None,
            kind: "planner".into(),
            sort: None,
            payload: json!({}),
        })
        .await
        .unwrap();
    sqlx::query("UPDATE cards SET role = 'planner' WHERE id = ?1")
        .bind(planner.id.as_str())
        .execute(repo.pool())
        .await
        .unwrap();
    cache.insert(
        planner.id.clone(),
        CardRole::Planner,
        TrackId::from(track.id.as_str()),
    );

    let scope = EventScope::Track {
        track: TrackId::from(track.id.as_str()),
        area: AreaId::from(area.id.as_str()),
    };
    let res = repo
        .log_pure_event(
            ActorId::AiPlanner(CardId::from(planner.id.as_str())),
            scope,
            None,
            &bus,
            &cache,
            &wcc,
            Event::CodexWorkerRequested {
                idempotency_key: "planner-pos-1".into(),
                goal: "go".into(),
                context: Value::Null,
                acceptance_criteria: None,
                agent_message: None,
            },
        )
        .await;
    assert!(
        res.is_ok(),
        "Planner card emitting Track-scoped CodexWorkerRequested must be accepted: {res:?}",
    );
}
