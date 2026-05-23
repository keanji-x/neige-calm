//! Scope β — end-to-end wiring for `X-Calm-Actor` + plugin-tool-call
//! correlation.
//!
//! Three assertions, each anchored on a real row in the `events` table:
//!
//!   1. The codex bridge's `X-Calm-Actor: ai:codex` header lands in
//!      `events.actor` for the resulting `codex.hook` row. This is the
//!      "AI write attribution" guarantee — pre-β the column read `kernel`
//!      regardless of who wrote.
//!   2. A `POST /api/plugins/:id/tool-call` with `call_id` in the body
//!      threads `correlation = "user_tool_call:<call_id>"` into every
//!      event the dispatch persists.
//!   3. Without an `X-Calm-Actor` header the middleware's `"user"`
//!      default applies — documented contract for older bridges or any
//!      caller that doesn't set the header.

#![cfg(unix)]

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use calm_server::actor::actor_middleware;
use calm_server::db::prelude::*;
use calm_server::db::sqlite::SqlxRepo;
use calm_server::event::EventBus;
use calm_server::model::{NewCove, NewWave};
use calm_server::plugin_host::{Manifest, PluginHost, PluginRegistry, PluginRuntimeStatus};
use calm_server::routes;
use calm_server::state::{AppState, CodexClient, DaemonClient};
use serde_json::json;
use tokio::time::{Instant, sleep};
use tower::ServiceExt;

const TOOLCALL_BIN: &str = env!("CARGO_BIN_EXE_plugin-host-stub-toolcall");

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn app(state: AppState) -> axum::Router {
    // Mirror main.rs: REST router under the actor middleware. Without this
    // the `Actor` extractor inside `ingest_hook` would 500.
    axum::Router::new()
        .merge(routes::router())
        .layer(axum::middleware::from_fn(actor_middleware))
        .with_state(state)
}

async fn fetch_actor_correlation(repo: &SqlxRepo, id: i64) -> (String, Option<String>) {
    let row: (String, Option<String>) =
        sqlx::query_as("SELECT actor, correlation FROM events WHERE id = ?1")
            .bind(id)
            .fetch_one(repo.pool())
            .await
            .expect("fetch event row");
    row
}

async fn last_event_id(repo: &SqlxRepo, kind: &str) -> i64 {
    let row: (i64,) =
        sqlx::query_as("SELECT id FROM events WHERE kind = ?1 ORDER BY id DESC LIMIT 1")
            .bind(kind)
            .fetch_one(repo.pool())
            .await
            .unwrap_or_else(|e| panic!("no event with kind={kind}: {e}"));
    row.0
}

fn base_state(repo: Arc<SqlxRepo>, events: EventBus) -> AppState {
    AppState::from_parts(
        repo.clone(),
        events.clone(),
        Arc::new(DaemonClient::new_stub()),
        Arc::new(PluginHost::new_full(
            Arc::new(PluginRegistry::empty()),
            repo.clone(),
            std::path::PathBuf::new(),
            std::env::temp_dir().join("calm-plugins-data"),
            Vec::new(),
            events,
            calm_server::card_role_cache::CardRoleCache::new(),
            calm_server::wave_cove_cache::WaveCoveCache::new(),
        )),
        Arc::new(CodexClient::new_stub()),
        None,
        None,
    )
}

// ---------------------------------------------------------------------------
// Test 1 — codex bridge actor lands in events.actor
// ---------------------------------------------------------------------------

