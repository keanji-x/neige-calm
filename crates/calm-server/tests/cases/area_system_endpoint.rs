//! Integration tests for the `/api/areas` + `/api/areas/system` routes: the race-safe system-area upsert,
//! and `POST /api/areas` silently dropping a client-supplied `kind`.

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use calm_server::db::prelude::*;
use calm_server::db::sqlite::SqlxRepo;
use calm_server::event::EventBus;
use calm_server::model::AreaKind;
use calm_server::plugin_host::{PluginHost, PluginRegistry};
use calm_server::routes;
use calm_server::state::{AppState, CodexClient, DaemonClient};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tower::ServiceExt;

/// Boot a minimal Axum router with an in-memory SqlxRepo; no seeding, the tests exercise the area endpoints themselves.
async fn boot() -> (axum::Router, Arc<dyn Repo>) {
    let (app, repo, _concrete) = boot_with_concrete().await;
    (app, repo)
}

/// Same as `boot`, but also returns the concrete `Arc<SqlxRepo>` for raw SQL assertions on `events`.
async fn boot_with_concrete() -> (axum::Router, Arc<dyn Repo>, Arc<SqlxRepo>) {
    let concrete = Arc::new(
        SqlxRepo::open("sqlite::memory:")
            .await
            .expect("open in-memory sqlite repo"),
    );
    let repo: Arc<dyn Repo> = concrete.clone();
    let state = AppState::from_parts(
        repo.clone(),
        EventBus::new(),
        Arc::new(DaemonClient::new_stub()),
        Arc::new(PluginHost::new_full(
            Arc::new(PluginRegistry::empty()),
            repo.clone(),
            std::path::PathBuf::new(),
            std::env::temp_dir().join("calm-plugins-data"),
            Vec::new(),
            EventBus::new(),
            calm_server::state::WriteContext::new(
                calm_server::card_role_cache::CardRoleCache::new(),
                calm_server::track_area_cache::TrackAreaCache::new(),
            ),
        )),
        Arc::new(CodexClient::new_stub()),
        None,
        None,
    );
    let app = routes::router()
        .layer(axum::middleware::from_fn(
            calm_server::actor::actor_middleware,
        ))
        .with_state(state);
    (app, repo, concrete)
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

async fn post_empty(app: axum::Router, uri: &str) -> (StatusCode, Value) {
    // No request body; `content-type: application/json` mirrors what the frontend `apiPost` helper emits.
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(uri)
                .header("content-type", "application/json")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let json: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, json)
}

#[tokio::test]
async fn post_areas_system_first_call_returns_201() {
    let (app, repo) = boot().await;
    let (status, body) = post_empty(app, "/api/areas/system").await;
    assert_eq!(status, StatusCode::CREATED, "first call mints: {body:?}");
    assert_eq!(
        body["kind"], "system",
        "minted row has kind=system: {body:?}"
    );
    let row = repo
        .area_get_system()
        .await
        .unwrap()
        .expect("system area persisted");
    assert_eq!(row.kind, AreaKind::System);
}

#[tokio::test]
async fn post_areas_system_second_call_returns_200_existing_row() {
    let (app, _repo) = boot().await;
    let (s1, b1) = post_empty(app.clone(), "/api/areas/system").await;
    assert_eq!(s1, StatusCode::CREATED, "first call: {b1:?}");
    let id1 = b1["id"].as_str().expect("id present").to_string();

    let (s2, b2) = post_empty(app, "/api/areas/system").await;
    assert_eq!(
        s2,
        StatusCode::OK,
        "second sequential call returns existing row with 200: {b2:?}"
    );
    assert_eq!(
        b2["id"].as_str().unwrap(),
        id1,
        "same row id as the first call: {b2:?}"
    );
}

/// Two concurrent mints: the partial unique index fails the loser's INSERT and the handler must re-read and
/// return success. Multi-thread runtime + `Barrier` on purpose: `tokio::join!` on `current_thread` never reproduces the race.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn post_areas_system_concurrent_calls_both_succeed() {
    use std::sync::Arc as StdArc;
    use tokio::sync::Barrier;

    let (app, repo) = boot().await;
    let barrier = StdArc::new(Barrier::new(2));

    let app_a = app.clone();
    let barrier_a = barrier.clone();
    let handle_a = tokio::spawn(async move {
        barrier_a.wait().await;
        post_empty(app_a, "/api/areas/system").await
    });
    let app_b = app.clone();
    let barrier_b = barrier.clone();
    let handle_b = tokio::spawn(async move {
        barrier_b.wait().await;
        post_empty(app_b, "/api/areas/system").await
    });

    let (status_a, body_a) = handle_a.await.expect("racer A panicked");
    let (status_b, body_b) = handle_b.await.expect("racer B panicked");

    assert!(
        status_a.is_success(),
        "first racer must succeed (not 5xx): status={status_a} body={body_a:?}"
    );
    assert!(
        status_b.is_success(),
        "second racer must succeed (not 5xx): status={status_b} body={body_b:?}"
    );
    assert_eq!(
        body_a["kind"], "system",
        "first racer body carries kind=system: {body_a:?}"
    );
    assert_eq!(
        body_b["kind"], "system",
        "second racer body carries kind=system: {body_b:?}"
    );
    let id_a = body_a["id"].as_str().expect("first racer id");
    let id_b = body_b["id"].as_str().expect("second racer id");
    assert_eq!(
        id_a, id_b,
        "both racers see the same singleton row id: a={id_a} b={id_b}"
    );

    let all = repo.areas_list().await.unwrap();
    let system_rows: Vec<_> = all.iter().filter(|c| c.kind == AreaKind::System).collect();
    assert_eq!(
        system_rows.len(),
        1,
        "exactly one kind='system' row after the race: {system_rows:?}"
    );
}

