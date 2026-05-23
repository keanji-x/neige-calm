//! Issue #250 PR 2 — coverage for `Wave.cwd`, `Wave.terminal_at`, the
//! `POST /api/waves` cwd-claim handling (attach_folder + resolve),
//! lifecycle terminal-stamp wiring inside `wave_update_tx`, and the
//! calendar window query `GET /api/waves?since&until&cove_id`.
//!
//! These tests boot a stub-daemon router (no real codex / no real
//! `calm-session-daemon`) so the spec-daemon spawn fails synchronously
//! on `POST /api/waves` — the route returns 500 on that branch but
//! the wave + cards + (optional) cove_folder rows still land at
//! commit time. Every assertion below targets DB state, the lifecycle
//! → terminal_at wiring, and the route-layer body shapes — none of
//! the assertions need the daemon to actually exec the codex binary.
//!
//! Tests in `wave_create_sync_daemon.rs` cover the real-daemon path
//! end-to-end (spec daemon cwd == wave.cwd, codex argv carries title);
//! this file owns the wider behavioral surface that doesn't need a
//! real spawn.

#![cfg(unix)]

use std::path::PathBuf;
use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use calm_server::card_role_cache::CardRoleCache;
use calm_server::db::prelude::*;
use calm_server::db::sqlite::SqlxRepo;
use calm_server::event::EventBus;
use calm_server::model::{NewCove, WaveLifecycle, WavePatch};
use calm_server::plugin_host::{PluginHost, PluginRegistry};
use calm_server::routes;
use calm_server::state::{AppState, CodexClient, DaemonClient};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tempfile::TempDir;
use tower::ServiceExt;

struct Boot {
    app: axum::Router,
    cove_id: String,
    /// A second cove pre-created so cross-cove conflict tests have a
    /// stable target. Used by the descendant/ancestor cases below.
    other_cove_id: String,
    repo: Arc<dyn Repo>,
    /// Concrete `SqlxRepo` handle so the window-query test can write
    /// raw timestamps via `pool()`. The same backing pool as `repo`
    /// (both `Arc`s point at the same `SqlxRepo`).
    sqlx_repo: Arc<SqlxRepo>,
    _tmp: TempDir,
}

async fn boot() -> Boot {
    let tmp = TempDir::new().expect("tempdir");
    let sqlx_repo = Arc::new(
        SqlxRepo::open("sqlite::memory:")
            .await
            .expect("open in-memory sqlite"),
    );
    let repo: Arc<dyn Repo> = sqlx_repo.clone();
    let cove = repo
        .cove_create(NewCove {
            name: "wave-cwd-test".into(),
            color: "#000".into(),
            sort: None,
        })
        .await
        .unwrap();
    let other = repo
        .cove_create(NewCove {
            name: "other-cove".into(),
            color: "#111".into(),
            sort: None,
        })
        .await
        .unwrap();

    // Stub daemon bin — spec card daemon spawn will fail at the
    // post-commit phase. The behaviors under test (wave + folder row
    // shape, terminal_at stamps) all execute *before* the spawn, so
    // a 500 on the response is expected and the test asserts on DB
    // state instead.
    let daemon = Arc::new(DaemonClient {
        data_dir: tmp.path().to_path_buf(),
        session_daemon_bin: PathBuf::from("/nonexistent-daemon-bin-cwd-test"),
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
            std::env::temp_dir().join("calm-plugins-data-cwd-test"),
            Vec::new(),
            EventBus::new(),
            card_role_cache.clone(),
            wave_cove_cache.clone(),
        )),
        Arc::new(CodexClient::new_stub()),
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
        other_cove_id: other.id.to_string(),
        repo,
        sqlx_repo,
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
    let json: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, json)
}

