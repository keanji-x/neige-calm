//! Dispatcher exercised through the real HTTP ingress (actor middleware + scope derivation +
//! role gate) rather than `log_pure_event` hand-drives.

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
use calm_server::model::{CardRole, NewArea};
use calm_server::plugin_host::{PluginHost, PluginRegistry};
use calm_server::routes;
use calm_server::state::{AppState, CodexClient, DaemonClient};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tempfile::TempDir;
use tower::ServiceExt;

use crate::support::git_helpers::attached_repo_fixture;

struct Boot {
    app: axum::Router,
    repo: Arc<SqlxRepo>,
    area_id: String,
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
    let area = repo
        .area_create(NewArea {
            name: "dispatch-auth-path".into(),
            color: "#000".into(),
            sort: None,
        })
        .await
        .unwrap();

    let daemon = Arc::new(DaemonClient {
        data_dir: tmp.path().to_path_buf(),
        // Non-existent daemon binary; the routes under test spawn nothing.
        proc_supervisor_sock: None,
    });
    let events = EventBus::new();
    let card_role_cache = CardRoleCache::new();
    repo.seed_card_role_cache(&card_role_cache).await.unwrap();
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
            std::env::temp_dir().join("calm-plugins-data-auth-path"),
            Vec::new(),
            events,
            calm_server::state::WriteContext::new(
                card_role_cache.clone(),
                track_area_cache.clone(),
            ),
        )),
        {
            // Deterministically-broken codex bin so the planner-push app-server boot fails fast; track
            // create tolerates this and returns 201.
            let mut codex = CodexClient::new_stub();
            codex.codex_bin = "/nonexistent-codex-bin-dispatcher-real-auth".into();
            Arc::new(codex)
        },
        Some(card_role_cache.clone()),
        Some(track_area_cache.clone()),
    );

    let app = routes::router()
        .layer(axum::middleware::from_fn(actor_middleware))
        .with_state(state);

    Boot {
        app,
        repo,
        area_id: area.id.to_string(),
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

    // 1. Track create through the route → planner card lands with CardRole::Planner.
    let (status, _track_body) = post_with_actor(
        boot.app.clone(),
        "/api/tracks",
        Some("user"),
        json!({"area_id": boot.area_id, "title": "real-auth track", "cwd": attached_repo_fixture("issue-250-pr2-test"), "attach_folder": true, "theme": {"fg": [216,219,226], "bg": [15,20,24]} }),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::CREATED,
        "track create returns 201 even when the planner app-server boot fails (issue #293 / PR #311 — boot is non-fatal); planner card + role-cache write-through still happen pre-boot so the assertions below still hold",
    );

    let tracks = boot.repo.tracks_by_area(&boot.area_id).await.unwrap();
    assert_eq!(tracks.len(), 1);
    let track = tracks.into_iter().next().unwrap();
    let cards_after_track = boot.repo.cards_by_track(track.id.as_str()).await.unwrap();
    // Track create mints two kernel-owned cards (planner + track-report); find the planner by kind.
    assert_eq!(
        cards_after_track.len(),
        2,
        "track create mints planner + track-report cards",
    );
    let planner_card_id = cards_after_track
        .iter()
        .find(|c| c.kind == "codex")
        .expect("planner card present")
        .id
        .clone();
    assert_eq!(
        boot.card_role_cache.get(&planner_card_id),
        Some(CardRole::Planner),
        "planner card's role lives in the cache after track create",
    );

    // POST through the cards route (not `Repo::card_create`) so the role-cache write-through
    // populates the SAME `CardRoleCache` the route + role gate consult; `SqlxRepo` carries its
    // own internal cache that never sees AppState writes.
    let uri_cards = format!("/api/tracks/{}/cards", track.id);
    let (status, card_body) = post_with_actor(
        boot.app.clone(),
        &uri_cards,
        Some("user"),
        json!({"kind": "codex", "payload": {}}),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "codex card create returns 201");
    let worker_codex_id = card_body
        .get("id")
        .and_then(|v| v.as_str())
        .expect("card response carries id")
        .to_string();
    assert_eq!(
        boot.card_role_cache
            .get(&calm_server::ids::CardId::from(worker_codex_id.as_str())),
        Some(CardRole::Worker),
        "freshly-created codex card defaults to CardRole::Worker via write-through",
    );

    let baseline = event_count(&boot.repo).await;

    // 2. Valid AiCodex ingest with a resolvable card_id.
    let uri_ok = format!("/internal/codex/hook?card_id={}", worker_codex_id);
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

    // events.kind is the `Event` enum's `kind_tag()` (`"codex.hook"`); the inner `kind` field lives
    // in the JSON payload.
    let row: (
        String,
        String,
        String,
        Option<String>,
        Option<String>,
        Option<String>,
        String,
    ) = sqlx::query_as(
        "SELECT actor, kind, scope_kind, scope_card, scope_track, scope_area, payload \
         FROM events WHERE kind = 'codex.hook' ORDER BY id DESC LIMIT 1",
    )
    .fetch_one(boot.repo.pool())
    .await
    .expect("hook event row landed");
    // The hook ingest re-attributes the actor from the `card_id` query parameter; `events.actor` is JSON.
    let actor_json: Value = serde_json::from_str(&row.0).expect("events.actor is JSON");
    assert_eq!(
        actor_json.get("kind").and_then(|v| v.as_str()),
        Some("AiCodex"),
        "ai:codex header reattributes to ActorId::AiCodex via the route's typed actor"
    );
    assert_eq!(
        actor_json.get("id").and_then(|v| v.as_str()),
        Some(worker_codex_id.as_str()),
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
        Some(worker_codex_id.as_str()),
        "scope_card must point at the codex card we POSTed against",
    );
    assert!(row.4.is_some(), "scope_track populated for card scope");
    assert!(row.5.is_some(), "scope_area populated for card scope");

    let after_ok = event_count(&boot.repo).await;
    assert_eq!(
        after_ok,
        baseline + 1,
        "exactly one new event row from the successful ingest",
    );

    // 3. A route that DOES forward the extracted `Actor` (overlays upsert; the codex hook route
    // reattributes via `card_id`): no header lands `actor = "User"`, while `ai:codex` is refused
    // because the middleware-default `to_actor_id` synthesizes an empty CardId the gate rejects.
    let upsert_uri = "/api/overlays";
    let upsert_body = json!({
        "plugin_id": "core",
        "entity_kind": "track",
        "entity_id": track.id.as_str(),
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

    // 4. Empty card_id with ai:codex header → the gate's empty-CardId guard fires before any SQL;
    // 403 and the events count must NOT bump.
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

    // 5. Unknown card_id with ai:codex → scope falls back to `EventScope::System` and the gate
    // refuses the AiCodex actor it cannot look up.
    let (status, _) = post_with_actor(
        boot.app.clone(),
        // track id is not a card id → unresolvable
        &format!("/internal/codex/hook?card_id={}", track.id),
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
