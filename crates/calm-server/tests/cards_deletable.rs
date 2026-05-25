//! Issue #229 PR A — system-card infrastructure.
//!
//! Coverage:
//!
//!   1. **Repo round-trip** — `card_create_with_id_tx` stores and
//!      `cards_by_wave` / `card_get` hydrate the `deletable` bit
//!      correctly for both `true` and `false`.
//!   2. **Migration backfill** — existing spec cards (minted via
//!      `POST /api/waves`) come back from `card_get` with
//!      `deletable = false` after migration 0013 runs, even though no
//!      caller passed the bit explicitly through the wire.
//!   3. **REST DELETE guard** — `DELETE /api/cards/:id` returns 403 on
//!      an undeletable (spec) card; 204 on a deletable (plain) card.
//!   4. **Wave delete cascade** — `DELETE /api/waves/:id` still
//!      cascades through to undeletable cards; the guard is scoped to
//!      `/api/cards/:id` only.
//!   5. **CardPatch deletable rejection** — `PATCH /api/cards/:id`
//!      with `{"deletable": ...}` in the body returns 400 (the field
//!      is not patchable from the API).
//!
//! Plugin-callback refusal lives next to the rest of the plugin host
//! tests in `crates/calm-server/src/plugin_host/callbacks.rs`
//! (mod tests).

#![cfg(unix)]

use std::path::PathBuf;
use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use calm_server::card_role_cache::CardRoleCache;
use calm_server::db::prelude::*;
use calm_server::db::sqlite::SqlxRepo;
use calm_server::event::EventBus;
use calm_server::model::{CardRole, NewCard, NewCove, NewWave};
use calm_server::plugin_host::{PluginHost, PluginRegistry};
use calm_server::routes;
use calm_server::state::{AppState, DaemonClient};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tempfile::TempDir;
use tower::ServiceExt;

mod common;

// ---------------------------------------------------------------------------
// Boot — same shape as `tests/spec_card.rs`, kept minimal here.
//
// Post-#236 (synchronous spec-daemon spawn): these tests need
// `POST /api/waves` to actually return 201, so we point `DaemonClient`
// at the real `calm-session-daemon` binary built into the workspace
// `target/` dir. The daemon binds its socket before exec'ing the
// inner program, so `spawn_daemon_for`'s wait-for-socket loop
// completes even when the inner program fails immediately. Same locator
// pattern as `tests/wave_create_sync_daemon.rs` and
// `tests/codex_card_endpoint.rs`.
//
// #293 cutover — `POST /api/waves` now ALSO boots a kernel-owned `codex
// app-server` before returning 201 (`spec_appserver::spawn_spec_appserver`,
// invoked via `s.codex.codex_bin`). With no real codex on PATH that spawn
// 500s. So we use `common::fake_codex_client()`, whose `codex_bin` points
// at the `osc-probe-child` fixture's fake app-server (see
// `tests/common/mod.rs`), instead of the bare `CodexClient::new_stub()`.
// ---------------------------------------------------------------------------

/// Same daemon-locator as `wave_create_sync_daemon.rs` /
/// `codex_card_endpoint.rs` — workspace bins live one dir up from the
/// per-test `deps/` directory.
fn locate_daemon_bin() -> PathBuf {
    let mut p = std::env::current_exe().expect("current_exe");
    p.pop();
    p.pop();
    p.push("calm-session-daemon");
    assert!(
        p.exists(),
        "calm-session-daemon not found at {p:?}; run \
         `cargo build -p calm-session --bin calm-session-daemon` first, or \
         use `cargo test --workspace` which builds workspace bins",
    );
    p
}

struct Boot {
    app: axum::Router,
    cove_id: String,
    repo: Arc<dyn Repo>,
    _tmp: TempDir,
}

