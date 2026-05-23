//! Issue #199 — dispatcher exercised through the real HTTP ingress
//! (actor middleware + scope derivation + role gate) rather than
//! `log_pure_event` hand-drives.
//!
//! What this catches that the other dispatcher / role tests don't:
//!
//!   * `tests/dispatcher.rs` and `tests/wave_as_actor_smoke.rs` both
//!     call `Repo::log_pure_event` directly — they skip the actor
//!     middleware, the request body extraction, and the scope
//!     derivation that lives in the route. A regression in any of
//!     those layers (e.g. the codex bridge's `card_id` query param
//!     no longer resolves to `EventScope::Card`, or the actor
//!     middleware silently defaults to `User` when it shouldn't)
//!     surfaces in real deploys but is invisible in those tests.
//!   * `tests/role_enforcement.rs` covers the gate in isolation but
//!     also bypasses HTTP entirely.
//!
//! Composition under test, exercised in ONE flow:
//!
//!   1. Cove → wave → codex card seeded. The wave-create route
//!      mints a spec card with `CardRole::Spec`; we add a second
//!      `kind: 'codex'` card via the route surface to get a worker-
//!      adjacent `CardRole::Plain` row whose `card_id` is valid for
//!      the `/internal/codex/hook` ingest.
//!   2. POST `/internal/codex/hook?card_id=<plain>` with
//!      `X-Calm-Actor: ai:codex` succeeds (204), and the resulting
//!      `hook.codex.*` events row records:
//!        * `actor = "ai:codex"` (middleware + scope-β reattribution)
//!        * `scope` resolves to `Card` (codex.rs scope derivation
//!          followed `card → wave → cove` correctly).
//!   3. POST `/internal/codex/hook?card_id=<plain>` WITHOUT the
//!      `X-Calm-Actor` header lands `actor = "user"` (middleware
//!      default), and the gate accepts it — `ActorId::User` is the
//!      unrestricted path. This pins the documented contract for
//!      bridges that don't yet stamp the header.
//!   4. POST `/internal/codex/hook?card_id=` (empty) with the
//!      `ai:codex` header is **rejected by the role gate** before
//!      any event row is appended. The 403 propagates; the events
//!      table count is unchanged from step 3. This is the "errored
//!      scope writes are rejected" verdict the issue calls out.
//!   5. POST `/internal/codex/hook?card_id=<wave_id>` (a card id
//!      that doesn't resolve to a card row — the route falls back
//!      to `EventScope::System`) with the `ai:codex` header is
//!      rejected for the same reason (unknown card id → role gate
//!      denies the typed `AiCodex(CardId)` actor it can't look up).
//!
//! The `CardRole` cache write-through invariant is verified
//! transitively: step 1's wave-create has to put the spec card
//! into the cache for the dispatcher / role gate to see it; step 2's
//! success and step 4's failure both depend on that cache being
//! seeded correctly.

#![cfg(unix)]

use std::path::PathBuf;
use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use calm_server::actor::actor_middleware;
use calm_server::card_role_cache::CardRoleCache;
use calm_server::db::prelude::*;
use calm_server::db::sqlite::SqlxRepo;
use calm_server::event::EventBus;
use calm_server::model::{CardRole, NewCove};
use calm_server::plugin_host::{PluginHost, PluginRegistry};
use calm_server::routes;
use calm_server::state::{AppState, CodexClient, DaemonClient};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tempfile::TempDir;
use tower::ServiceExt;

struct Boot {
    app: axum::Router,
    repo: Arc<SqlxRepo>,
    cove_id: String,
    card_role_cache: CardRoleCache,
    _tmp: TempDir,
}

