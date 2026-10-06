//! #2228 — the workspace re-point restarts the Planner under the card's `planner_recovery_locks`,
//! as `/planner/reset` does, so a send waits for that restart instead of racing it.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use axum::body::Body;
use axum::http::{Request, StatusCode};
use calm_server::db::prelude::*;
use calm_server::model::{NewArea, new_id};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tower::ServiceExt;

use crate::planner_first_start::{AppServer, Boot, app_state, post_input, router};

async fn request(
    app: axum::Router,
    method: &'static str,
    uri: String,
    body: Value,
) -> (StatusCode, Value) {
    let response = app
        .oneshot(
            Request::builder()
                .method(method)
                .uri(uri)
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

fn git(at: &Path, args: &[&str]) {
    let status = Command::new("git")
        .args(["-c", "user.name=t", "-c", "user.email=t@example.com"])
        .args(args)
        .current_dir(at)
        .status()
        .unwrap();
    assert!(status.success(), "git {args:?} in {at:?}");
}

/// A repository the person already has: the re-point's target.
fn user_repo(at: &Path) -> PathBuf {
    std::fs::create_dir_all(at).unwrap();
    git(at, &["init", "-q", "-b", "main"]);
    git(at, &["config", "gc.auto", "0"]);
    std::fs::write(at.join("README.md"), b"the user's own work\n").unwrap();
    git(at, &["add", "-A"]);
    git(at, &["commit", "-q", "--no-verify", "-m", "user commit"]);
    at.to_path_buf()
}

async fn wait_until(what: &str, mut done: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !done() {
        assert!(Instant::now() < deadline, "timed out: {what}");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

/// The re-point's restart is held in `thread/start` while it holds the card's lock; a send to the
/// card is shown queued on that lock, and once the restart lands the send goes to the restarted
/// session rather than racing the start.
#[tokio::test]
async fn a_send_during_the_repoint_restart_waits_on_the_cards_lock() {
    let (tmp, repo, state) = app_state(AppServer::Running).await;
    let area = repo
        .area_create(NewArea {
            name: "repoint".into(),
            color: "#000".into(),
            sort: None,
        })
        .await
        .unwrap();
    let app = router(&state);
    let (status, track) = request(
        app.clone(),
        "POST",
        "/api/tracks".into(),
        json!({
            "planner_provider": "codex",
            "area_id": area.id.as_str(),
            "title": "moving",
            "theme": {"fg": [216, 219, 226], "bg": [15, 20, 24]},
        }),
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "body={track}");
    let track_id = track["id"].as_str().unwrap().to_string();
    let planner_card_id: String =
        sqlx::query_scalar("SELECT id FROM cards WHERE track_id = ?1 AND role = 'planner'")
            .bind(&track_id)
            .fetch_one(repo.pool())
            .await
            .unwrap();
    let target = user_repo(&tmp.path().join("my-project"));
    let boot = Boot {
        app,
        state,
        repo,
        planner_card_id,
        _tmp: tmp,
    };
    let card_id = boot.planner_card_id.clone();
    assert_eq!(boot.start_ops().await, 1, "premise: the create's own start");
    assert_eq!(
        boot.state.planner_recovery_lock_handles_for_test(&card_id),
        0,
        "premise: nothing holds the card's lock"
    );

    let thread_start = boot
        .state
        .shared_codex_appserver
        .lock_thread_start_serial_for_test()
        .await;
    let repoint = tokio::spawn(request(
        boot.app.clone(),
        "PATCH",
        format!("/api/tracks/{track_id}"),
        json!({"workspace": {
            "kind": "attached",
            "path": target.to_string_lossy(),
            "attach_folder": true,
        }}),
    ));
    let deadline = Instant::now() + Duration::from_secs(10);
    while boot.start_ops().await < 2 {
        assert!(
            Instant::now() < deadline,
            "the re-point never submitted its restart"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let held = boot.state.planner_recovery_lock_handles_for_test(&card_id);
    assert!(
        held > 0,
        "the re-point submitted its restart without holding the card's lock"
    );

    let send = {
        let (app, uri) = (boot.app.clone(), boot.input_uri());
        tokio::spawn(async move { post_input(app, &uri, "after the move", &new_id()).await })
    };
    wait_until("the send queues on the card's lock", || {
        boot.state.planner_recovery_lock_handles_for_test(&card_id) > held
    })
    .await;
    assert!(!send.is_finished(), "the send waits behind the restart");
    assert!(
        !repoint.is_finished(),
        "the restart is still in thread/start"
    );

    drop(thread_start);
    let (status, body) = repoint.await.unwrap();
    assert_eq!(status, StatusCode::OK, "body={body}");
    let (status, body) = send.await.unwrap();
    assert_eq!(status, StatusCode::OK, "body={body}");
    let active = boot
        .repo
        .session_projection_active_for_card(&card_id)
        .await
        .unwrap()
        .expect("the restart left an active session");
    assert_eq!(
        body["worker_session_id"],
        json!(active.id),
        "the send went to the restarted session"
    );
    assert_eq!(boot.start_ops().await, 2, "the send started nothing itself");
    assert_eq!(boot.active_session_rows().await, 1);
    boot.wait_delivered("after the move").await;
    boot.shutdown().await;
}
