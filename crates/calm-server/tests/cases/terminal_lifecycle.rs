//! Eager-teardown tests for card/track/area delete: the real route handlers reap a seeded terminal
//! row whose process is a spawned `/bin/sleep`, signalled through the persisted-pid fallback (unix only).

#![cfg(unix)]

use std::process::{Command, Stdio};
use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use calm_server::db::prelude::*;
use calm_server::db::sqlite::SqlxRepo;
use calm_server::event::EventBus;
use calm_server::model::{NewArea, NewCard, NewTerminal, NewTrack};
use calm_server::plugin_host::{PluginHost, PluginRegistry};
use calm_server::routes;
use calm_server::state::{AppState, CodexClient, DaemonClient};
use serde_json::json;
use tower::ServiceExt;

/// An `AppState` backed by an in-memory SQLite repo; no real codex binaries.
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
                calm_server::track_area_cache::TrackAreaCache::new(),
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

fn build_app(state: AppState) -> axum::Router {
    axum::Router::new()
        .merge(routes::cards::router())
        .merge(routes::tracks::router())
        .merge(routes::areas::router())
        .layer(axum::middleware::from_fn(
            calm_server::actor::actor_middleware,
        ))
        .with_state(state)
}

fn spawn_long_running_child() -> std::process::Child {
    Command::new("/bin/sleep")
        .arg("60")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn /bin/sleep")
}