#[tokio::test]
async fn codex_hook_records_ai_codex_actor_from_card_id_query() {
    // PR3 (#136) — the codex bridge ingest path now reattributes from
    // the `card_id` query parameter via `ActorId::AiCodex(CardId)`.
    // The role gate's empty-CardId guard catches unset card_ids; the
    // unknown-card guard catches card_ids that aren't in the role
    // cache (e.g. the card was deleted between hook fire and ingest).
    // So this test must seed a real card and put it into the role
    // cache before POSTing.
    let repo = Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap());
    // Seed cove + wave + card so the card_id query points at a row
    // the role cache will see.
    let cove = repo
        .cove_create(NewCove {
            name: "c".into(),
            color: "#fff".into(),
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
    let card = repo
        .card_create(calm_server::model::NewCard {
            wave_id: wave.id.clone(),
            kind: "codex".into(),
            sort: None,
            payload: json!({}),
        })
        .await
        .unwrap();
    let events = EventBus::new();
    let cache = calm_server::card_role_cache::CardRoleCache::new();
    repo.seed_card_role_cache(&cache).await.unwrap();
    let state = calm_server::state::AppState::from_parts(
        repo.clone() as Arc<dyn Repo>,
        events.clone(),
        Arc::new(DaemonClient::new_stub()),
        Arc::new(PluginHost::new_full(
            Arc::new(PluginRegistry::empty()),
            repo.clone() as Arc<dyn Repo>,
            std::path::PathBuf::new(),
            std::env::temp_dir().join("calm-plugins-data"),
            Vec::new(),
            events.clone(),
            cache.clone(),
            calm_server::wave_cove_cache::WaveCoveCache::new(),
        )),
        Arc::new(CodexClient::new_stub()),
        Some(cache),
        Some(calm_server::wave_cove_cache::WaveCoveCache::new()),
    );
    let app = app(state);

    let body = json!({
        "hook_event_name": "PreToolUse",
        "tool_name": "Bash",
        "tool_input": { "command": "ls" },
    })
    .to_string();

    let uri = format!("/internal/codex/hook?card_id={}", card.id);
    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(uri)
                .header("content-type", "application/json")
                .header("X-Calm-Actor", "ai:codex")
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NO_CONTENT);

    let id = last_event_id(&repo, "codex.hook").await;
    let (actor, correlation) = fetch_actor_correlation(&repo, id).await;
    let actor_json: serde_json::Value =
        serde_json::from_str(&actor).expect("events.actor is JSON-serialized ActorId");
    assert_eq!(
        actor_json,
        serde_json::json!({"kind": "AiCodex", "id": card.id.as_str()}),
        "ingest_hook stamps AiCodex(<card_id from query>) per PR3 of #136"
    );
    assert_eq!(
        correlation, None,
        "ingest_hook never sets correlation — no caller-side tracing id"
    );
}

// ---------------------------------------------------------------------------
// Test 3 — default actor applies when header absent
// ---------------------------------------------------------------------------
//
// Lives between tests 1 and 2 so the assertion sits next to its counterpart:
// "with header → ai:codex" / "without header → user". The numbering in the
// scope brief is a narrative order, not a file order.

#[tokio::test]
async fn codex_hook_with_missing_card_is_rejected_by_role_gate() {
    // PR3 (#136) — even when the header is absent, the route stamps
    // `ActorId::AiCodex(<card_id from query>)`. If the card_id
    // references a card the role cache doesn't know (because it was
    // never minted, or was deleted between hook fire and ingest),
    // `enforce_role`'s unknown-card branch denies the write. This is
    // intentional: refusing to ingest a hook for a deleted /
    // fabricated card id is the safe default. The header's presence
    // doesn't change the gate's behaviour at all in PR3.
    let repo = Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap());
    let events = EventBus::new();
    let app = app(base_state(repo.clone(), events.clone()));

    let body = json!({
        "hook_event_name": "Stop",
    })
    .to_string();

    let resp = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/internal/codex/hook?card_id=card_legacy")
                .header("content-type", "application/json")
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);

    // No events row should have been written.
    let count: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM events WHERE kind = 'codex.hook'")
        .fetch_one(repo.pool())
        .await
        .unwrap();
    assert_eq!(count.0, 0);
}

// ---------------------------------------------------------------------------
// Test 2 — plugin tool-call threads correlation
// ---------------------------------------------------------------------------
//
// Boots a real `PluginHost` running `stub-plugin-toolcall` (any mode — the
// stub answers `initialize` so the plugin reaches Running; we don't need
// it to respond to a real tools/call because the kernel handles the
// `neige.*` dispatch internally). Calls
// `POST /api/plugins/<id>/tool-call` with `name = "neige.overlay.set"`
// and `call_id = "abc-123"`; then verifies the resulting `overlay.set`
// row in `events` carries `correlation = "user_tool_call:abc-123"`.