/// `CreateAreaRequest` deliberately has no `kind`; serde drops the unknown field and the row lands as `User`.
/// Adding `kind` to the request would let any client claim the singleton system slot.
#[tokio::test]
async fn post_areas_silently_drops_kind_field_lands_as_user() {
    let (app, repo) = boot().await;

    let (status, body) = post(
        app,
        "/api/areas",
        json!({ "name": "trojan", "color": "#bad", "kind": "system" }),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::CREATED,
        "POST /api/areas with unknown `kind` field still returns 201 (serde drops it): body={body:?}"
    );
    assert_eq!(
        body["kind"], "user",
        "the unknown `kind` was ignored and the row landed as User: {body:?}"
    );

    let id = body["id"].as_str().expect("created id");
    let row = repo
        .area_get(id)
        .await
        .unwrap()
        .expect("created area persisted");
    assert_eq!(row.kind, AreaKind::User);
    assert!(
        repo.area_get_system().await.unwrap().is_none(),
        "no system row should be created by the public POST surface"
    );
}

/// The 403 guard lives at the public boundary, so `area_delete_tx` stays a no-policy primitive.
#[tokio::test]
async fn delete_system_area_via_rest_is_forbidden() {
    let (app, repo) = boot().await;

    let (status, body) = post_empty(app.clone(), "/api/areas/system").await;
    assert_eq!(status, StatusCode::CREATED, "mint system area: {body:?}");
    let system_id = body["id"].as_str().expect("system area id").to_string();

    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri(format!("/api/areas/{system_id}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let delete_status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let delete_body: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    assert_eq!(
        delete_status,
        StatusCode::FORBIDDEN,
        "DELETE on system area must be forbidden (got {delete_status}): {delete_body:?}"
    );
    assert_eq!(
        delete_body["code"], "forbidden",
        "error body carries the `forbidden` code: {delete_body:?}"
    );

    let still_there = repo
        .area_get_system()
        .await
        .unwrap()
        .expect("system area still present after rejected delete");
    assert_eq!(still_there.id.as_str(), system_id);
    assert_eq!(still_there.kind, AreaKind::System);

    // The guard is targeted at `kind = 'system'`, not a blanket "no deletes".
    let (create_status, create_body) = post(
        app.clone(),
        "/api/areas",
        json!({ "name": "u", "color": "#000" }),
    )
    .await;
    assert_eq!(create_status, StatusCode::CREATED, "user area created");
    let user_id = create_body["id"].as_str().expect("user area id");
    let user_del = app
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri(format!("/api/areas/{user_id}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        user_del.status(),
        StatusCode::NO_CONTENT,
        "user area delete still works"
    );
}

/// The system area is kernel-owned scaffolding: the mint's event must carry `actor = Kernel`, not the middleware's `User`.
#[tokio::test]
async fn post_areas_system_stamps_kernel_actor_in_events() {
    let (app, _repo, concrete) = boot_with_concrete().await;

    let (status, body) = post_empty(app, "/api/areas/system").await;
    assert_eq!(status, StatusCode::CREATED, "mint succeeded: {body:?}");

    let row: (String, String) =
        sqlx::query_as("SELECT kind, actor FROM events ORDER BY id DESC LIMIT 1")
            .fetch_one(concrete.pool())
            .await
            .expect("read latest event row");
    assert_eq!(
        row.0, "area.updated",
        "latest event is the system area mint: {row:?}"
    );
    let actor: Value =
        serde_json::from_str(&row.1).expect("events.actor is JSON-serialized ActorId");
    assert_eq!(
        actor,
        json!({ "kind": "Kernel" }),
        "system area mint stamps Kernel actor, not User: {actor}"
    );
}

/// `ai:codex` rather than the bare default so a regression back to `actor.to_actor_id()` is observable as `AiCodex(<empty>)`.
#[tokio::test]
async fn post_areas_system_ignores_caller_actor_header() {
    let (app, _repo, concrete) = boot_with_concrete().await;

    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/areas/system")
                .header("content-type", "application/json")
                .header("X-Calm-Actor", "ai:codex")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::CREATED,
        "mint succeeded even with ai:codex header"
    );

    let row: (String, String) =
        sqlx::query_as("SELECT kind, actor FROM events ORDER BY id DESC LIMIT 1")
            .fetch_one(concrete.pool())
            .await
            .expect("read latest event row");
    assert_eq!(row.0, "area.updated");
    let actor: Value =
        serde_json::from_str(&row.1).expect("events.actor is JSON-serialized ActorId");
    assert_eq!(
        actor,
        json!({ "kind": "Kernel" }),
        "Kernel override wins over the declared `ai:codex` header: {actor}"
    );
}