/// The test process is the child's parent, so the exit must be reaped via `try_wait`:
/// `kill(pid, 0)` reports success on zombies and would read as "still alive".
async fn await_child_killed(child: &mut std::process::Child) {
    let pid = child.id();
    for _ in 0..40 {
        match child.try_wait() {
            Ok(Some(status)) => {
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

#[tokio::test]
async fn card_delete_reaps_terminal_process() {
    let state = fresh_state().await;
    let raw = state.raw_repo();

    // Seed: area → track → terminal card → terminal row pointing at a
    // real spawned process.
    let area = raw
        .area_create(NewArea {
            name: "c".into(),
            color: "#000".into(),
            sort: None,
        })
        .await
        .unwrap();
    let track = raw
        .track_create(NewTrack {
            template_input: None,
            area_id: area.id.clone(),
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
            track_id: track.id.clone(),
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

#[tokio::test]
async fn track_delete_refuses_unowned_live_pid_only_terminals() {
    let state = fresh_state().await;
    let raw = state.raw_repo();

    // Seed: a track with TWO terminal cards whose rows contain only raw live pids, so deletion
    // cannot prove either pid still names our child.
    let area = raw
        .area_create(NewArea {
            name: "c".into(),
            color: "#000".into(),
            sort: None,
        })
        .await
        .unwrap();
    let track = raw
        .track_create(NewTrack {
            template_input: None,
            area_id: area.id.clone(),
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
            track_id: track.id.clone(),
            title: None,
            kind: "terminal".into(),
            sort: None,
            payload: json!({}),
        })
        .await
        .unwrap();
    let card_b = raw
        .card_create(NewCard {
            track_id: track.id.clone(),
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

    // DELETE /api/tracks/{id}
    let app = build_app(state.clone());
    let resp = app
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri(format!("/api/tracks/{}", track.id.as_str()))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::INTERNAL_SERVER_ERROR);

    // Fail closed: no bare-pid TERM/KILL, and no ownership rows disappear.
    assert!(child_a.try_wait().unwrap().is_none());
    assert!(child_b.try_wait().unwrap().is_none());
    assert!(state.repo.terminal_get(&term_a.id).await.unwrap().is_some());
    assert!(state.repo.terminal_get(&term_b.id).await.unwrap().is_some());
    assert!(
        state
            .repo
            .card_get(card_a.id.as_str())
            .await
            .unwrap()
            .is_some()
    );
    assert!(
        state
            .repo
            .card_get(card_b.id.as_str())
            .await
            .unwrap()
            .is_some()
    );
    assert!(
        state
            .repo
            .track_get(track.id.as_str())
            .await
            .unwrap()
            .is_some()
    );
    child_a.kill().unwrap();
    child_b.kill().unwrap();
    let _ = child_a.wait();
    let _ = child_b.wait();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn track_delete_external_teardown_does_not_hold_the_sqlite_writer() {
    let (state, repo) = fresh_state_with_repo().await;
    let area = repo
        .area_create(NewArea {
            name: "writer-probe-owner".into(),
            color: "#000".into(),
            sort: None,
        })
        .await
        .unwrap();
    let track = repo
        .track_create(NewTrack {
            template_input: None,
            area_id: area.id,
            title: "writer-probe-track".into(),
            sort: None,
            cwd: String::new(),
            template_id: None,
            plugin_scope: None,
            attach_folder: false,
            theme: calm_server::routes::theme::RequestTheme::default_dark(),
        })
        .await
        .unwrap();
    let hook = calm_server::routes::tracks::TrackDeleteTeardownHook {
        entered: Arc::new(tokio::sync::Notify::new()),
        release: Arc::new(tokio::sync::Notify::new()),
    };
    calm_server::routes::tracks::install_track_delete_teardown_hook_for_test(
        track.id.as_str(),
        hook.clone(),
    );
    let app = build_app(state);
    let track_id = track.id.to_string();
    let deleting = tokio::spawn(async move {
        app.oneshot(
            Request::builder()
                .method("DELETE")
                .uri(format!("/api/tracks/{track_id}"))
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
        repo.area_create(NewArea {
            name: "writer-probe-unrelated".into(),
            color: "#111".into(),
            sort: None,
        }),
    )
    .await
    .expect("unrelated writer was blocked by external track teardown")
    .expect("unrelated writer must succeed");

    hook.release.notify_one();
    let response = tokio::time::timeout(Duration::from_secs(5), deleting)
        .await
        .expect("track delete must finish after teardown release")
        .unwrap();
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
}

#[tokio::test]
async fn area_delete_refuses_an_unowned_live_pid_only_terminal() {
    let state = fresh_state().await;
    let raw = state.raw_repo();

    // A raw live pid without a renderer-owned proc_id or persisted identity
    // tuple cannot safely be signaled: the OS may already have recycled it.
    let area = raw
        .area_create(NewArea {
            name: "c".into(),
            color: "#000".into(),
            sort: None,
        })
        .await
        .unwrap();
    let track = raw
        .track_create(NewTrack {
            template_input: None,
            area_id: area.id.clone(),
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
            track_id: track.id.clone(),
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

    // DELETE /api/areas/{id}
    let app = build_app(state.clone());
    let resp = app
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri(format!("/api/areas/{}", area.id.as_str()))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::INTERNAL_SERVER_ERROR);
    assert!(child.try_wait().unwrap().is_none());
    assert!(
        state.repo.terminal_get(&term.id).await.unwrap().is_some(),
        "terminal row must survive a refused area delete"
    );
    assert!(
        state
            .repo
            .card_get(card.id.as_str())
            .await
            .unwrap()
            .is_some(),
        "card row must survive a refused area delete"
    );
    assert!(
        state
            .repo
            .track_get(track.id.as_str())
            .await
            .unwrap()
            .is_some(),
        "track row must survive a refused area delete"
    );
    assert!(
        state
            .repo
            .area_get(area.id.as_str())
            .await
            .unwrap()
            .is_some(),
        "area row must survive a refused delete"
    );
    child.kill().unwrap();
    let _ = child.wait();
}

#[tokio::test]
async fn card_delete_succeeds_when_card_has_no_terminal() {
    // Non-terminal cards must still delete cleanly — eager
    // teardown must not bail when `terminal_get_by_card` returns None.
    let state = fresh_state().await;
    let raw = state.raw_repo();
    let area = raw
        .area_create(NewArea {
            name: "c".into(),
            color: "#000".into(),
            sort: None,
        })
        .await
        .unwrap();
    let track = raw
        .track_create(NewTrack {
            template_input: None,
            area_id: area.id.clone(),
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
            track_id: track.id.clone(),
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
