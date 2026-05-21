//! Scope G — `X-Calm-Actor` middleware tests.
//!
//! Asserts the declarative-actor wiring:
//!
//!   1. Missing header defaults to `"user"`.
//!   2. Valid `ai:<id>` is recorded verbatim.
//!   3. Reserved `kernel` is rejected from header with 400.
//!   4. Reserved `plugin:<id>` is rejected from header with 400.
//!   5. Malformed forms (`ai:`, `ai:UPPER`) are rejected with 400.
//!   6. The plugin-callback write path keeps stamping `"plugin:<id>"`
//!      regardless of any header — REST middleware and callback dispatcher
//!      are separate code paths.
//!
//! Harness shape mirrors `tests/codex_ingest.rs`: an in-memory `SqlxRepo`, a
//! stub `DaemonClient`/`CodexClient`/`PluginHost`, and the REST router
//! wrapped with the actor middleware. We assert against `events.actor`
//! directly so the recorded value is what we care about, not the response
//! body shape.

use std::sync::Arc;

use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use calm_server::actor::actor_middleware;
use calm_server::db::prelude::*;
use calm_server::db::sqlite::{SqlxRepo, overlay_upsert_tx};
use calm_server::db::write_with_event_typed;
use calm_server::event::{Event, EventBus};
use calm_server::model::{NewCove, NewOverlay, NewWave};
use calm_server::plugin_host::{PluginHost, PluginRegistry};
use calm_server::routes;
use calm_server::state::{AppState, CodexClient, DaemonClient};
use tower::ServiceExt;

/// Build an `AppState` plus a router with the actor middleware wired in,
/// matching `main.rs`. Returned repo is the concrete `SqlxRepo` so tests can
/// query the events table directly.
async fn boot() -> (axum::Router, Arc<SqlxRepo>, AppState) {
    let concrete = Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap());
    let repo: Arc<dyn Repo> = concrete.clone();
    let events = EventBus::new();
    let state = AppState::from_parts(
        repo.clone(),
        events,
        Arc::new(DaemonClient::new_stub()),
        Arc::new(PluginHost::new(Arc::new(PluginRegistry::empty()), repo)),
        Arc::new(CodexClient::new_stub()),
    );
    // Same shape as main.rs: middleware sits on the REST router only.
    let app = axum::Router::new()
        .merge(routes::router())
        .layer(axum::middleware::from_fn(actor_middleware))
        .with_state(state.clone());
    (app, concrete, state)
}

/// Drive a `POST /api/coves` and return the recorded actor for the event
/// the write produced. Header value is passed verbatim when `header` is
/// `Some(...)`. Returns the response status and (if 2xx) the actor string.
async fn post_cove_and_read_actor(
    app: axum::Router,
    repo: &SqlxRepo,
    header: Option<&str>,
) -> (StatusCode, Option<String>) {
    let mut req = Request::builder()
        .method("POST")
        .uri("/api/coves")
        .header("content-type", "application/json");
    if let Some(h) = header {
        req = req.header("X-Calm-Actor", h);
    }
    let body = serde_json::json!({ "name": "c", "color": "#000" }).to_string();
    let resp = app
        .oneshot(req.body(Body::from(body)).unwrap())
        .await
        .unwrap();
    let status = resp.status();
    if !status.is_success() {
        return (status, None);
    }

    // The most recent event row is the cove we just created. We could
    // parse the response body for the id and round-trip through events,
    // but the events table is monotonic and we just created a row — read
    // the latest.
    let row: (String, String) =
        sqlx::query_as("SELECT kind, actor FROM events ORDER BY id DESC LIMIT 1")
            .fetch_one(repo.pool())
            .await
            .unwrap();
    assert_eq!(
        row.0, "cove.updated",
        "expected cove.updated, got {}",
        row.0
    );
    (status, Some(row.1))
}

// ---------------------------------------------------------------------------
// 1. No header → actor recorded as "user".
// ---------------------------------------------------------------------------

#[tokio::test]
async fn missing_header_defaults_to_user_actor() {
    let (app, repo, _state) = boot().await;
    let (status, actor) = post_cove_and_read_actor(app, &repo, None).await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(actor.as_deref(), Some("user"));
}

// ---------------------------------------------------------------------------
// 2. Valid AI header → recorded verbatim.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn valid_ai_actor_recorded() {
    let (app, repo, _state) = boot().await;
    let (status, actor) = post_cove_and_read_actor(app, &repo, Some("ai:codex")).await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(actor.as_deref(), Some("ai:codex"));
}

#[tokio::test]
async fn valid_ai_actor_with_dashes_recorded() {
    let (app, repo, _state) = boot().await;
    let (status, actor) = post_cove_and_read_actor(app, &repo, Some("ai:claude-3-5")).await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(actor.as_deref(), Some("ai:claude-3-5"));
}