#[tokio::test]
async fn plugin_tool_call_threads_call_id_as_correlation() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let plugins_dir = tmp.path().join("plugins");
    let plugins_data_dir = tmp.path().join("plugins-data");
    let plugin_id = "test.callid.overlay";
    let install_dir = plugins_dir.join(plugin_id);
    let bin_dir = install_dir.join("bin");
    std::fs::create_dir_all(&bin_dir).unwrap();
    std::fs::create_dir_all(&plugins_data_dir).unwrap();
    std::os::unix::fs::symlink(Path::new(TOOLCALL_BIN), bin_dir.join("stub")).unwrap();

    let repo: Arc<SqlxRepo> = Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap());
    // Seed cove + wave so the plugin can overlay-set onto a real wave id.
    let cove = repo
        .cove_create(NewCove {
            name: "demo".into(),
            color: "#fff".into(),
            sort: None,
        })
        .await
        .unwrap();
    let wave = repo
        .wave_create(NewWave {
            cove_id: cove.id,
            title: "w".into(),
            sort: None,
        })
        .await
        .unwrap();

    let manifest_json = json!({
        "manifest_version": 1,
        "id": plugin_id,
        "version": "0.1.0",
        "min_kernel_version": "0.0.1",
        "display_name": "Call-id correlation",
        "entrypoint": { "command": "bin/stub" },
        // #198 concern 5: per-view permissions.tools is enforced on
        // /api/plugins/:id/tool-call. The test exercises neige.overlay.set,
        // so grant exactly that.
        "views": [{
            "view_id": "main",
            "title": "Main",
            "scope": "card",
            "permissions": { "tools": ["neige.overlay.set"] }
        }],
        "permissions": {
            "overlays_write": ["wave"],
            "cards_create": false,
            "cards_read_all": true,
            "events_subscribe": []
        }
    });
    let manifest: Manifest = Manifest::parse(&manifest_json.to_string()).expect("manifest");

    let registry = PluginRegistry::empty();
    registry.insert(manifest, Some(install_dir.clone()));
    let events = EventBus::new();
    repo.plugin_install(calm_server::model::NewPlugin {
        id: plugin_id.into(),
        version: "0.1.0".into(),
        install_path: install_dir.display().to_string(),
        manifest: json!({}),
        enabled: true,
        user_config: json!({}),
    })
    .await
    .expect("seed plugin row");
    let plugin_host = Arc::new(PluginHost::new_full(
        Arc::new(registry),
        repo.clone() as Arc<dyn Repo>,
        plugins_dir,
        plugins_data_dir,
        Vec::new(),
        events.clone(),
        calm_server::card_role_cache::CardRoleCache::new(),
        calm_server::wave_cove_cache::WaveCoveCache::new(),
    ));

    plugin_host.spawn(plugin_id).await.expect("spawn");
    // Wait-for-running mirrors plugin_routes_tool_call.rs.
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Some(s) = plugin_host.status(plugin_id).await
            && matches!(s.status, PluginRuntimeStatus::Running)
        {
            break;
        }
        if Instant::now() > deadline {
            panic!("plugin did not reach Running within 5s");
        }
        sleep(Duration::from_millis(25)).await;
    }

    let state = AppState::from_parts(
        repo.clone() as Arc<dyn Repo>,
        events,
        Arc::new(DaemonClient::new_stub()),
        plugin_host.clone(),
        Arc::new(CodexClient::new_stub()),
        None, // PR3 (#136): card_role_cache — tests don't exercise role gating
        None, // #234: wave_cove_cache — same rationale
    );

    let body = json!({
        "name": "neige.overlay.set",
        "arguments": {
            "entity_kind": "wave",
            "entity_id": wave.id,
            "kind": "status",
            "payload": { "state": "running" }
        },
        "call_id": "abc-123"
    })
    .to_string();

    let resp = app(state.clone())
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/api/plugins/{plugin_id}/tool-call"))
                .header("content-type", "application/json")
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let id = last_event_id(&repo, "overlay.set").await;
    let (actor, correlation) = fetch_actor_correlation(&repo, id).await;
    // PR2 of #136 typed the actor: overlay-set from the plugin callback
    // path now lands as `ActorId::Plugin(<id>)`, serialized into the
    // `events.actor` TEXT column as the typed JSON shape.
    let actor_json: serde_json::Value =
        serde_json::from_str(&actor).expect("events.actor is JSON-serialized ActorId");
    assert_eq!(
        actor_json,
        serde_json::json!({"kind": "Plugin", "id": plugin_id}),
        "overlay-set actor is always Plugin(<id>), server-enforced"
    );
    assert_eq!(
        correlation.as_deref(),
        Some("user_tool_call:abc-123"),
        "call_id from request body must thread to events.correlation"
    );

    plugin_host.stop(plugin_id).await.ok();
}