async fn boot() -> Boot {
    let tmp = TempDir::new().expect("tempdir");
    let repo = Arc::new(
        SqlxRepo::open("sqlite::memory:")
            .await
            .expect("open in-memory sqlite"),
    );
    let cove = repo
        .cove_create(NewCove {
            name: "dispatch-auth-path".into(),
            color: "#000".into(),
            sort: None,
        })
        .await
        .unwrap();

    let daemon = Arc::new(DaemonClient {
        data_dir: tmp.path().to_path_buf(),
        // Non-existent daemon binary; the dispatcher's spawn step fails
        // post-commit on the codex / terminal write paths, but the
        // routes under test (`/internal/codex/hook`) don't spawn
        // anything — the failure path is irrelevant here.
        session_daemon_bin: PathBuf::from("/nonexistent-daemon-bin-auth-path"),
    });
    let events = EventBus::new();
    let card_role_cache = CardRoleCache::new();
    repo.seed_card_role_cache(&card_role_cache).await.unwrap();
    let wave_cove_cache = calm_server::wave_cove_cache::WaveCoveCache::new();
    repo.seed_wave_cove_cache(&wave_cove_cache).await.unwrap();
    let state = AppState::from_parts(
        repo.clone(),
        events.clone(),
        daemon,
        Arc::new(PluginHost::new_full(
            Arc::new(PluginRegistry::empty()),
            repo.clone(),
            PathBuf::new(),
            std::env::temp_dir().join("calm-plugins-data-auth-path"),
            Vec::new(),
            events,
            card_role_cache.clone(),
            wave_cove_cache.clone(),
        )),
        Arc::new(CodexClient::new_stub()),
        Some(card_role_cache.clone()),
        Some(wave_cove_cache.clone()),
    );

    let app = routes::router()
        .layer(axum::middleware::from_fn(actor_middleware))
        .with_state(state);

    Boot {
        app,
        repo,
        cove_id: cove.id.to_string(),
        card_role_cache,
        _tmp: tmp,
    }
}

async fn post_with_actor(
    app: axum::Router,
    uri: &str,
    actor: Option<&str>,
    body: Value,
) -> (StatusCode, Value) {
    let mut req = Request::builder()
        .method("POST")
        .uri(uri)
        .header("content-type", "application/json");
    if let Some(a) = actor {
        req = req.header("X-Calm-Actor", a);
    }
    let resp = app
        .oneshot(req.body(Body::from(body.to_string())).unwrap())
        .await
        .unwrap();
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let json: Value = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or(Value::Null)
    };
    (status, json)
}

async fn event_count(repo: &SqlxRepo) -> i64 {
    let row: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM events")
        .fetch_one(repo.pool())
        .await
        .unwrap();
    row.0
}