// ---------------------------------------------------------------------------
// 3. Reserved `kernel` rejected from header.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn kernel_actor_rejected_from_header() {
    let (app, repo, _state) = boot().await;
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/coves")
                .header("content-type", "application/json")
                .header("X-Calm-Actor", "kernel")
                .body(Body::from(
                    serde_json::json!({ "name": "c", "color": "#000" }).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);

    // And the rejection happens before the handler — so no event row was
    // written.
    let row: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM events")
        .fetch_one(repo.pool())
        .await
        .unwrap();
    assert_eq!(row.0, 0, "rejected header must not produce an event row");
}

// ---------------------------------------------------------------------------
// 4. Reserved `plugin:<id>` rejected from header.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn plugin_actor_rejected_from_header() {
    let (app, repo, _state) = boot().await;
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/coves")
                .header("content-type", "application/json")
                .header("X-Calm-Actor", "plugin:hello-world")
                .body(Body::from(
                    serde_json::json!({ "name": "c", "color": "#000" }).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    let row: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM events")
        .fetch_one(repo.pool())
        .await
        .unwrap();
    assert_eq!(row.0, 0);
}

// ---------------------------------------------------------------------------
// 5. Malformed `ai:<id>` forms rejected.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn empty_ai_id_rejected() {
    let (app, _repo, _state) = boot().await;
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/coves")
                .header("content-type", "application/json")
                .header("X-Calm-Actor", "ai:")
                .body(Body::from(
                    serde_json::json!({ "name": "c", "color": "#000" }).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);

    // Body shape carries the `bad_request` code so frontends can branch.
    let body = to_bytes(resp.into_body(), 1024).await.unwrap();
    let v: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(v["code"], "bad_request");
}

#[tokio::test]
async fn uppercase_ai_id_rejected() {
    let (app, _repo, _state) = boot().await;
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/coves")
                .header("content-type", "application/json")
                .header("X-Calm-Actor", "ai:UPPER")
                .body(Body::from(
                    serde_json::json!({ "name": "c", "color": "#000" }).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
}

// ---------------------------------------------------------------------------
// 6. Plugin callback path unchanged.
// ---------------------------------------------------------------------------
//
// The callback dispatcher in `plugin_host/callbacks.rs` calls
// `write_with_event_typed` with `actor = format!("plugin:{plugin_id}")`
// directly — it does NOT go through the REST middleware. This test exercises
// the identical write the dispatcher performs (an overlay upsert), with the
// plugin actor format, on a server whose REST router has the actor
// middleware wired in. The middleware would have rejected `plugin:*` from
// the header (test #4 covers that); here we assert the server-internal
// path still produces a `plugin:<id>` event row regardless.
//
// This is a deliberately narrow assertion. The full callback round-trip is
// already covered by `tests/plugin_host_callbacks.rs`. Scope G only needs to
// prove that wiring middleware on the REST path didn't accidentally also
// constrain the plugin-callback path.

#[tokio::test]
async fn plugin_callback_path_writes_plugin_actor_regardless_of_middleware() {
    let (_app, repo, state) = boot().await;

    // Seed a wave so the overlay upsert has a real entity to target.
    let cove = repo
        .cove_create(NewCove {
            name: "c".into(),
            color: "#000".into(),
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

    // Exactly what `plugin_host::callbacks::overlay_set` does after the
    // perm check — the production code path is:
    //   let actor = format!("plugin:{}", ctx.plugin_id);
    //   write_with_event_typed(repo, &actor, None, &bus, |tx| { ... })
    let plugin_id = "hello-world";
    let actor = format!("plugin:{plugin_id}");
    let new_overlay = NewOverlay {
        plugin_id: plugin_id.to_string(),
        entity_kind: "wave".into(),
        entity_id: wave.id.clone(),
        kind: "status".into(),
        payload: serde_json::json!({ "state": "Idle" }),
    };
    let (overlay, event_id) = write_with_event_typed(
        state.repo.as_ref(),
        &actor,
        None,
        &state.events,
        move |tx| {
            Box::pin(async move {
                let o = overlay_upsert_tx(tx, new_overlay).await?;
                Ok((o.clone(), Event::OverlaySet(o)))
            })
        },
    )
    .await
    .expect("plugin overlay write");
    assert_eq!(overlay.plugin_id, plugin_id);

    let row: (String, String) = sqlx::query_as("SELECT kind, actor FROM events WHERE id = ?1")
        .bind(event_id)
        .fetch_one(repo.pool())
        .await
        .unwrap();
    assert_eq!(row.0, "overlay.set");
    assert_eq!(
        row.1, "plugin:hello-world",
        "plugin-callback path must stamp `plugin:<id>` even when REST middleware would reject it"
    );
}