// ---------------------------------------------------------------------------
// Test 2b — omitting call_id still works (correlation NULL)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn plugin_tool_call_without_call_id_leaves_correlation_null() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let plugins_dir = tmp.path().join("plugins");
    let plugins_data_dir = tmp.path().join("plugins-data");
    let plugin_id = "test.no-callid.overlay";
    let install_dir = plugins_dir.join(plugin_id);
    let bin_dir = install_dir.join("bin");
    std::fs::create_dir_all(&bin_dir).unwrap();
    std::fs::create_dir_all(&plugins_data_dir).unwrap();
    std::os::unix::fs::symlink(Path::new(TOOLCALL_BIN), bin_dir.join("stub")).unwrap();

    let repo: Arc<SqlxRepo> = Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap());
    let cove = repo
        .cove_create(NewCove {
            name: "demo".into(),
            color: "#fff".into(),
            sort: None,
        })
        .await
        .unwrap();
    let wave = repo
        .wave_create(NewWave {
            cove_id: cove.id,
            title: "w".into(),
            sort: None,
        })
        .await
        .unwrap();

    let manifest_json = json!({
        "manifest_version": 1,
        "id": plugin_id,
        "version": "0.1.0",
        "min_kernel_version": "0.0.1",
        "display_name": "No-callid",
        "entrypoint": { "command": "bin/stub" },
        "views": [{
            "view_id": "main",
            "title": "Main",
            "scope": "card",
            "permissions": { "tools": ["neige.overlay.set"] }
        }],
        "permissions": {
            "overlays_write": ["wave"],
            "cards_create": false,
            "cards_read_all": true,
            "events_subscribe": []
        }
    });
    let manifest: Manifest = Manifest::parse(&manifest_json.to_string()).expect("manifest");

    let registry = PluginRegistry::empty();
    registry.insert(manifest, Some(install_dir.clone()));
    let events = EventBus::new();
    repo.plugin_install(calm_server::model::NewPlugin {
        id: plugin_id.into(),
        version: "0.1.0".into(),
        install_path: install_dir.display().to_string(),
        manifest: json!({}),
        enabled: true,
        user_config: json!({}),
    })
    .await
    .expect("seed plugin row");
    let plugin_host = Arc::new(PluginHost::new_full(
        Arc::new(registry),
        repo.clone() as Arc<dyn Repo>,
        plugins_dir,
        plugins_data_dir,
        Vec::new(),
        events.clone(),
        calm_server::card_role_cache::CardRoleCache::new(),
        calm_server::wave_cove_cache::WaveCoveCache::new(),
    ));

    plugin_host.spawn(plugin_id).await.expect("spawn");
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Some(s) = plugin_host.status(plugin_id).await
            && matches!(s.status, PluginRuntimeStatus::Running)
        {
            break;
        }
        if Instant::now() > deadline {
            panic!("plugin did not reach Running within 5s");
        }
        sleep(Duration::from_millis(25)).await;
    }

    let state = AppState::from_parts(
        repo.clone() as Arc<dyn Repo>,
        events,
        Arc::new(DaemonClient::new_stub()),
        plugin_host.clone(),
        Arc::new(CodexClient::new_stub()),
        None, // PR3 (#136): card_role_cache — tests don't exercise role gating
        None, // #234: wave_cove_cache — same rationale
    );

    // No call_id field at all — exercises serde default + "no allocation"
    // path of `CallbackCtx::correlation`.
    let body = json!({
        "name": "neige.overlay.set",
        "arguments": {
            "entity_kind": "wave",
            "entity_id": wave.id,
            "kind": "status",
            "payload": { "state": "running" }
        }
    })
    .to_string();

    let resp = app(state.clone())
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/api/plugins/{plugin_id}/tool-call"))
                .header("content-type", "application/json")
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let id = last_event_id(&repo, "overlay.set").await;
    let (_, correlation) = fetch_actor_correlation(&repo, id).await;
    assert_eq!(
        correlation, None,
        "absent call_id must leave events.correlation NULL"
    );

    plugin_host.stop(plugin_id).await.ok();
}

