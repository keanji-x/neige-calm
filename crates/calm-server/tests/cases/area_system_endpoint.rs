//! Integration tests for the `/api/areas` + `/api/areas/system` routes
//! that came with issue #175 (system area kind).
//!
//! Two contracts under test:
//!
//!   1. **Race-safe upsert.** `POST /api/areas/system` is hit from every
//!      cold-boot Today-page load. Two concurrent tabs can both see
//!      `area_get_system() == None` and both reach the mint closure; the
//!      partial unique index on `areas(kind) WHERE kind = 'system'`
//!      (migration 0009) fails the loser's INSERT. The route handler
//!      catches that DB error, re-reads, and returns 200 instead of 500.
//!      We simulate the race with `tokio::join!` and assert both calls
//!      surface a successful response and the DB ends up with exactly
//!      one system row.
//!
//!   2. **`POST /api/areas` silently ignores `kind`.** `CreateAreaRequest`
//!      deliberately omits a `kind` field (and `serde` is permissive by
//!      default — `deny_unknown_fields` is *not* set), so a client
//!      payload like `{"name":"x","color":"#000","kind":"system"}` is
//!      accepted, the unknown field is dropped, and the row lands as
//!      `AreaKind::User`. This test pins that behavior so a future
//!      well-meaning patch that adds `kind` to `CreateAreaRequest` lights up here
//!      before it ships — promoting a user area to the singleton system
//!      kind through the public surface would break #175's invariants.

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

/// Boot a minimal Axum router with an in-memory SqlxRepo. Shape mirrors
/// `payload_validation.rs::boot` — no area/track seeding here because the
/// tests exercise the area endpoints themselves.
async fn boot() -> (axum::Router, Arc<dyn Repo>) {
    let (app, repo, _concrete) = boot_with_concrete().await;
    (app, repo)
}

/// Same shape as `boot`, but additionally returns the concrete
/// `Arc<SqlxRepo>` so callers can reach `repo.pool()` for raw SQL
/// assertions on the `events` table (used by the actor-stamp test below).
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
    // `POST /api/areas/system` takes no request body. Axum accepts an
    // empty body for handlers without a `Json<T>` extractor — we still
    // set `content-type: application/json` so the request mirrors what
    // the frontend `api/calm.ts` `apiPost` helper emits.
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

/// Issue #175 — race regression test. Two concurrent `POST
/// /api/areas/system` calls can both see `area_get_system() == None`
/// in the pre-check and race into the mint closure. The partial unique
/// index on `areas(kind) WHERE kind = 'system'` fails the loser's
/// INSERT; the route handler must catch the DB error, re-read the
/// winner's row, and return a successful response — not a 500.
///
/// Before the race-safety fix this test surfaced as one 201 + one 500;
/// after the fix it's one 201 + one 200 (or two 200s if both racers
/// happen to fall through into the catch path after the index already
/// failed both). Either successful pairing is acceptable; the
/// post-conditions we pin are:
///   * neither response is a 5xx,
///   * both bodies carry `kind == "system"` and the same `id`,
///   * the DB contains exactly one `kind='system'` row.
///
/// We run on the multi-thread runtime + `tokio::spawn` each racer onto
/// its own task + bracket with a `tokio::sync::Barrier` so both racers
/// actually arrive at the handler at the same time. A naive
/// `tokio::join!` on the default `current_thread` runtime cooperatively
/// schedules one future to completion before yielding, and the race
/// never reproduces under `oneshot`'s short hot loop — we observed this
/// while writing the test, so the multi-thread + barrier shape is
/// deliberate, not boilerplate.
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

    // DB-side invariant: exactly one system row, no duplicates leaked.
    let all = repo.areas_list().await.unwrap();
    let system_rows: Vec<_> = all.iter().filter(|c| c.kind == AreaKind::System).collect();
    assert_eq!(
        system_rows.len(),
        1,
        "exactly one kind='system' row after the race: {system_rows:?}"
    );
}

