//! Issue #197 — eager-teardown regression tests for card/wave/cove
//! delete.
//!
//! Unix-only: SIGTERM via `nix::sys::signal::kill` is the cleanup
//! helper's enforcement lever; on non-unix the helper is a no-op and
//! these tests don't carry signal.
//!
//! Pre-#197 the lifecycle leaked: `terminals.card_id` was
//! `ON DELETE CASCADE`, so the FK quietly nuked the terminal row when
//! its card was deleted — but the terminal process lived
//! on until the orphan sweeper caught them ~30-60 s later (or never,
//! if the server restarted in between).
//!
//! Post-#197 the FK is `ON DELETE RESTRICT` (migration 0011) and the
//! route handlers own the synchronous teardown:
//! `terminal_sweeper::reap_terminal_artifacts` is called for every
//! terminal under the entity being deleted, *before* the row delete
//! fires.
//!
//! These tests drive the real route handlers via `tower::ServiceExt::oneshot`,
//! seed a terminal row with a real spawned child process, then assert
//! post-delete that:
//!   * the child process is gone (waited away or signalled away),
//!   * the terminal row is removed from the DB.
//!
//! The child we spawn is `/bin/sleep` (POSIX guaranteed) — we never
//! stand up a terminal renderer, just a long-running process that the
//! cleanup helper can SIGTERM via the persisted pid fallback.

#![cfg(unix)]

use std::process::{Command, Stdio};
use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use calm_server::db::prelude::*;
use calm_server::db::sqlite::SqlxRepo;
use calm_server::event::EventBus;
use calm_server::model::{NewCard, NewCove, NewTerminal, NewWave};
use calm_server::plugin_host::{PluginHost, PluginRegistry};
use calm_server::routes;
use calm_server::state::{AppState, CodexClient, DaemonClient};
use serde_json::json;
use tower::ServiceExt;

/// Spin up an `AppState` backed by an in-memory SQLite repo, mirroring
/// the existing test fixtures (`payload_validation.rs`,
/// `terminal_sweeper.rs`). No real codex binaries — we never
/// invoke them in this file.
fn state_from_repo(repo: Arc<dyn Repo>) -> AppState {
    AppState::from_parts(
        repo.clone(),
        EventBus::new(),
        Arc::new(DaemonClient::new_stub()),
        Arc::new(PluginHost::new_full(
            Arc::new(PluginRegistry::empty()),
            repo,
            std::path::PathBuf::new(),
            std::env::temp_dir().join("calm-plugins-data"),
            Vec::new(),
            EventBus::new(),
            calm_server::state::WriteContext::new(
                calm_server::card_role_cache::CardRoleCache::new(),
                calm_server::wave_cove_cache::WaveCoveCache::new(),
            ),
        )),
        Arc::new(CodexClient::new_stub()),
        None,
        None,
    )
}

async fn fresh_state_with_repo() -> (AppState, Arc<dyn Repo>) {
    let repo: Arc<dyn Repo> = Arc::new(
        SqlxRepo::open("sqlite::memory:")
            .await
            .expect("open in-memory sqlite repo"),
    );
    (state_from_repo(repo.clone()), repo)
}

async fn fresh_state() -> AppState {
    fresh_state_with_repo().await.0
}

/// Compose a minimal Axum app with the cards + waves + coves routers
/// + the `actor_middleware` that the handlers depend on. Same shape
///   as `payload_validation.rs::app`.
fn build_app(state: AppState) -> axum::Router {
    axum::Router::new()
        .merge(routes::cards::router())
        .merge(routes::waves::router())
        .merge(routes::coves::router())
        .layer(axum::middleware::from_fn(
            calm_server::actor::actor_middleware,
        ))
        .with_state(state)
}

/// Spawn a long-running child process and return its pid + a JoinHandle
/// the test can use to confirm reap. `/bin/sleep 60` is the lightest
/// process that won't exit on its own inside the test budget.
fn spawn_long_running_child() -> std::process::Child {
    Command::new("/bin/sleep")
        .arg("60")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn /bin/sleep")
}

