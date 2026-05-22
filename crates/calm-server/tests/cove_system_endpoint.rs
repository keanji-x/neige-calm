//! Integration tests for the `/api/coves` + `/api/coves/system` routes
//! that came with issue #175 (system cove kind).
//!
//! Two contracts under test:
//!
//!   1. **Race-safe upsert.** `POST /api/coves/system` is hit from every
//!      cold-boot Today-page load. Two concurrent tabs can both see
//!      `cove_get_system() == None` and both reach the mint closure; the
//!      partial unique index on `coves(kind) WHERE kind = 'system'`
//!      (migration 0009) fails the loser's INSERT. The route handler
//!      catches that DB error, re-reads, and returns 200 instead of 500.
//!      We simulate the race with `tokio::join!` and assert both calls
//!      surface a successful response and the DB ends up with exactly
//!      one system row.
//!
//!   2. **`POST /api/coves` silently ignores `kind`.** `NewCove`
//!      deliberately omits a `kind` field (and `serde` is permissive by
//!      default — `deny_unknown_fields` is *not* set), so a client
//!      payload like `{"name":"x","color":"#000","kind":"system"}` is
//!      accepted, the unknown field is dropped, and the row lands as
//!      `CoveKind::User`. This test pins that behavior so a future
//!      well-meaning patch that adds `kind` to `NewCove` lights up here
//!      before it ships — promoting a user cove to the singleton system
//!      kind through the public surface would break #175's invariants.

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use calm_server::db::prelude::*;
use calm_server::db::sqlite::SqlxRepo;
use calm_server::event::EventBus;
use calm_server::model::CoveKind;
use calm_server::plugin_host::{PluginHost, PluginRegistry};
use calm_server::routes;
use calm_server::state::{AppState, CodexClient, DaemonClient};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tower::ServiceExt;

/// Boot a minimal Axum router with an in-memory SqlxRepo. Shape mirrors
/// `payload_validation.rs::boot` — no cove/wave seeding here because the
/// tests exercise the cove endpoints themselves.
async fn boot() -> (axum::Router, Arc<dyn Repo>) {
    let repo: Arc<dyn Repo> = Arc::new(
        SqlxRepo::open("sqlite::memory:")
            .await
            .expect("open in-memory sqlite repo"),
    );
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
            calm_server::card_role_cache::CardRoleCache::new(),
        )),
        Arc::new(CodexClient::new_stub()),
        None,
    );
    let app = routes::router()
        .layer(axum::middleware::from_fn(
            calm_server::actor::actor_middleware,
        ))
        .with_state(state);
    (app, repo)
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
    // `POST /api/coves/system` takes no request body. Axum accepts an
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
async fn post_coves_system_first_call_returns_201() {
    let (app, repo) = boot().await;
    let (status, body) = post_empty(app, "/api/coves/system").await;
    assert_eq!(status, StatusCode::CREATED, "first call mints: {body:?}");
    assert_eq!(
        body["kind"], "system",
        "minted row has kind=system: {body:?}"
    );
    let row = repo
        .cove_get_system()
        .await
        .unwrap()
        .expect("system cove persisted");
    assert_eq!(row.kind, CoveKind::System);
}

#[tokio::test]
async fn post_coves_system_second_call_returns_200_existing_row() {
    let (app, _repo) = boot().await;
    let (s1, b1) = post_empty(app.clone(), "/api/coves/system").await;
    assert_eq!(s1, StatusCode::CREATED, "first call: {b1:?}");
    let id1 = b1["id"].as_str().expect("id present").to_string();

    let (s2, b2) = post_empty(app, "/api/coves/system").await;
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
/// /api/coves/system` calls can both see `cove_get_system() == None`
/// in the pre-check and race into the mint closure. The partial unique
/// index on `coves(kind) WHERE kind = 'system'` fails the loser's
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
async fn post_coves_system_concurrent_calls_both_succeed() {
    use std::sync::Arc as StdArc;
    use tokio::sync::Barrier;

    let (app, repo) = boot().await;
    let barrier = StdArc::new(Barrier::new(2));

    let app_a = app.clone();
    let barrier_a = barrier.clone();
    let handle_a = tokio::spawn(async move {
        barrier_a.wait().await;
        post_empty(app_a, "/api/coves/system").await
    });
    let app_b = app.clone();
    let barrier_b = barrier.clone();
    let handle_b = tokio::spawn(async move {
        barrier_b.wait().await;
        post_empty(app_b, "/api/coves/system").await
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
    let all = repo.coves_list().await.unwrap();
    let system_rows: Vec<_> = all.iter().filter(|c| c.kind == CoveKind::System).collect();
    assert_eq!(
        system_rows.len(),
        1,
        "exactly one kind='system' row after the race: {system_rows:?}"
    );
}

/// Issue #175 — contract test. `POST /api/coves` accepts a JSON body
/// shaped by `NewCove { name, color, sort? }`. `serde`'s default
/// behavior is to silently drop unknown fields, so a payload that
/// includes `"kind": "system"` parses cleanly and the row still lands
/// as `CoveKind::User` (because `cove_create_tx` hardcodes `User`).
/// This test pins the silent-drop behavior so a future patch that adds
/// a `kind` field to `NewCove` — even with the best intentions — turns
/// red here before it ships. Promoting a user cove to `kind='system'`
/// through the public surface would let any client claim the singleton
/// system slot and break the invariants of the hidden Today scaffolding.
#[tokio::test]
async fn post_coves_silently_drops_kind_field_lands_as_user() {
    let (app, repo) = boot().await;

    let (status, body) = post(
        app,
        "/api/coves",
        json!({ "name": "trojan", "color": "#bad", "kind": "system" }),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::CREATED,
        "POST /api/coves with unknown `kind` field still returns 201 (serde drops it): body={body:?}"
    );
    assert_eq!(
        body["kind"], "user",
        "the unknown `kind` was ignored and the row landed as User: {body:?}"
    );

    // Belt + braces: the DB row itself carries `CoveKind::User`, and the
    // system-cove slot is still empty (no client-payload-controlled
    // promotion happened).
    let id = body["id"].as_str().expect("created id");
    let row = repo
        .cove_get(id)
        .await
        .unwrap()
        .expect("created cove persisted");
    assert_eq!(row.kind, CoveKind::User);
    assert!(
        repo.cove_get_system().await.unwrap().is_none(),
        "no system row should be created by the public POST surface"
    );
}