async fn get(app: axum::Router, uri: &str) -> (StatusCode, Value) {
    let resp = app
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(uri)
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

// ---------------------------------------------------------------------------
// POST /api/waves — cwd validation + attach_folder path
// ---------------------------------------------------------------------------

/// Happy path 1: the body's cove already claims an ancestor of cwd.
/// `attach_folder = false` is enough — no new folder row is needed.
/// Spec-daemon spawn will fail (stub bin); tolerate 201 or 500 but
/// assert the wave row landed with the cwd verbatim.
#[tokio::test]
async fn post_api_waves_uses_existing_folder_claim() {
    let boot = boot().await;

    // Pre-seed: the cove claims `/workspace` as a folder.
    boot.repo
        .cove_folder_create(&boot.cove_id, "/workspace")
        .await
        .unwrap();

    let (status, body) = post(
        boot.app.clone(),
        "/api/waves",
        json!({
            "cove_id": boot.cove_id,
            "title": "w-existing-claim",
            "cwd": "/workspace/sub/dir",
            "attach_folder": false,
        }),
    )
    .await;
    // Stub daemon: spawn may 500 post-commit; wave row still lands.
    assert!(
        status == StatusCode::CREATED || status == StatusCode::INTERNAL_SERVER_ERROR,
        "expected 201 or 500 (daemon stub may fail post-commit); got {status} body={body}",
    );

    let waves = boot.repo.waves_by_cove(&boot.cove_id).await.unwrap();
    assert_eq!(waves.len(), 1, "exactly one wave created");
    assert_eq!(waves[0].cwd, "/workspace/sub/dir");
    assert_eq!(waves[0].terminal_at, None);
    assert_eq!(waves[0].lifecycle, WaveLifecycle::Draft);

    // No extra folder row was minted (attach_folder = false +
    // existing claim covers cwd).
    let folders = boot.repo.cove_folders_by_cove(&boot.cove_id).await.unwrap();
    assert_eq!(folders.len(), 1);
    assert_eq!(folders[0].path, "/workspace");
}

/// Happy path 2: cwd is unclaimed, body sets `attach_folder = true`.
/// The folder row + the wave row land in the same tx.
#[tokio::test]
async fn post_api_waves_with_attach_folder_creates_folder_and_wave() {
    let boot = boot().await;

    let (status, _body) = post(
        boot.app.clone(),
        "/api/waves",
        json!({
            "cove_id": boot.cove_id,
            "title": "w-attach",
            "cwd": "/srv/projects/alpha",
            "attach_folder": true,
        }),
    )
    .await;
    assert!(status == StatusCode::CREATED || status == StatusCode::INTERNAL_SERVER_ERROR);

    // Folder claim landed.
    let folders = boot.repo.cove_folders_by_cove(&boot.cove_id).await.unwrap();
    assert_eq!(folders.len(), 1);
    assert_eq!(folders[0].path, "/srv/projects/alpha");

    // Wave row carries the same path.
    let waves = boot.repo.waves_by_cove(&boot.cove_id).await.unwrap();
    assert_eq!(waves.len(), 1);
    assert_eq!(waves[0].cwd, "/srv/projects/alpha");
}

/// `attach_folder = false` with an unclaimed cwd is refused (409) —
/// otherwise the wave would be orphaned (no cove resolves it).
#[tokio::test]
async fn post_api_waves_rejects_unclaimed_cwd_without_attach_folder() {
    let boot = boot().await;

    let (status, body) = post(
        boot.app.clone(),
        "/api/waves",
        json!({
            "cove_id": boot.cove_id,
            "title": "w-orphan",
            "cwd": "/unclaimed/path",
            "attach_folder": false,
        }),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "body = {body}");
    // No wave / folder rows should have landed.
    assert_eq!(
        boot.repo.waves_by_cove(&boot.cove_id).await.unwrap().len(),
        0
    );
    assert_eq!(
        boot.repo
            .cove_folders_by_cove(&boot.cove_id)
            .await
            .unwrap()
            .len(),
        0,
    );
}

/// `attach_folder = true` against a cwd that already conflicts with
/// another cove's claim is refused (409) with the structured
/// `FolderConflict` body, and the whole tx rolls back (no wave row,
/// no extra folder row).
#[tokio::test]
async fn post_api_waves_attach_folder_conflict_rolls_back() {
    let boot = boot().await;

    // Pre-seed the *other* cove with a folder that overlaps the cwd
    // we're about to try claiming.
    boot.repo
        .cove_folder_create(&boot.other_cove_id, "/shared/workspace")
        .await
        .unwrap();
    let folders_before = boot.repo.cove_folders_list_all().await.unwrap().len();
    assert_eq!(folders_before, 1);

    let (status, body) = post(
        boot.app.clone(),
        "/api/waves",
        json!({
            "cove_id": boot.cove_id,
            "title": "w-conflict",
            "cwd": "/shared/workspace/inner",
            "attach_folder": true,
        }),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "body = {body}");

    // The structured 409 body carries the conflicting folder. Match
    // any of `equal | ancestor | descendant` since the route may
    // classify either side as the canonical kind; the issue's
    // requirement is just that the conflict is precisely surfaced.
    let kind = body
        .get("conflict_kind")
        .and_then(Value::as_str)
        .expect("structured FolderConflict body");
    assert!(
        matches!(kind, "descendant" | "ancestor" | "equal"),
        "unexpected conflict kind `{kind}` in body {body}",
    );

    // Rollback: no new wave, no new folder.
    assert_eq!(
        boot.repo.waves_by_cove(&boot.cove_id).await.unwrap().len(),
        0
    );
    let folders_after = boot.repo.cove_folders_list_all().await.unwrap().len();
    assert_eq!(
        folders_after, folders_before,
        "attach_folder = true must roll back the folder insert on conflict; \
         folder count before = {folders_before}, after = {folders_after}"
    );
}

/// `attach_folder = false` against a cwd that resolves to *another*
/// cove must 409 — the wave's cove and the folder's cove must agree.
#[tokio::test]
async fn post_api_waves_rejects_cwd_owned_by_another_cove() {
    let boot = boot().await;

    boot.repo
        .cove_folder_create(&boot.other_cove_id, "/owned/by/other")
        .await
        .unwrap();

    let (status, body) = post(
        boot.app.clone(),
        "/api/waves",
        json!({
            "cove_id": boot.cove_id,
            "title": "w-cross",
            "cwd": "/owned/by/other/sub",
            "attach_folder": false,
        }),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "body = {body}");
    let conflict_cove = body
        .get("cove_id")
        .and_then(Value::as_str)
        .expect("structured body");
    assert_eq!(conflict_cove, boot.other_cove_id);

    // No wave on either cove.
    assert_eq!(
        boot.repo.waves_by_cove(&boot.cove_id).await.unwrap().len(),
        0
    );
    assert_eq!(
        boot.repo
            .waves_by_cove(&boot.other_cove_id)
            .await
            .unwrap()
            .len(),
        0,
    );
}

/// Non-absolute cwd → 400 before any DB write.
#[tokio::test]
async fn post_api_waves_rejects_non_absolute_cwd() {
    let boot = boot().await;

    let (status, body) = post(
        boot.app.clone(),
        "/api/waves",
        json!({
            "cove_id": boot.cove_id,
            "title": "w-relative",
            "cwd": "relative/path",
            "attach_folder": true,
        }),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "body = {body}");
    assert_eq!(
        boot.repo.waves_by_cove(&boot.cove_id).await.unwrap().len(),
        0
    );
    assert_eq!(
        boot.repo
            .cove_folders_by_cove(&boot.cove_id)
            .await
            .unwrap()
            .len(),
        0,
    );
}

// ---------------------------------------------------------------------------
// Lifecycle → terminal_at stamping (wave_update_tx)
// ---------------------------------------------------------------------------

/// Helper: create a fresh wave in `Draft` state via the repo (bypassing
/// the route so we don't have to do the cwd/folder dance in every
/// lifecycle test).
async fn seed_wave(repo: &Arc<dyn Repo>, cove_id: &str) -> calm_server::model::Wave {
    repo.wave_create(calm_server::model::NewWave {
        cove_id: cove_id.into(),
        title: "lifecycle-test".into(),
        sort: None,
        cwd: String::new(),
        attach_folder: false,
    })
    .await
    .unwrap()
}

/// Advance through `Draft → Planning → Dispatching → Working → Reviewing
/// → Done` via direct `wave_update_tx` calls and assert that
/// `terminal_at` lands as `Some(_)` exactly once on the Done write.
#[tokio::test]
async fn lifecycle_to_done_stamps_terminal_at() {
    let boot = boot().await;
    let wave = seed_wave(&boot.repo, &boot.cove_id).await;
    // Route everything through `wave_update` (which opens a tx and
    // calls `wave_update_tx` under the hood). The lifecycle validator
    // runs at the *route* layer; bypassing it here is fine — we're
    // isolating the terminal_at column write.

    // Each step uses the public `wave_update` (which calls
    // `wave_update_tx` under the hood). terminal_at must stay None
    // for every non-terminal transition and become Some on Done.
    for step in [
        WaveLifecycle::Planning,
        WaveLifecycle::Dispatching,
        WaveLifecycle::Working,
        WaveLifecycle::Reviewing,
    ] {
        let updated = boot
            .repo
            .wave_update(
                wave.id.as_str(),
                WavePatch {
                    lifecycle: Some(step),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        assert_eq!(
            updated.terminal_at, None,
            "terminal_at must stay None while lifecycle is non-terminal ({step:?}); \
             updated row = {updated:?}",
        );
    }

    let before_done_ms = calm_server::model::now_ms();
    let done = boot
        .repo
        .wave_update(
            wave.id.as_str(),
            WavePatch {
                lifecycle: Some(WaveLifecycle::Done),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    let after_done_ms = calm_server::model::now_ms();

    let stamp = done
        .terminal_at
        .expect("terminal_at must be Some after lifecycle → Done");
    assert!(
        stamp >= before_done_ms && stamp <= after_done_ms,
        "terminal_at must be a unix-ms within the call window \
         (before={before_done_ms}, stamp={stamp}, after={after_done_ms})",
    );
    assert_eq!(done.lifecycle, WaveLifecycle::Done);
}

/// User-driven reopen (`Done → Planning`) must clear `terminal_at`.
#[tokio::test]
async fn lifecycle_reopen_clears_terminal_at() {
    let boot = boot().await;
    let wave = seed_wave(&boot.repo, &boot.cove_id).await;

    // Force the wave into Done first.
    for step in [
        WaveLifecycle::Planning,
        WaveLifecycle::Dispatching,
        WaveLifecycle::Working,
        WaveLifecycle::Reviewing,
        WaveLifecycle::Done,
    ] {
        boot.repo
            .wave_update(
                wave.id.as_str(),
                WavePatch {
                    lifecycle: Some(step),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
    }
    let done = boot.repo.wave_get(wave.id.as_str()).await.unwrap().unwrap();
    assert!(
        done.terminal_at.is_some(),
        "preconditon: terminal_at stamped"
    );

    // Now reopen — terminal → planning is the only legal reopen edge.
    let reopened = boot
        .repo
        .wave_update(
            wave.id.as_str(),
            WavePatch {
                lifecycle: Some(WaveLifecycle::Planning),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(reopened.lifecycle, WaveLifecycle::Planning);
    assert_eq!(
        reopened.terminal_at, None,
        "reopen must clear terminal_at; got {reopened:?}",
    );
}

/// Working → Blocked is non-terminal; terminal_at must not be stamped.
#[tokio::test]
async fn lifecycle_working_to_blocked_leaves_terminal_at_unset() {
    let boot = boot().await;
    let wave = seed_wave(&boot.repo, &boot.cove_id).await;

    for step in [
        WaveLifecycle::Planning,
        WaveLifecycle::Dispatching,
        WaveLifecycle::Working,
        WaveLifecycle::Blocked,
    ] {
        let updated = boot
            .repo
            .wave_update(
                wave.id.as_str(),
                WavePatch {
                    lifecycle: Some(step),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        assert_eq!(updated.terminal_at, None);
    }
}

/// Standalone tx surface check: `wave_update` (which routes through
/// `wave_update_tx`) lands `terminal_at = Some(_)` in the same write
/// as the lifecycle column. The route + MCP layers both call into
/// this same primitive, so a single repo-level assertion locks the
/// invariant down for every entry point.
#[tokio::test]
async fn wave_update_tx_stamps_terminal_at_inside_one_tx() {
    let repo = Arc::new(
        SqlxRepo::open("sqlite::memory:")
            .await
            .expect("open in-memory sqlite"),
    );
    let cove = repo
        .cove_create(NewCove {
            name: "tx-test".into(),
            color: "#000".into(),
            sort: None,
        })
        .await
        .unwrap();
    let wave = seed_wave(&(repo.clone() as Arc<dyn Repo>), cove.id.as_str()).await;
    let done = repo
        .wave_update(
            wave.id.as_str(),
            WavePatch {
                lifecycle: Some(WaveLifecycle::Done),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    assert!(
        done.terminal_at.is_some(),
        "wave_update_tx must stamp terminal_at when lifecycle lands in a terminal state; \
         got {done:?}"
    );
}

// ---------------------------------------------------------------------------
// GET /api/waves window query
// ---------------------------------------------------------------------------

/// Three waves with engineered timestamps cover every branch of the
/// window predicate `created_at <= until AND (terminal_at IS NULL OR
/// terminal_at >= since)`:
///
///   * A — created=1, terminal=2  → terminated *before* the window.
///   * B — created=5, terminal=NULL → open across the window.
///   * C — created=10, terminal=12 → created *after* the window.
///
/// Asking for `since=4, until=8` must include only B. The test forces
/// the timestamps via raw SQL after the kernel mints the rows (the
/// real `now_ms()` would make all three cluster within a millisecond
/// and the window math wouldn't be stable).
#[tokio::test]
async fn list_waves_window_filters_by_created_and_terminal_at() {
    let boot = boot().await;
    let a = seed_wave(&boot.repo, &boot.cove_id).await;
    let b = seed_wave(&boot.repo, &boot.cove_id).await;
    let c = seed_wave(&boot.repo, &boot.cove_id).await;

    // Pin the timestamps via raw SQL. The kernel `wave_create_tx`
    // / `wave_update_tx` always stamp `now_ms()`; for the window
    // predicate test we need stable, separated values that the
    // boundary code never overwrites. Routing through the
    // `SqlxRepo::pool()` accessor keeps the test out of the public
    // trait surface — the production code path is unchanged.
    let pool = boot.sqlx_repo.pool();
    sqlx::query("UPDATE waves SET created_at = ?1, terminal_at = ?2 WHERE id = ?3")
        .bind(1_i64)
        .bind(2_i64)
        .bind(a.id.as_str())
        .execute(pool)
        .await
        .unwrap();
    sqlx::query("UPDATE waves SET created_at = ?1, terminal_at = NULL WHERE id = ?2")
        .bind(5_i64)
        .bind(b.id.as_str())
        .execute(pool)
        .await
        .unwrap();
    sqlx::query("UPDATE waves SET created_at = ?1, terminal_at = ?2 WHERE id = ?3")
        .bind(10_i64)
        .bind(12_i64)
        .bind(c.id.as_str())
        .execute(pool)
        .await
        .unwrap();

    let (status, body) = get(
        boot.app.clone(),
        &format!("/api/waves?since=4&until=8&cove_id={}", boot.cove_id),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body = {body}");
    let arr = body.as_array().expect("array body");
    let ids: Vec<String> = arr
        .iter()
        .map(|w| w.get("id").and_then(Value::as_str).unwrap().to_string())
        .collect();
    assert_eq!(
        ids.len(),
        1,
        "exactly one wave (B) must match since=4&until=8; got ids={ids:?}",
    );
    assert_eq!(ids[0], b.id.to_string());
}

/// `since > until` is a 400.
#[tokio::test]
async fn list_waves_window_inverted_returns_400() {
    let boot = boot().await;
    let (status, body) = get(boot.app.clone(), "/api/waves?since=100&until=50").await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "body = {body}");
}

/// Empty query returns every wave (no filters applied).
#[tokio::test]
async fn list_waves_window_no_params_returns_all_waves() {
    let boot = boot().await;
    seed_wave(&boot.repo, &boot.cove_id).await;
    seed_wave(&boot.repo, &boot.cove_id).await;
    seed_wave(&boot.repo, &boot.other_cove_id).await;

    let (status, body) = get(boot.app.clone(), "/api/waves").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body.as_array().map(|a| a.len()), Some(3));
}

/// `cove_id` alone partitions by cove.
#[tokio::test]
async fn list_waves_window_cove_id_filter() {
    let boot = boot().await;
    seed_wave(&boot.repo, &boot.cove_id).await;
    seed_wave(&boot.repo, &boot.cove_id).await;
    seed_wave(&boot.repo, &boot.other_cove_id).await;

    let (status, body) = get(
        boot.app.clone(),
        &format!("/api/waves?cove_id={}", boot.cove_id),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body.as_array().map(|a| a.len()), Some(2));
}