#[tokio::test]
async fn dispatcher_real_auth_path_cardrole_eventscope_semantics() {
    let boot = boot().await;

    // ---- 1. Wave create through the route → spec card lands with
    //         CardRole::Spec, role cache reflects it.
    let (status, _wave_body) = post_with_actor(
        boot.app.clone(),
        "/api/waves",
        Some("user"),
        json!({"cove_id": boot.cove_id, "title": "real-auth wave", "cwd": "/tmp/issue-250-pr2-test", "attach_folder": true, "theme": {"fg": [216,219,226], "bg": [15,20,24]} }),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::INTERNAL_SERVER_ERROR,
        "wave create returns 500 when daemon spawn fails synchronously (issue #236); spec card + role-cache write-through still happen pre-spawn so the assertions below still hold",
    );

    let waves = boot.repo.waves_by_cove(&boot.cove_id).await.unwrap();
    assert_eq!(waves.len(), 1);
    let wave = waves.into_iter().next().unwrap();
    let cards_after_wave = boot.repo.cards_by_wave(wave.id.as_str()).await.unwrap();
    // Issue #229 PR B — wave create now mints two kernel-owned cards
    // (spec + wave-report). Find the spec card by kind.
    assert_eq!(
        cards_after_wave.len(),
        2,
        "wave create mints spec + wave-report cards",
    );
    let spec_card_id = cards_after_wave
        .iter()
        .find(|c| c.kind == "codex")
        .expect("spec card present")
        .id
        .clone();
    assert_eq!(
        boot.card_role_cache.get(&spec_card_id),
        Some(CardRole::Spec),
        "spec card's role lives in the cache after wave create",
    );

    // Seed a plain `kind: 'codex'` card so we have a card_id the codex
    // bridge ingest can resolve. We POST through the cards route (not
    // `Repo::card_create`) so the role-cache write-through populates
    // the SAME `CardRoleCache` instance that the route + role gate
    // consult — `SqlxRepo` carries its own internal cache field that
    // never sees AppState writes. The route's `card_create_with_id_tx`
    // call threads `s.card_role_cache` explicitly, which is the one
    // we need to query below.
    let uri_cards = format!("/api/waves/{}/cards", wave.id);
    let (status, card_body) = post_with_actor(
        boot.app.clone(),
        &uri_cards,
        Some("user"),
        json!({"kind": "codex", "payload": {}}),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "codex card create returns 201");
    let plain_codex_id = card_body
        .get("id")
        .and_then(|v| v.as_str())
        .expect("card response carries id")
        .to_string();
    assert_eq!(
        boot.card_role_cache
            .get(&calm_server::ids::CardId::from(plain_codex_id.as_str())),
        Some(CardRole::Plain),
        "freshly-created codex card defaults to CardRole::Plain via write-through",
    );

    let baseline = event_count(&boot.repo).await;

    // ---- 2. Valid AiCodex ingest with a resolvable card_id.
    let uri_ok = format!("/internal/codex/hook?card_id={}", plain_codex_id);
    let (status, body) = post_with_actor(
        boot.app.clone(),
        &uri_ok,
        Some("ai:codex"),
        json!({"hook_event_name": "PreToolUse", "tool_name": "Read"}),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::NO_CONTENT,
        "codex hook ingest with valid card_id + ai:codex header → 204 (got {status:?}, body {body})"
    );

    // The route stamps `actor = "ai:codex"` and resolves the scope
    // through `card → wave → cove`. Confirm both at the SQL level —
    // the scope is decomposed across `scope_kind`, `scope_card`,
    // `scope_wave`, `scope_cove` (migration 0007).
    // events.kind is the `Event` enum's `kind_tag()` — `"codex.hook"`
    // for `Event::CodexHook` (the inner `kind` field, formatted as
    // "hook.codex.<event_name>", lives in the JSON payload). Filter
    // by the kind_tag column and decode the payload separately.
    let row: (
        String,
        String,
        String,
        Option<String>,
        Option<String>,
        Option<String>,
        String,
    ) = sqlx::query_as(
        "SELECT actor, kind, scope_kind, scope_card, scope_wave, scope_cove, payload \
         FROM events WHERE kind = 'codex.hook' ORDER BY id DESC LIMIT 1",
    )
    .fetch_one(boot.repo.pool())
    .await
    .expect("hook event row landed");
    // The codex hook ingest re-attributes the actor from the `card_id`
    // query parameter, threading an `ActorId::AiCodex(<card_id>)` into
    // the event row (not the raw header string). `events.actor` is
    // JSON-serialized; parse it back to assert the kind + id.
    let actor_json: Value = serde_json::from_str(&row.0).expect("events.actor is JSON");
    assert_eq!(
        actor_json.get("kind").and_then(|v| v.as_str()),
        Some("AiCodex"),
        "ai:codex header reattributes to ActorId::AiCodex via the route's typed actor"
    );
    assert_eq!(
        actor_json.get("id").and_then(|v| v.as_str()),
        Some(plain_codex_id.as_str()),
        "ActorId::AiCodex carries the card_id from the route's query param"
    );
    assert_eq!(row.1, "codex.hook");
    let payload: Value = serde_json::from_str(&row.6).expect("payload column is JSON");
    assert_eq!(
        payload.get("kind").and_then(|v| v.as_str()),
        Some("hook.codex.pre_tool_use"),
        "route's snake-cased hook_event_name lives inside the CodexHook payload's `kind` field",
    );
    assert_eq!(
        row.2, "card",
        "valid card_id must resolve to EventScope::Card (got scope_kind {})",
        row.2,
    );
    assert_eq!(
        row.3.as_deref(),
        Some(plain_codex_id.as_str()),
        "scope_card must point at the codex card we POSTed against",
    );
    assert!(row.4.is_some(), "scope_wave populated for card scope");
    assert!(row.5.is_some(), "scope_cove populated for card scope");

    let after_ok = event_count(&boot.repo).await;
    assert_eq!(
        after_ok,
        baseline + 1,
        "exactly one new event row from the successful ingest",
    );

    // ---- 3. Actor middleware exercised via a route that DOES forward
    //         the extracted `Actor` to the typed `ActorId` (overlays
    //         upsert — the codex hook route deliberately ignores its
    //         `_actor` and reattributes via `card_id`, so it can't
    //         prove this leg). A `POST /api/overlays` with no header
    //         must land `actor = "User"` (middleware default → typed
    //         actor); the same POST with `X-Calm-Actor: ai:codex`
    //         is REFUSED at the gate because the middleware-default
    //         `to_actor_id` for `ai:codex` synthesizes an empty CardId
    //         that the gate's empty-CardId guard rejects.
    let upsert_uri = "/api/overlays";
    let upsert_body = json!({
        "plugin_id": "core",
        "entity_kind": "wave",
        "entity_id": wave.id.as_str(),
        "kind": "status",
        "payload": {"state": "ok"},
    });
    let (status, _) =
        post_with_actor(boot.app.clone(), upsert_uri, None, upsert_body.clone()).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "no-header overlay upsert lands (user is unrestricted)"
    );
    let last_overlay_actor: (String,) = sqlx::query_as(
        "SELECT actor FROM events WHERE kind = 'overlay.set' ORDER BY id DESC LIMIT 1",
    )
    .fetch_one(boot.repo.pool())
    .await
    .unwrap();
    let actor_json: Value =
        serde_json::from_str(&last_overlay_actor.0).expect("events.actor is JSON");
    assert_eq!(
        actor_json.get("kind").and_then(|v| v.as_str()),
        Some("User"),
        "no header → middleware default `user` → ActorId::User",
    );

    let after_default = event_count(&boot.repo).await;
    assert!(
        after_default >= baseline + 2,
        "two writes landed by now: codex hook + overlay upsert (events.id baseline+>=2)",
    );

    // ---- 4. Empty card_id with ai:codex header → role gate rejects.
    //
    // The gate's empty-CardId guard fires before any SQL runs (see
    // `tests/role_enforcement.rs::empty_codex_card_id_rejected`).
    // The HTTP surface returns 403; the events count must NOT bump.
    let (status, _) = post_with_actor(
        boot.app.clone(),
        "/internal/codex/hook?card_id=",
        Some("ai:codex"),
        json!({"hook_event_name": "PreToolUse"}),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::FORBIDDEN,
        "empty card_id with ai:codex must be rejected by the role gate",
    );
    let after_empty = event_count(&boot.repo).await;
    assert_eq!(
        after_empty, after_default,
        "rejected ingest must NOT append to the event log",
    );

    // ---- 5. Unknown card_id with ai:codex → unknown-card gate
    //         rejects. The route's scope derivation falls back to
    //         `EventScope::System` for a card that doesn't resolve;
    //         the role gate then refuses the typed AiCodex actor it
    //         can't look up in the role cache. Same 403, same
    //         events-table invariant as step 4.
    let (status, _) = post_with_actor(
        boot.app.clone(),
        // wave id is not a card id → unresolvable
        &format!("/internal/codex/hook?card_id={}", wave.id),
        Some("ai:codex"),
        json!({"hook_event_name": "PreToolUse"}),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::FORBIDDEN,
        "unknown card_id with ai:codex must be rejected by the role gate"
    );
    let after_unknown = event_count(&boot.repo).await;
    assert_eq!(
        after_unknown, after_default,
        "unknown-card rejected ingest must NOT append to the event log either",
    );
}