/// Confirm that a child process spawned by the test has been killed by
/// the cleanup helper. Since the test process is the parent of the
/// child, we have to reap via `try_wait` ourselves to see the exit —
/// `kill(pid, 0)` reports success on zombies and would yield a false
/// "still alive" reading.
///
/// Up to ~2 s of polling. The cleanup helper's SIGTERM is delivered
/// synchronously inside the route handler, but the kernel's process
/// teardown is asynchronous; `/bin/sleep` typically exits within
/// single-digit ms of receiving SIGTERM.
async fn await_child_killed(child: &mut std::process::Child) {
    let pid = child.id();
    for _ in 0..40 {
        match child.try_wait() {
            Ok(Some(status)) => {
                // Sanity: `/bin/sleep` killed by SIGTERM exits with no
                // status code — the unix exit-status carries the
                // signal. We just want non-None.
                let _ = status;
                return;
            }
            Ok(None) => {
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
            Err(e) => panic!("try_wait(pid={pid}) failed: {e}"),
        }
    }
    let _ = child.kill();
    let _ = child.wait();
    panic!("pid {pid} was still alive after 2s (force-killed by fixture cleanup)");
}

// ---------------------------------------------------------------------------
// Card delete eager teardown
// ---------------------------------------------------------------------------

#[tokio::test]
async fn card_delete_reaps_terminal_process() {
    let state = fresh_state().await;
    let raw = state.raw_repo();

    // Seed: cove → wave → terminal card → terminal row pointing at a
    // real spawned process.
    let cove = raw
        .cove_create(NewCove {
            name: "c".into(),
            color: "#000".into(),
            sort: None,
        })
        .await
        .unwrap();
    let wave = raw
        .wave_create(NewWave {
            template_input: None,
            cove_id: cove.id.clone(),
            title: "w".into(),
            sort: None,
            cwd: String::new(),
            template_id: None,
            plugin_scope: None,
            attach_folder: false,
            theme: calm_server::routes::theme::RequestTheme::default_dark(),
        })
        .await
        .unwrap();
    let card = raw
        .card_create(NewCard {
            wave_id: wave.id.clone(),
            title: None,
            kind: "terminal".into(),
            sort: None,
            payload: json!({}),
        })
        .await
        .unwrap();
    let term = state
        .repo
        .terminal_create(NewTerminal {
            card_id: card.id.clone(),
            program: "/bin/true".into(),
            cwd: "/tmp".into(),
            env: json!({}),
            theme: calm_server::routes::theme::RequestTheme::default_dark(),
        })
        .await
        .unwrap();
    let mut child = spawn_long_running_child();
    state
        .repo
        .terminal_set_pid(&term.id, Some(child.id()))
        .await
        .unwrap();

    // Sanity: pid is alive and terminal row exists.
    assert!(
        state.repo.terminal_get(&term.id).await.unwrap().is_some(),
        "terminal row exists pre-delete"
    );

    // DELETE /api/cards/{id}
    let app = build_app(state.clone());
    let resp = app
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri(format!("/api/cards/{}", card.id.as_str()))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::NO_CONTENT,
        "card delete should return 204"
    );

    // Post-delete: process gone and row removed.
    await_child_killed(&mut child).await;
    assert!(
        state.repo.terminal_get(&term.id).await.unwrap().is_none(),
        "terminal row must be deleted with the card"
    );
    assert!(
        state
            .repo
            .card_get(card.id.as_str())
            .await
            .unwrap()
            .is_none(),
        "card row must be deleted"
    );
}

// ---------------------------------------------------------------------------
// Wave delete eager teardown
// ---------------------------------------------------------------------------

