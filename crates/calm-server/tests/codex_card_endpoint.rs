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
use std::time::Duration;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use calm_server::db::prelude::*;
use calm_server::db::sqlite::SqlxRepo;
use calm_server::event::{BroadcastEnvelope, Event, EventBus};
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
    events: EventBus,
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
        events,
        repo,
        _tmp: tmp,
    }
}

async fn boot_happy() -> Boot {
    boot().await
}

/// #388 Phase 3b: drive a deliberate spawn failure by pointing
/// `proc_supervisor_sock` at a path no supervisor is listening on. The
/// renderer's connect-to-supervisor returns NotFound and propagates
/// the error through the route as 500.
async fn boot_with_bad_supervisor(bad_sock: PathBuf) -> Boot {
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
        proc_supervisor_sock: Some(bad_sock),
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
        events,
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
async fn post_codex_card_atomic_returns_card_with_linked_payload() {
    let boot = boot_happy().await;

    let (status, card) = post(
        boot.app.clone(),
        format!("/api/waves/{}/codex-cards", boot.wave_id),
        json!({ "cwd": "/workspace", "sort": 1.0, "theme": {"fg": [216,219,226], "bg": [15,20,24]} }),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "body={card:?}");
    assert_eq!(card["kind"], "codex", "card kind: {card:?}");
    let terminal_id = card["payload"]["terminal_id"]
        .as_str()
        .expect("payload.terminal_id is a string")
        .to_string();
    assert!(
        !terminal_id.is_empty(),
        "payload.terminal_id non-empty: {card:?}"
    );
    assert_eq!(
        card["payload"]["cwd"], "/workspace",
        "payload.cwd echoed: {card:?}"
    );
    assert_eq!(
        card["payload"]["schemaVersion"], 1,
        "payload.schemaVersion stamped: {card:?}"
    );

    // The linked terminal row is parented to the new card.
    let term = boot
        .repo
        .terminal_get(&terminal_id)
        .await
        .unwrap()
        .expect("terminal row persists");
    assert_eq!(term.card_id.as_str(), card["id"].as_str().unwrap());
    assert_eq!(term.program, "codex", "program is hardwired to codex");
    assert_eq!(term.cwd, "/workspace");
}

#[tokio::test]
async fn post_codex_card_atomic_emits_single_card_added_event() {
    let boot = boot_happy().await;
    let mut rx = boot.events.subscribe();

    let (status, _card) = post(
        boot.app.clone(),
        format!("/api/waves/{}/codex-cards", boot.wave_id),
        json!({ "cwd": "/workspace", "sort": 1.0, "theme": {"fg": [216,219,226], "bg": [15,20,24]} }),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED);

    // Drain over a short window. Expectation: exactly ONE card.added
    // (carrying the fully-stamped payload) and ZERO card.updated frames.
    // The old 2-step recipe emitted card.added (payload=null) followed
    // by card.updated (payload={terminal_id, cwd}); the atomic endpoint
    // collapses both into a single broadcast.
    let mut added: Vec<BroadcastEnvelope> = Vec::new();
    let mut updated_count = 0usize;
    let deadline = tokio::time::Instant::now() + Duration::from_millis(200);
    loop {
        let now = tokio::time::Instant::now();
        if now >= deadline {
            break;
        }
        let remaining = deadline - now;
        match tokio::time::timeout(remaining, rx.recv()).await {
            Ok(Ok(env)) => match &env.event {
                Event::CardAdded(_) => added.push(env),
                Event::CardUpdated(_) => updated_count += 1,
                _ => {}
            },
            Ok(Err(_)) => break,
            Err(_) => break,
        }
    }
    assert_eq!(
        added.len(),
        1,
        "exactly one card.added; got {} added + {} updated",
        added.len(),
        updated_count
    );
    assert_eq!(
        updated_count, 0,
        "no card.updated allowed; got {updated_count}"
    );
    let env = added.into_iter().next().unwrap();
    match env.event {
        Event::CardAdded(card) => {
            assert_eq!(card.kind, "codex");
            assert!(
                card.payload
                    .get("terminal_id")
                    .and_then(|v| v.as_str())
                    .is_some(),
                "card.added event payload must carry terminal_id: {:?}",
                card.payload
            );
        }
        other => panic!("expected CardAdded, got {other:?}"),
    }
}

#[tokio::test]
async fn post_codex_card_atomic_returns_500_on_daemon_spawn_failure_but_persists_row() {
    // #388 Phase 3b: production spawn now goes through
    // `calm-proc-supervisor` over a control UDS instead of forking the
    // terminal renderer directly. To deliberately fail startup,
    // point `proc_supervisor_sock` at a non-existent path so the
    // renderer's connect to the supervisor fails with NotFound.
    let bad_sock = std::env::temp_dir().join("definitely-not-a-real-proc-supervisor-sock-xyz");
    let _ = std::fs::remove_file(&bad_sock);
    let boot = boot_with_bad_supervisor(bad_sock).await;

    let (status, body) = post(
        boot.app.clone(),
        format!("/api/waves/{}/codex-cards", boot.wave_id),
        json!({ "cwd": "/workspace", "theme": {"fg": [216,219,226], "bg": [15,20,24]} }),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::INTERNAL_SERVER_ERROR,
        "expected 500 on daemon spawn failure: {body:?}"
    );

    // Even though spawn failed, the card + terminal row must remain in
    // the DB and the payload.terminal_id must be stamped — the orphan
    // sweeper is responsible for cleanup.
    let cards = boot.repo.cards_by_wave(&boot.wave_id).await.unwrap();
    assert_eq!(
        cards.len(),
        1,
        "exactly one card persisted; got {}",
        cards.len()
    );
    let card = &cards[0];
    assert_eq!(card.kind, "codex");
    let terminal_id = card.payload["terminal_id"]
        .as_str()
        .expect("payload.terminal_id stamped before spawn attempt");
    let term = boot
        .repo
        .terminal_get(terminal_id)
        .await
        .unwrap()
        .expect("terminal row persisted despite spawn failure");
    assert_eq!(term.card_id, card.id);
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

#[tokio::test]
async fn post_codex_card_atomic_defaults_cwd_to_home() {
    let boot = boot_happy().await;

    let (status, card) = post(
        boot.app.clone(),
        format!("/api/waves/{}/codex-cards", boot.wave_id),
        // Only required field (#177): theme. Every other field falls
        // back to its default (cwd → $HOME, prompt → user-initiated).
        json!({ "theme": {"fg": [216,219,226], "bg": [15,20,24]} }),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "body={card:?}");
    let terminal_id = card["payload"]["terminal_id"]
        .as_str()
        .expect("payload.terminal_id is a string");
    let term = boot
        .repo
        .terminal_get(terminal_id)
        .await
        .unwrap()
        .expect("terminal row exists after create");

    // Match `routes::codex_cards::default_cwd`: $HOME if set + non-empty,
    // else server cwd. Both endpoint and test resolve at the same moment
    // in the same process so the values agree.
    let expected = std::env::var("HOME")
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| {
            std::env::current_dir()
                .unwrap_or_default()
                .to_string_lossy()
                .to_string()
        });
    assert_eq!(term.cwd, expected, "default cwd: {:?}", term.cwd);
    assert_eq!(
        card["payload"]["cwd"], expected,
        "payload.cwd echoes terminal.cwd: {card:?}"
    );
}
