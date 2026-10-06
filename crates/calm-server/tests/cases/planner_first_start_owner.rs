//! #2184 — a send starts a card only when no creator can start it any more. An ordinary create,
//! its keyed retry and the launchpad's ensure each own their Planner's start, and a send that
//! started a session first would have that session superseded by theirs.

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use calm_server::db::prelude::*;
use calm_server::model::{NewArea, new_id};
use calm_server::test_seams::{
    PausePoint, TRACK_CREATE_BEFORE_PLANNER_START, install_pause_for_test,
};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tokio::sync::Notify;
use tower::ServiceExt;

use crate::planner_first_start::{AppServer, Boot, app_state, post_input, router, within};

/// `POST /api/tracks`, message-less: the production create, whose Planner start is its own.
async fn create_ordinary_track(
    app: axum::Router,
    area_id: String,
    title: &'static str,
) -> (StatusCode, Value) {
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/tracks")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({
                        "planner_provider": "codex",
                        "area_id": area_id,
                        "title": title,
                        "cwd": null,
                        "attach_folder": false,
                        "theme": {"fg": [216, 219, 226], "bg": [15, 20, 24]},
                    })
                    .to_string(),
                ))
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

/// Codex review (#2184): an ordinary create commits its cards, then submits its own start. A send
/// in that window must not start the card, or the create's start would supersede the session the
/// send's message went to. The never-started card of a non-managed Track stays 409.
#[tokio::test]
async fn a_send_between_a_creates_commit_and_its_start_does_not_pre_empt_the_creator() {
    let (tmp, repo, state) = app_state(AppServer::Running).await;
    let area = repo
        .area_create(NewArea {
            name: "racing create".into(),
            color: "#000".into(),
            sort: None,
        })
        .await
        .unwrap();
    let app = router(&state);
    let parked = PausePoint {
        entered: Arc::new(Notify::new()),
        release: Arc::new(Notify::new()),
    };
    install_pause_for_test(
        TRACK_CREATE_BEFORE_PLANNER_START,
        area.id.as_str(),
        parked.clone(),
    );
    let create = tokio::spawn(create_ordinary_track(
        app.clone(),
        area.id.to_string(),
        "racing",
    ));
    within(
        parked.entered.notified(),
        "the create parks before its start",
    )
    .await;
    let planner_card_id: String = sqlx::query_scalar(
        "SELECT c.id FROM cards c JOIN tracks t ON t.id = c.track_id \
          WHERE t.area_id = ?1 AND c.role = 'planner'",
    )
    .bind(area.id.as_str())
    .fetch_one(repo.pool())
    .await
    .expect("the create committed its Planner card before its start");
    let boot = Boot {
        app,
        state,
        repo,
        planner_card_id,
        _tmp: tmp,
    };

    let (status, body) =
        post_input(boot.app.clone(), &boot.input_uri(), "too soon", &new_id()).await;
    assert_eq!(status, StatusCode::CONFLICT, "body={body}");
    assert_eq!(
        body["code"],
        json!("planner_harness_dormant"),
        "body={body}"
    );
    assert_eq!(boot.start_ops().await, 0, "the send submitted no start");
    assert_eq!(boot.session_rows().await, 0);

    parked.release.notify_one();
    let (status, created) = create.await.unwrap();
    assert!(status.is_success(), "{status} {created}");
    let active = boot
        .repo
        .session_projection_active_for_card(&boot.planner_card_id)
        .await
        .unwrap()
        .expect("the create's own start ran");

    let (status, body) = post_input(boot.app.clone(), &boot.input_uri(), "now", &new_id()).await;
    assert_eq!(status, StatusCode::OK, "body={body}");
    assert_eq!(
        body["worker_session_id"],
        json!(active.id),
        "the creator's session"
    );
    assert_eq!(boot.start_ops().await, 1, "one start: the creator's");
    assert_eq!(boot.active_session_rows().await, 1);
    boot.wait_delivered("now").await;
    boot.shutdown().await;
}