#[tokio::test]
async fn wave_delete_reaps_every_terminal_under_wave() {
    let state = fresh_state().await;
    let raw = state.raw_repo();

    // Seed: cove → wave with TWO terminal cards, each with a live child.
    // The wave-delete path must reap both.
    let cove = raw
        .cove_create(NewCove {
            name: "c".into(),
            color: "#000".into(),
            sort: None,
        })
        .await
        .unwrap();
    let wave = raw
        .wave_create(NewWave {
            template_input: None,
            cove_id: cove.id.clone(),
            title: "w".into(),
            sort: None,
            cwd: String::new(),
            template_id: None,
            plugin_scope: None,
            attach_folder: false,
            theme: calm_server::routes::theme::RequestTheme::default_dark(),
        })
        .await
        .unwrap();

    let card_a = raw
        .card_create(NewCard {
            wave_id: wave.id.clone(),
            title: None,
            kind: "terminal".into(),
            sort: None,
            payload: json!({}),
        })
        .await
        .unwrap();
    let card_b = raw
        .card_create(NewCard {
            wave_id: wave.id.clone(),
            title: None,
            kind: "terminal".into(),
            sort: None,
            payload: json!({}),
        })
        .await
        .unwrap();

    let term_a = state
        .repo
        .terminal_create(NewTerminal {
            card_id: card_a.id.clone(),
            program: "/bin/true".into(),
            cwd: "/tmp".into(),
            env: json!({}),
            theme: calm_server::routes::theme::RequestTheme::default_dark(),
        })
        .await
        .unwrap();
    let term_b = state
        .repo
        .terminal_create(NewTerminal {
            card_id: card_b.id.clone(),
            program: "/bin/true".into(),
            cwd: "/tmp".into(),
            env: json!({}),
            theme: calm_server::routes::theme::RequestTheme::default_dark(),
        })
        .await
        .unwrap();

    let mut child_a = spawn_long_running_child();
    let mut child_b = spawn_long_running_child();
    state
        .repo
        .terminal_set_pid(&term_a.id, Some(child_a.id()))
        .await
        .unwrap();
    state
        .repo
        .terminal_set_pid(&term_b.id, Some(child_b.id()))
        .await
        .unwrap();

    // DELETE /api/waves/{id}
    let app = build_app(state.clone());
    let resp = app
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri(format!("/api/waves/{}", wave.id.as_str()))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::NO_CONTENT,
        "wave delete should return 204"
    );

    // Post-delete: both processes gone, both
    // terminal rows + both card rows + the wave row removed.
    await_child_killed(&mut child_a).await;
    await_child_killed(&mut child_b).await;
    assert!(state.repo.terminal_get(&term_a.id).await.unwrap().is_none());
    assert!(state.repo.terminal_get(&term_b.id).await.unwrap().is_none());
    assert!(
        state
            .repo
            .card_get(card_a.id.as_str())
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        state
            .repo
            .card_get(card_b.id.as_str())
            .await
            .unwrap()
            .is_none()
    );
    assert!(
        state
            .repo
            .wave_get(wave.id.as_str())
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn wave_delete_external_teardown_does_not_hold_the_sqlite_writer() {
    let (state, repo) = fresh_state_with_repo().await;
    let cove = repo
        .cove_create(NewCove {
            name: "writer-probe-owner".into(),
            color: "#000".into(),
            sort: None,
        })
        .await
        .unwrap();
    let wave = repo
        .wave_create(NewWave {
            template_input: None,
            cove_id: cove.id,
            title: "writer-probe-wave".into(),
            sort: None,
            cwd: String::new(),
            template_id: None,
            plugin_scope: None,
            attach_folder: false,
            theme: calm_server::routes::theme::RequestTheme::default_dark(),
        })
        .await
        .unwrap();
    let hook = calm_server::routes::waves::WaveDeleteTeardownHook {
        entered: Arc::new(tokio::sync::Notify::new()),
        release: Arc::new(tokio::sync::Notify::new()),
    };
    calm_server::routes::waves::install_wave_delete_teardown_hook_for_test(
        wave.id.as_str(),
        hook.clone(),
    );
    let app = build_app(state);
    let wave_id = wave.id.to_string();
    let deleting = tokio::spawn(async move {
        app.oneshot(
            Request::builder()
                .method("DELETE")
                .uri(format!("/api/waves/{wave_id}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap()
    });

    tokio::time::timeout(Duration::from_secs(1), hook.entered.notified())
        .await
        .expect("delete must reach external teardown without holding the writer");

    tokio::time::timeout(
        Duration::from_millis(250),
        repo.cove_create(NewCove {
            name: "writer-probe-unrelated".into(),
            color: "#111".into(),
            sort: None,
        }),
    )
    .await
    .expect("unrelated writer was blocked by external wave teardown")
    .expect("unrelated writer must succeed");

    hook.release.notify_one();
    let response = tokio::time::timeout(Duration::from_secs(5), deleting)
        .await
        .expect("wave delete must finish after teardown release")
        .unwrap();
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
}

// ---------------------------------------------------------------------------
// Cove delete eager teardown
// ---------------------------------------------------------------------------

#[tokio::test]
async fn cove_delete_reaps_every_terminal_under_cove() {
    let state = fresh_state().await;
    let raw = state.raw_repo();

    // Seed: cove → wave → terminal card → terminal row pointing at a
    // real spawned process. The cove-delete
    // path walks waves → cards → terminals and must reap the terminal
    // before the structural delete fires (else `terminals.card_id`'s
    // RESTRICT FK would trip).
    let cove = raw
        .cove_create(NewCove {
            name: "c".into(),
            color: "#000".into(),
            sort: None,
        })
        .await
        .unwrap();
    let wave = raw
        .wave_create(NewWave {
            template_input: None,
            cove_id: cove.id.clone(),
            title: "w".into(),
            sort: None,
            cwd: String::new(),
            template_id: None,
            plugin_scope: None,
            attach_folder: false,
            theme: calm_server::routes::theme::RequestTheme::default_dark(),
        })
        .await
        .unwrap();
    let card = raw
        .card_create(NewCard {
            wave_id: wave.id.clone(),
            title: None,
            kind: "terminal".into(),
            sort: None,
            payload: json!({}),
        })
        .await
        .unwrap();
    let term = state
        .repo
        .terminal_create(NewTerminal {
            card_id: card.id.clone(),
            program: "/bin/true".into(),
            cwd: "/tmp".into(),
            env: json!({}),
            theme: calm_server::routes::theme::RequestTheme::default_dark(),
        })
        .await
        .unwrap();
    let mut child = spawn_long_running_child();
    state
        .repo
        .terminal_set_pid(&term.id, Some(child.id()))
        .await
        .unwrap();
    assert!(
        state.repo.terminal_get(&term.id).await.unwrap().is_some(),
        "terminal row exists pre-delete"
    );

    // DELETE /api/coves/{id}
    let app = build_app(state.clone());
    let resp = app
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri(format!("/api/coves/{}", cove.id.as_str()))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::NO_CONTENT,
        "cove delete should return 204"
    );

    // Post-delete: child process gone and every row
    // in the terminal/card/wave/cove subtree removed.
    await_child_killed(&mut child).await;
    assert!(
        state.repo.terminal_get(&term.id).await.unwrap().is_none(),
        "terminal row must be deleted with the cove"
    );
    assert!(
        state
            .repo
            .card_get(card.id.as_str())
            .await
            .unwrap()
            .is_none(),
        "card row must be deleted with the cove"
    );
    assert!(
        state
            .repo
            .wave_get(wave.id.as_str())
            .await
            .unwrap()
            .is_none(),
        "wave row must be deleted with the cove"
    );
    assert!(
        state
            .repo
            .cove_get(cove.id.as_str())
            .await
            .unwrap()
            .is_none(),
        "cove row must be deleted"
    );
}

// ---------------------------------------------------------------------------
// Idempotency: card delete on a card that has no terminal
// ---------------------------------------------------------------------------

#[tokio::test]
async fn card_delete_succeeds_when_card_has_no_terminal() {
    // Non-terminal cards must still delete cleanly — eager
    // teardown must not bail when `terminal_get_by_card` returns None.
    let state = fresh_state().await;
    let raw = state.raw_repo();
    let cove = raw
        .cove_create(NewCove {
            name: "c".into(),
            color: "#000".into(),
            sort: None,
        })
        .await
        .unwrap();
    let wave = raw
        .wave_create(NewWave {
            template_input: None,
            cove_id: cove.id.clone(),
            title: "w".into(),
            sort: None,
            cwd: String::new(),
            template_id: None,
            plugin_scope: None,
            attach_folder: false,
            theme: calm_server::routes::theme::RequestTheme::default_dark(),
        })
        .await
        .unwrap();
    let card = raw
        .card_create(NewCard {
            wave_id: wave.id.clone(),
            title: None,
            kind: "ui://plugin/foo".into(),
            sort: None,
            payload: json!({}),
        })
        .await
        .unwrap();

    let app = build_app(state.clone());
    let resp = app
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri(format!("/api/cards/{}", card.id.as_str()))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NO_CONTENT);
    assert!(
        state
            .repo
            .card_get(card.id.as_str())
            .await
            .unwrap()
            .is_none()
    );
}
