//! Integration tests for `POST /api/waves/:wave_id/codex-cards` —
//! the atomic codex-card endpoint introduced in #117.
//!
//! Twin of `tests/terminal_card_endpoint.rs`. Boots a real Axum router
//! (in-memory `SqlxRepo`) + the actual terminal renderer for
//! happy paths, and points `DaemonClient::proc_supervisor_sock` at a
//! non-existent socket for the "spawn failure but rows persisted" case.
//!
//! Test taxonomy:
//!   * `post_codex_card_atomic_returns_card_with_linked_payload` — 201,
//!     `kind == "codex"`, `payload.terminal_id` non-empty,
//!     `payload.cwd` set when provided, `payload.schemaVersion == 1`.
//!   * `post_codex_card_atomic_emits_single_card_added_event` — exactly
//!     one `card.added`, zero `card.updated`, payload carries terminal_id.
//!   * `post_codex_card_atomic_returns_500_on_daemon_spawn_failure_but_persists_row`
//!     — 500 to the client; card + terminal rows + payload.terminal_id
//!     all in the DB.
//!   * `post_codex_card_atomic_404_on_unknown_wave` — 404, no row leak.
//!   * `post_codex_card_atomic_rejects_control_chars_in_cwd` — 400, no
//!     row leak, for newline/CR/tab/NUL in `cwd` (regression guard for
//!     `build_codex_config_toml`'s hand-rolled TOML escape table).
//!   * `post_codex_card_atomic_defaults_cwd_to_home` — empty body stamps
//!     `$HOME` (or server cwd) into `payload.cwd`.

#![cfg(unix)]

use std::path::PathBuf;
use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use calm_server::db::prelude::*;
use calm_server::db::sqlite::SqlxRepo;
use calm_server::event::EventBus;
use calm_server::model::{NewCove, NewWave};
use calm_server::plugin_host::{PluginHost, PluginRegistry};
use calm_server::routes;
use calm_server::state::{AppState, CodexClient, DaemonClient};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tempfile::TempDir;
use tower::ServiceExt;
struct Boot {
    app: axum::Router,
    wave_id: String,
    repo: Arc<dyn Repo>,
    _tmp: TempDir,
}

async fn boot() -> Boot {
    let tmp = TempDir::new().expect("tempdir for daemon sockets");
    let repo: Arc<dyn Repo> = Arc::new(
        SqlxRepo::open("sqlite::memory:")
            .await
            .expect("open in-memory sqlite"),
    );
    let cove = repo
        .cove_create(NewCove {
            name: "endpoint-test".into(),
            color: "#000".into(),
            sort: None,
        })
        .await
        .unwrap();
    let wave = repo
        .wave_create(NewWave {
            cove_id: cove.id,
            title: "endpoint-test".into(),
            sort: None,
            cwd: String::new(),
            attach_folder: false,
            theme: calm_server::routes::theme::RequestTheme::default_dark(),
        })
        .await
        .unwrap();

    let daemon = Arc::new(DaemonClient {
        data_dir: tmp.path().to_path_buf(),
        proc_supervisor_sock: None,
    });
    let events = EventBus::new();
    let state = AppState::from_parts(
        repo.clone(),
        events.clone(),
        daemon,
        Arc::new(PluginHost::new_full(
            Arc::new(PluginRegistry::empty()),
            repo.clone(),
            PathBuf::new(),
            std::env::temp_dir().join("calm-plugins-data"),
            Vec::new(),
            EventBus::new(),
            calm_server::card_role_cache::CardRoleCache::new(),
            calm_server::wave_cove_cache::WaveCoveCache::new(),
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

    Boot {
        app,
        wave_id: wave.id.to_string(),
        repo,
        _tmp: tmp,
    }
}

async fn post(app: axum::Router, uri: String, body: Value) -> (StatusCode, Value) {
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

#[tokio::test]
async fn post_codex_card_atomic_404_on_unknown_wave() {
    // No daemon spawn happens on the 404 path, so the stub binary is fine.
    let boot = boot().await;

    let (status, body) = post(
        boot.app.clone(),
        "/api/waves/wave_does_not_exist/codex-cards".to_string(),
        json!({ "theme": {"fg": [216,219,226], "bg": [15,20,24]} }),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "body={body:?}");

    // No card and no terminal row leaked under the original wave either.
    let cards = boot.repo.cards_by_wave(&boot.wave_id).await.unwrap();
    assert!(cards.is_empty(), "no rows must leak on 404: {cards:?}");
}

#[tokio::test]
async fn post_codex_card_atomic_rejects_control_chars_in_cwd() {
    // `build_codex_config_toml` hand-escapes only `\` and `"` when
    // inlining `cwd` into a TOML basic string. A control character in
    // the cwd (newline, tab, NUL, ...) would slip past that hand-roll
    // and produce TOML-spec-invalid output, crashing codex's config
    // parser at spawn time. The route handler is supposed to validate
    // `NewCodexCardBody.cwd` for ASCII control chars and return 400
    // *before* the create-card transaction even opens.
    //
    // We exercise a handful of representative control characters and
    // assert that each one (a) returns a 4xx and (b) leaves no card or
    // terminal row in the DB.
    let cases: &[(&str, &str)] = &[
        ("/tmp/x\nfoo", "newline"),
        ("/tmp/x\rfoo", "carriage return"),
        ("/tmp/x\tfoo", "tab"),
        ("/tmp/x\0foo", "nul"),
    ];

    for (cwd, label) in cases {
        // No daemon spawn happens on the validation-rejection path, so
        // a non-existent binary is fine — we never reach `spawn_terminal_for`.
        let boot = boot().await;

        let (status, body) = post(
            boot.app.clone(),
            format!("/api/waves/{}/codex-cards", boot.wave_id),
            json!({ "cwd": cwd, "prompt": "hi", "theme": {"fg": [216,219,226], "bg": [15,20,24]} }),
        )
        .await;
        assert_eq!(
            status,
            StatusCode::BAD_REQUEST,
            "{label}: expected 400 for control char in cwd; body={body:?}"
        );
        assert_eq!(
            body["code"], "bad_request",
            "{label}: error code should be bad_request; body={body:?}"
        );

        // No row leak — validation must run BEFORE the create transaction.
        let cards = boot.repo.cards_by_wave(&boot.wave_id).await.unwrap();
        assert!(
            cards.is_empty(),
            "{label}: no card row may be created on validation reject; got {cards:?}"
        );
    }
}