/// An ordinary Track whose create-time start failed ("planner agent is inert"): its create, and a
/// keyed retry of it, own the Planner's start and may submit it again, so a send must not start
/// it (Codex #2184 r2). It stays 409 as on main; recovering it is #2212.
#[tokio::test]
async fn an_ordinary_track_whose_create_time_start_failed_stays_dormant_for_its_creator() {
    let (tmp, repo, state) = app_state(AppServer::Running).await;
    let area = repo
        .area_create(NewArea {
            name: "inert".into(),
            color: "#000".into(),
            sort: None,
        })
        .await
        .unwrap();
    state
        .shared_codex_appserver
        .fail_next_thread_start_for_test();
    let app = router(&state);
    let (status, body) = create_ordinary_track(app.clone(), area.id.to_string(), "inert").await;
    assert!(
        status.is_success(),
        "the create answers success with an inert Planner: {status} {body}"
    );
    let track_id = body["id"].as_str().expect("created track id").to_string();
    let planner_card_id: String =
        sqlx::query_scalar("SELECT id FROM cards WHERE track_id = ?1 AND role = 'planner'")
            .bind(&track_id)
            .fetch_one(repo.pool())
            .await
            .unwrap();
    let boot = Boot {
        app,
        state,
        repo,
        planner_card_id,
        _tmp: tmp,
    };
    assert_eq!(
        boot.scalar(
            "SELECT COUNT(*) FROM worker_sessions WHERE card_id = ?1 AND state = 'failed' \
               AND completed_at_ms IS NOT NULL AND thread_id IS NULL"
        )
        .await,
        1,
        "premise: the failed start left its compensated row"
    );
    assert_eq!(boot.active_session_rows().await, 0, "premise: inert");

    let (status, body) =
        post_input(boot.app.clone(), &boot.input_uri(), "wake up", &new_id()).await;

    assert_eq!(status, StatusCode::CONFLICT, "body={body}");
    assert_eq!(
        body["code"],
        json!("planner_harness_dormant"),
        "body={body}"
    );
    assert_eq!(boot.active_session_rows().await, 0);
    assert_eq!(
        boot.start_ops().await,
        1,
        "only the failed create-time start; the send submitted none"
    );
    assert_eq!(boot.bindings().await, 0, "the message was not stored");
}

/// Channel A's probe (#2184 r2): the launchpad's bootstrap start fails, leaving only a failed,
/// threadless row. The launchpad's ensure still owns the start and re-runs it with a new thread,
/// so a send in between must not start a session that ensure would then supersede.
#[tokio::test]
async fn a_send_after_a_failed_launchpad_bootstrap_waits_for_the_launchpads_own_start() {
    let (tmp, repo, state) = app_state(AppServer::Running).await;
    state
        .shared_codex_appserver
        .fail_next_thread_start_for_test();
    let app = router(&state);
    let (status, body) = ensure_launchpad(app.clone()).await;
    assert!(
        status.is_server_error(),
        "premise: the bootstrap failed: {status} {body}"
    );
    let planner_card_id: String = sqlx::query_scalar(
        "SELECT c.id FROM cards c JOIN tracks t ON t.id = c.track_id \
          WHERE t.purpose = 'launchpad' AND c.role = 'planner'",
    )
    .fetch_one(repo.pool())
    .await
    .expect("the launchpad's Planner card");
    let boot = Boot {
        app,
        state,
        repo,
        planner_card_id,
        _tmp: tmp,
    };
    assert_eq!(
        boot.active_session_rows().await,
        0,
        "premise: no live session"
    );
    let starts = boot.start_ops().await;

    let (status, body) = post_input(
        boot.app.clone(),
        &boot.input_uri(),
        "hello launchpad",
        &new_id(),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "body={body}");
    assert_eq!(
        body["code"],
        json!("planner_harness_dormant"),
        "body={body}"
    );
    assert_eq!(
        boot.start_ops().await,
        starts,
        "the send submitted no start"
    );
    assert_eq!(boot.active_session_rows().await, 0);

    let (status, body) = ensure_launchpad(boot.app.clone()).await;
    assert!(
        status.is_success(),
        "the launchpad starts its own Planner: {status} {body}"
    );
    let active = boot
        .repo
        .session_projection_active_for_card(&boot.planner_card_id)
        .await
        .unwrap()
        .expect("the launchpad's session");
    let (status, body) = post_input(
        boot.app.clone(),
        &boot.input_uri(),
        "hello again",
        &new_id(),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body={body}");
    assert_eq!(
        body["worker_session_id"],
        json!(active.id),
        "the launchpad's own session"
    );
    assert_eq!(boot.active_session_rows().await, 1);
    boot.wait_delivered("hello again").await;
    boot.shutdown().await;
}

async fn ensure_launchpad(app: axum::Router) -> (StatusCode, Value) {
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/today/launchpad/ensure")
                .body(Body::empty())
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