async fn boot() -> Boot {
    let tmp = TempDir::new().expect("tempdir");
    let repo: Arc<dyn Repo> = Arc::new(
        SqlxRepo::open("sqlite::memory:")
            .await
            .expect("open in-memory sqlite"),
    );
    let cove = repo
        .cove_create(NewCove {
            name: "deletable-test".into(),
            color: "#000".into(),
            sort: None,
        })
        .await
        .unwrap();

    let daemon = Arc::new(DaemonClient {
        data_dir: tmp.path().to_path_buf(),
        session_daemon_bin: locate_daemon_bin(),
    });
    let events = EventBus::new();
    let card_role_cache = CardRoleCache::new();
    let wave_cove_cache = calm_server::wave_cove_cache::WaveCoveCache::new();
    repo.seed_wave_cove_cache(&wave_cove_cache).await.unwrap();
    let state = AppState::from_parts(
        repo.clone(),
        events,
        daemon,
        Arc::new(PluginHost::new_full(
            Arc::new(PluginRegistry::empty()),
            repo.clone(),
            PathBuf::new(),
            std::env::temp_dir().join("calm-plugins-data-deletable-test"),
            Vec::new(),
            EventBus::new(),
            card_role_cache.clone(),
            wave_cove_cache.clone(),
        )),
        Arc::new(common::fake_codex_client()),
        Some(card_role_cache.clone()),
        Some(wave_cove_cache.clone()),
    );

    let app = routes::router()
        .layer(axum::middleware::from_fn(
            calm_server::actor::actor_middleware,
        ))
        .with_state(state);

    Boot {
        app,
        cove_id: cove.id.to_string(),
        repo,
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
    let body: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, body)
}

async fn delete(app: axum::Router, uri: &str) -> StatusCode {
    let resp = app
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri(uri)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    resp.status()
}

async fn patch(app: axum::Router, uri: &str, body: Value) -> (StatusCode, Value) {
    let resp = app
        .oneshot(
            Request::builder()
                .method("PATCH")
                .uri(uri)
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let body: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, body)
}

// ---------------------------------------------------------------------------
// (1) Repo round-trip
// ---------------------------------------------------------------------------

#[tokio::test]
async fn card_create_with_id_tx_round_trips_deletable_bit() {
    // Both `true` (default for plain user cards) and `false` (kernel
    // owned) round-trip cleanly through INSERT → SELECT and through
    // both repo accessors (`card_get`, `cards_by_wave`).
    let repo = SqlxRepo::open("sqlite::memory:")
        .await
        .expect("open in-memory sqlite");
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
            cwd: String::new(),
            attach_folder: false,
            theme: calm_server::routes::theme::RequestTheme::default_dark(),
        })
        .await
        .unwrap();
    let cache = CardRoleCache::new();

    // Deletable card.
    let mut tx = repo.pool().begin().await.unwrap();
    let deletable_card = calm_server::db::sqlite::card_create_with_id_tx(
        &mut tx,
        calm_server::model::new_id(),
        NewCard {
            wave_id: wave.id.clone(),
            kind: "terminal".into(),
            sort: None,
            payload: json!({}),
        },
        CardRole::Plain,
        true,
        &cache,
    )
    .await
    .unwrap();

    // Undeletable card. Note the role is Plain here — the test isolates
    // the `deletable` axis from the role axis. Production callers wire
    // `false` only on kernel-owned cards (Spec / ReportCard).
    let undeletable_card = calm_server::db::sqlite::card_create_with_id_tx(
        &mut tx,
        calm_server::model::new_id(),
        NewCard {
            wave_id: wave.id.clone(),
            kind: "terminal".into(),
            sort: None,
            payload: json!({}),
        },
        CardRole::Plain,
        false,
        &cache,
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();

    // The returned struct carries the bit (constructor path).
    assert!(deletable_card.deletable);
    assert!(!undeletable_card.deletable);

    // `card_get` hydrates the bit from the row.
    let got_deletable = repo
        .card_get(deletable_card.id.as_str())
        .await
        .unwrap()
        .expect("deletable card");
    assert!(got_deletable.deletable);
    let got_undeletable = repo
        .card_get(undeletable_card.id.as_str())
        .await
        .unwrap()
        .expect("undeletable card");
    assert!(!got_undeletable.deletable);

    // `cards_by_wave` hydrates both.
    let listed = repo.cards_by_wave(wave.id.as_str()).await.unwrap();
    assert_eq!(listed.len(), 2);
    let by_id: std::collections::HashMap<_, _> = listed
        .iter()
        .map(|c| (c.id.as_str().to_string(), c))
        .collect();
    assert!(by_id.get(deletable_card.id.as_str()).unwrap().deletable);
    assert!(!by_id.get(undeletable_card.id.as_str()).unwrap().deletable);
}

