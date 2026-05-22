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
use calm_server::event::{Event, EventBus, EventScope};
use calm_server::ids::ActorId;
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
        events.clone(),
        Arc::new(DaemonClient::new_stub()),
        Arc::new(PluginHost::new_full(
            Arc::new(PluginRegistry::empty()),
            repo,
            std::path::PathBuf::new(),
            std::env::temp_dir().join("calm-plugins-data"),
            Vec::new(),
            events,
            calm_server::card_role_cache::CardRoleCache::new(),
        )),
        Arc::new(CodexClient::new_stub()),
        None,
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

/// PR2 of #136 typed the actor field. `events.actor` now stores the
/// JSON form of [`ActorId`]; the route's `Actor::to_actor_id()` maps
/// the header string back onto the typed enum. The middleware's
/// validation surface is unchanged — only the on-disk shape moved.
fn parse_actor_json(s: &str) -> serde_json::Value {
    serde_json::from_str(s).expect("events.actor is JSON-serialized ActorId")
}

#[tokio::test]
async fn missing_header_defaults_to_user_actor() {
    let (app, repo, _state) = boot().await;
    let (status, actor) = post_cove_and_read_actor(app, &repo, None).await;
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(
        parse_actor_json(actor.as_deref().unwrap()),
        serde_json::json!({"kind": "User"})
    );
}

// ---------------------------------------------------------------------------
// 2. Valid AI header → mapped onto AiCodex; non-`codex` forms collapse to User.
// ---------------------------------------------------------------------------
//
// PR2 of #136 — the route-level `Actor::to_actor_id()` mapping only
// covers `"user"` → `User` and `"ai:codex"` → `AiCodex(<empty>)` today
// (the only two header forms a production deploy actually emits; the
// codex bridge stamps `ai:codex`, every other caller uses no header).
// Any other `ai:<id>` form falls through to the defensive `User`
// fallback — PR3+ will refine this once typed actors are wired into the
// dispatcher.

#[tokio::test]
async fn ai_codex_header_rejected_without_card_context() {
    // PR3 (#136) — `ai:codex` on the REST surface maps to
    // `ActorId::AiCodex(CardId(""))` (no card context at the actor-
    // extraction point); the `enforce_role` gate refuses the write
    // outright via its empty-CardId guard. This used to land in the
    // events table with a placeholder empty CardId in PR2; PR3
    // tightens the gate so an AI write without a real card identity
    // is impossible.
    //
    // The codex bridge ingest path (`routes::codex::ingest_hook`) is
    // unaffected — it now resolves the real card id from its query
    // param before stamping the actor (see PR3 reattribution in that
    // file). Other production callers of `ai:codex` on REST don't
    // exist today; the gate makes that an enforced invariant.
    let (app, _repo, _state) = boot().await;
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/coves")
                .header("content-type", "application/json")
                .header("X-Calm-Actor", "ai:codex")
                .body(Body::from(
                    serde_json::json!({ "name": "c", "color": "#000" }).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn valid_ai_actor_with_dashes_recorded() {
    let (app, repo, _state) = boot().await;
    let (status, actor) = post_cove_and_read_actor(app, &repo, Some("ai:claude-3-5")).await;
    assert_eq!(status, StatusCode::CREATED);
    // Non-`codex` AI ids collapse to the defensive `User` fallback in
    // PR2. Documented in `Actor::to_actor_id` — PR3+ may refine.
    assert_eq!(
        parse_actor_json(actor.as_deref().unwrap()),
        serde_json::json!({"kind": "User"})
    );
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
            theme: None,
        })
        .await
        .unwrap();

    // Exactly what `plugin_host::callbacks::overlay_set` does after the
    // perm check. PR2 of #136 typed the actor; PR3 (#136) added the
    // role cache as another required arg:
    //   let actor = ActorId::Plugin(ctx.plugin_id.to_string());
    //   write_with_event_typed(repo, actor, scope, None, &bus, &cache, |tx| { ... })
    let plugin_id = "hello-world";
    let actor = ActorId::Plugin(plugin_id.to_string());
    let new_overlay = NewOverlay {
        plugin_id: plugin_id.to_string(),
        entity_kind: "wave".into(),
        entity_id: wave.id.to_string(),
        kind: "status".into(),
        payload: serde_json::json!({ "state": "Idle" }),
    };
    let (overlay, event_id) = write_with_event_typed(
        state.repo.as_ref(),
        actor,
        EventScope::Wave {
            wave: wave.id.clone(),
            cove: cove.id.clone(),
        },
        None,
        &state.events,
        &calm_server::card_role_cache::CardRoleCache::new(),
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
    // PR2 of #136: events.actor stores the typed JSON form.
    let actor_json: serde_json::Value = serde_json::from_str(&row.1).unwrap();
    assert_eq!(
        actor_json,
        serde_json::json!({"kind": "Plugin", "id": "hello-world"}),
        "plugin-callback path must stamp Plugin(<id>) even when REST middleware would reject it"
    );
}

// ---------------------------------------------------------------------------
// 7. PR2 of #136 end-to-end: POST /api/cards stamps the full scope chain.
// ---------------------------------------------------------------------------
//
// Drive the REST surface end-to-end and assert the resulting `events` row
// carries `scope_kind = 'card'` plus the full `scope_card` / `scope_wave`
// / `scope_cove` ancestor chain. This is the spot-check the issue brief
// calls out — a single test that exercises the whole pipeline (route →
// `card_scope` helper → `write_with_event_typed` → `event_append_in_tx`
// → SQL bind) instead of unit-testing the layers in isolation.

#[tokio::test]
async fn create_card_stamps_full_scope_chain() {
    let (app, repo, _state) = boot().await;

    // Seed a cove + wave so the card has somewhere to live.
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
            theme: None,
        })
        .await
        .unwrap();

    let body = serde_json::json!({
        "kind": "plugin:test:demo",
        "payload": {}
    })
    .to_string();
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/api/waves/{}/cards", wave.id))
                .header("content-type", "application/json")
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED);
    let resp_body = to_bytes(resp.into_body(), 8192).await.unwrap();
    let card_json: serde_json::Value = serde_json::from_slice(&resp_body).unwrap();
    let card_id = card_json["id"].as_str().expect("card id").to_string();

    // The most recent event row is the one we just produced. Read every
    // scope_* column and assert the full chain is populated.
    let row: (String, Option<String>, Option<String>, Option<String>) = sqlx::query_as(
        "SELECT scope_kind, scope_cove, scope_wave, scope_card
         FROM events ORDER BY id DESC LIMIT 1",
    )
    .fetch_one(repo.pool())
    .await
    .unwrap();
    assert_eq!(row.0, "card", "scope_kind == 'card' for card.added");
    assert_eq!(
        row.1.as_deref(),
        Some(cove.id.as_str()),
        "scope_cove populated"
    );
    assert_eq!(
        row.2.as_deref(),
        Some(wave.id.as_str()),
        "scope_wave populated"
    );
    assert_eq!(
        row.3.as_deref(),
        Some(card_id.as_str()),
        "scope_card populated with the freshly-minted card id"
    );
}