/// Issue #175 — contract test. `POST /api/areas` accepts a JSON body
/// shaped by `CreateAreaRequest` (whose required identity fields remain
/// `name` and `color`). `serde`'s default
/// behavior is to silently drop unknown fields, so a payload that
/// includes `"kind": "system"` parses cleanly and the row still lands
/// as `AreaKind::User` (because `area_create_tx` hardcodes `User`).
/// This test pins the silent-drop behavior so a future patch that adds
/// a `kind` field to `CreateAreaRequest` — even with the best intentions — turns
/// red here before it ships. Promoting a user area to `kind='system'`
/// through the public surface would let any client claim the singleton
/// system slot and break the invariants of the hidden Today scaffolding.
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

    // Belt + braces: the DB row itself carries `AreaKind::User`, and the
    // system-area slot is still empty (no client-payload-controlled
    // promotion happened).
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

/// Followup to issue #175 — `DELETE /api/areas/{id}` must refuse to
/// remove the singleton system area. The handler-level guard returns 403
/// (`CalmError::Forbidden`) before reaching `area_delete_tx`, so the
/// underlying primitive stays a low-level no-policy helper (it's also
/// reachable from server-internal sites like replay fixtures) and the
/// policy lives at the public boundary.
///
/// We assert:
///   * a 403 status,
///   * the system row is still present in the DB after the rejected call,
///   * for contrast, deleting a user area minted via the public POST
///     surface still succeeds (204) — the guard is targeted, not blanket.
#[tokio::test]
async fn delete_system_area_via_rest_is_forbidden() {
    let (app, repo) = boot().await;

    // Mint the system area via the public upsert endpoint.
    let (status, body) = post_empty(app.clone(), "/api/areas/system").await;
    assert_eq!(status, StatusCode::CREATED, "mint system area: {body:?}");
    let system_id = body["id"].as_str().expect("system area id").to_string();

    // Attempt to delete it via the public DELETE surface — must be 403.
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

    // DB invariant: the system row is still there. The handler short-
    // circuited before opening a write txn, so this is a strong check.
    let still_there = repo
        .area_get_system()
        .await
        .unwrap()
        .expect("system area still present after rejected delete");
    assert_eq!(still_there.id.as_str(), system_id);
    assert_eq!(still_there.kind, AreaKind::System);

    // Belt + braces: a regular user area can still be deleted via the
    // same endpoint — the guard is targeted at `kind = 'system'`, not a
    // blanket "no deletes" regression.
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

/// Followup to issue #175 — the `area.updated` event emitted by `POST
/// /api/areas/system` must carry `actor = Kernel`, not the
/// middleware-injected `User`. The system area is kernel-owned
/// scaffolding (no human "did" the mint — the frontend just bootstraps
/// the default Today terminal); a `User` stamp here would mislead any
/// future audit pipeline that joins on `events.actor`.
///
/// We pin the on-disk shape directly via `repo.pool()` — the events
/// table is the audit source of truth, and this test mirrors the style
/// of `tests/actor.rs::missing_header_defaults_to_user_actor`.
#[tokio::test]
async fn post_areas_system_stamps_kernel_actor_in_events() {
    let (app, _repo, concrete) = boot_with_concrete().await;

    // Mint the system area (default header — no X-Calm-Actor present;
    // middleware would inject `User`, the handler must override to
    // `Kernel`).
    let (status, body) = post_empty(app, "/api/areas/system").await;
    assert_eq!(status, StatusCode::CREATED, "mint succeeded: {body:?}");

    // Read the most recent event — the mint is the only write so far.
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

/// Followup to issue #175 — even with a `X-Calm-Actor: ai:codex` header
/// the system area mint must record `Kernel` in the event log. The
/// header is validated (the middleware would still reject malformed
/// values), but its declared identity does **not** leak into the
/// system-scaffolding write. This locks down the override at the handler
/// rather than relying on the middleware-default-only path.
///
/// We use `ai:codex` rather than the bare `user` default because the
/// override is most observable when there's a non-default header for
/// it to discard — a regression that drops the hardcode and falls back
/// to `actor.to_actor_id()` would surface here as `AiCodex(<empty>)`.
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