// ---------------------------------------------------------------------------
// (2) Migration backfill — spec cards minted by `POST /api/waves` come
// back with deletable=false. The migration's `UPDATE ... WHERE role =
// 'spec'` covers legacy rows; the wave-create route also passes
// `deletable: false` explicitly so fresh rows inherit the same shape.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn spec_card_minted_by_wave_create_is_undeletable() {
    let boot = boot().await;
    let (status, body) = post(
        boot.app.clone(),
        "/api/waves",
        json!({"cove_id": boot.cove_id, "title": "w", "cwd": "/tmp/issue-250-pr2-test", "attach_folder": true, "theme": {"fg": [216,219,226], "bg": [15,20,24]} }),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "wave create returned: {body}");
    let wave_id = body
        .get("id")
        .and_then(Value::as_str)
        .expect("wave id in response")
        .to_string();

    let cards = boot.repo.cards_by_wave(&wave_id).await.unwrap();
    // Issue #229 PR B — wave create now mints two cards in the same tx:
    // the spec card (PR6) and the wave-report card (PR B). Both are
    // kernel-owned (`deletable = false`); the report card sorts ahead
    // (`sort = -1.0`) so the WaveGrid renders it at the top.
    assert_eq!(
        cards.len(),
        2,
        "wave create mints spec + wave-report; got {} cards",
        cards.len(),
    );
    assert!(
        cards.iter().all(|c| !c.deletable),
        "both spec and wave-report cards must be undeletable; got: {:?}",
        cards
            .iter()
            .map(|c| (c.kind.clone(), c.deletable))
            .collect::<Vec<_>>(),
    );
    // Sanity: each role is represented exactly once.
    let kinds: Vec<&str> = cards.iter().map(|c| c.kind.as_str()).collect();
    assert!(
        kinds.contains(&"codex"),
        "spec card kind is codex; got {kinds:?}"
    );
    assert!(
        kinds.contains(&"wave-report"),
        "wave-report card present; got {kinds:?}"
    );
}

// ---------------------------------------------------------------------------
// (3) REST DELETE guard.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn delete_card_returns_403_for_undeletable_spec_card() {
    let boot = boot().await;
    // Mint a wave (and thus its spec card).
    let (status, body) = post(
        boot.app.clone(),
        "/api/waves",
        json!({"cove_id": boot.cove_id, "title": "w", "cwd": "/tmp/issue-250-pr2-test", "attach_folder": true, "theme": {"fg": [216,219,226], "bg": [15,20,24]} }),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "wave create body: {body}");
    let wave_id = body["id"].as_str().unwrap().to_string();
    let cards = boot.repo.cards_by_wave(&wave_id).await.unwrap();
    // Find the spec card by kind (PR B adds a wave-report card alongside).
    let spec_card = cards
        .iter()
        .find(|c| c.kind == "codex")
        .expect("spec card present");
    let spec_card_id = spec_card.id.as_str().to_string();
    assert!(!spec_card.deletable);

    // DELETE /api/cards/:id on the spec card → 403.
    let status = delete(boot.app.clone(), &format!("/api/cards/{spec_card_id}")).await;
    assert_eq!(
        status,
        StatusCode::FORBIDDEN,
        "spec card delete must be refused with 403"
    );

    // The row is still there.
    let after = boot.repo.card_get(&spec_card_id).await.unwrap();
    assert!(
        after.is_some(),
        "spec card row must survive the refused delete"
    );
}