// ---------------------------------------------------------------------------
// Test 2c — empty-string call_id normalizes to absent
// ---------------------------------------------------------------------------
//
// A buggy/legacy client that sends `call_id: ""` (e.g. an iframe that calls
// `crypto.randomUUID()` in a context where it returned empty, or a manual
// curl invocation) must not produce a dangling `correlation =
// "user_tool_call:"` row. The route normalizes empty to absent before
// threading into the callback ctx.

#[tokio::test]
async fn plugin_tool_call_treats_empty_call_id_as_absent() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let plugins_dir = tmp.path().join("plugins");
    let plugins_data_dir = tmp.path().join("plugins-data");
    let plugin_id = "test.empty-callid.overlay";
    let install_dir = plugins_dir.join(plugin_id);
    let bin_dir = install_dir.join("bin");
    std::fs::create_dir_all(&bin_dir).unwrap();
    std::fs::create_dir_all(&plugins_data_dir).unwrap();
    std::os::unix::fs::symlink(Path::new(TOOLCALL_BIN), bin_dir.join("stub")).unwrap();

    let repo: Arc<SqlxRepo> = Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap());
    let cove = repo
        .cove_create(NewCove {
            name: "demo".into(),
            color: "#fff".into(),
            sort: None,
        })
        .await
        .unwrap();
    let wave = repo
        .wave_create(NewWave {
            cove_id: cove.id,
            title: "w".into(),
            sort: None,
        })
        .await
        .unwrap();

    let manifest_json = json!({
        "manifest_version": 1,
        "id": plugin_id,
        "version": "0.1.0",
        "min_kernel_version": "0.0.1",
        "display_name": "Empty-callid",
        "entrypoint": { "command": "bin/stub" },
        "views": [{
            "view_id": "main",
            "title": "Main",
            "scope": "card",
            "permissions": { "tools": ["neige.overlay.set"] }
        }],
        "permissions": {
            "overlays_write": ["wave"],
            "cards_create": false,
            "cards_read_all": true,
            "events_subscribe": []
        }
    });
    let manifest: Manifest = Manifest::parse(&manifest_json.to_string()).expect("manifest");

    let registry = PluginRegistry::empty();
    registry.insert(manifest, Some(install_dir.clone()));
    let events = EventBus::new();
    repo.plugin_install(calm_server::model::NewPlugin {
        id: plugin_id.into(),
        version: "0.1.0".into(),
        install_path: install_dir.display().to_string(),
        manifest: json!({}),
        enabled: true,
        user_config: json!({}),
    })
    .await
    .expect("seed plugin row");
    let plugin_host = Arc::new(PluginHost::new_full(
        Arc::new(registry),
        repo.clone() as Arc<dyn Repo>,
        plugins_dir,
        plugins_data_dir,
        Vec::new(),
        events.clone(),
        calm_server::card_role_cache::CardRoleCache::new(),
        calm_server::wave_cove_cache::WaveCoveCache::new(),
    ));

    plugin_host.spawn(plugin_id).await.expect("spawn");
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Some(s) = plugin_host.status(plugin_id).await
            && matches!(s.status, PluginRuntimeStatus::Running)
        {
            break;
        }
        if Instant::now() > deadline {
            panic!("plugin did not reach Running within 5s");
        }
        sleep(Duration::from_millis(25)).await;
    }

    let state = AppState::from_parts(
        repo.clone() as Arc<dyn Repo>,
        events,
        Arc::new(DaemonClient::new_stub()),
        plugin_host.clone(),
        Arc::new(CodexClient::new_stub()),
        None, // PR3 (#136): card_role_cache — tests don't exercise role gating
        None, // #234: wave_cove_cache — same rationale
    );

    // Empty-string call_id — must be normalized to absent, NOT produce
    // `correlation = "user_tool_call:"`.
    let body = json!({
        "name": "neige.overlay.set",
        "arguments": {
            "entity_kind": "wave",
            "entity_id": wave.id,
            "kind": "status",
            "payload": { "state": "running" }
        },
        "call_id": ""
    })
    .to_string();

    let resp = app(state.clone())
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/api/plugins/{plugin_id}/tool-call"))
                .header("content-type", "application/json")
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let id = last_event_id(&repo, "overlay.set").await;
    let (_, correlation) = fetch_actor_correlation(&repo, id).await;
    assert_eq!(
        correlation, None,
        "empty-string call_id must normalize to NULL — no dangling `user_tool_call:` rows"
    );

    plugin_host.stop(plugin_id).await.ok();
}