#[tokio::test]
async fn delete_card_returns_204_for_deletable_plain_card() {
    let boot = boot().await;
    // Wave + plain user card.
    let (status, body) = post(
        boot.app.clone(),
        "/api/waves",
        json!({"cove_id": boot.cove_id, "title": "w", "cwd": "/tmp/issue-250-pr2-test", "attach_folder": true, "theme": {"fg": [216,219,226], "bg": [15,20,24]} }),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "wave create body: {body}");
    let wave_id = body["id"].as_str().unwrap().to_string();

    let (status, body) = post(
        boot.app.clone(),
        &format!("/api/waves/{wave_id}/cards"),
        json!({"kind": "plugin:t:v"}),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::CREATED,
        "plain card create body: {body}"
    );
    let plain_card_id = body["id"].as_str().unwrap().to_string();

    let status = delete(boot.app.clone(), &format!("/api/cards/{plain_card_id}")).await;
    assert_eq!(
        status,
        StatusCode::NO_CONTENT,
        "plain user-deletable card delete returns 204"
    );

    let after = boot.repo.card_get(&plain_card_id).await.unwrap();
    assert!(after.is_none(), "plain card row removed");
}

// ---------------------------------------------------------------------------
// (4) Wave delete cascade — undeletable cards still go away when their
// parent wave is deleted. The guard scopes to `/api/cards/:id` only.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn wave_delete_cascades_to_undeletable_spec_card() {
    let boot = boot().await;
    let (status, body) = post(
        boot.app.clone(),
        "/api/waves",
        json!({"cove_id": boot.cove_id, "title": "w", "cwd": "/tmp/issue-250-pr2-test", "attach_folder": true, "theme": {"fg": [216,219,226], "bg": [15,20,24]} }),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "wave create body: {body}");
    let wave_id = body["id"].as_str().unwrap().to_string();
    let cards = boot.repo.cards_by_wave(&wave_id).await.unwrap();
    let spec_card = cards
        .iter()
        .find(|c| c.kind == "codex")
        .expect("spec card present");
    let spec_card_id = spec_card.id.as_str().to_string();
    assert!(!spec_card.deletable);

    // The wave-delete route surfaces the cascade through the FK chain.
    // Spec cards carry a terminal, and `terminals.card_id` is ON DELETE
    // RESTRICT (migration 0011); the route's terminal-reap step handles
    // that. We just assert the end state: wave gone, card gone, no 403
    // leak from the per-card guard.
    let status = delete(boot.app.clone(), &format!("/api/waves/{wave_id}")).await;
    assert_eq!(
        status,
        StatusCode::NO_CONTENT,
        "wave delete must succeed even with an undeletable child card"
    );

    let after_wave = boot.repo.wave_get(&wave_id).await.unwrap();
    assert!(after_wave.is_none());
    let after_card = boot.repo.card_get(&spec_card_id).await.unwrap();
    assert!(after_card.is_none(), "spec card cascade-deleted with wave");
}

// ---------------------------------------------------------------------------
// (5) PATCH `deletable` rejection — the field is kernel-managed and
// must not be patchable via API.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn patch_card_with_deletable_returns_400() {
    let boot = boot().await;
    let (status, body) = post(
        boot.app.clone(),
        "/api/waves",
        json!({"cove_id": boot.cove_id, "title": "w", "cwd": "/tmp/issue-250-pr2-test", "attach_folder": true, "theme": {"fg": [216,219,226], "bg": [15,20,24]} }),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "wave create body: {body}");
    let wave_id = body["id"].as_str().unwrap().to_string();
    let (status, body) = post(
        boot.app.clone(),
        &format!("/api/waves/{wave_id}/cards"),
        json!({"kind": "plugin:t:v"}),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::CREATED,
        "plain card create body: {body}"
    );
    let card_id = body["id"].as_str().unwrap().to_string();

    // The route must reject any patch carrying `deletable` (even when
    // the value matches the current row — the field is kernel-managed,
    // not "stable-write-allowed"). Belt-and-suspenders against a future
    // client that thinks `{"deletable": true}` is a no-op echo.
    let (status, body) = patch(
        boot.app.clone(),
        &format!("/api/cards/{card_id}"),
        json!({"deletable": false}),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "patching deletable must 400; body={body}",
    );
}
