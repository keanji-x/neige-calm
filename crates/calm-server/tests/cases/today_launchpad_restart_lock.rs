//! #2228 — `POST /api/today/launchpad/ensure` starts the EXISTING launchpad Planner (here: its
//! re-point after the workspace root moved) under the card's `planner_recovery_locks`, so a send
//! to that Planner waits for the start instead of racing it.

use std::time::{Duration, Instant};

use axum::http::StatusCode;
use calm_server::db::prelude::*;
use calm_server::model::new_id;
use serde_json::{Value, json};

use crate::planner_first_start::{AppServer, Boot, app_state, post_input, router};
use crate::planner_repoint_restart_lock::{joined, request, wait_until};

fn ensure(app: axum::Router) -> tokio::task::JoinHandle<(StatusCode, Value)> {
    tokio::spawn(request(
        app,
        "POST",
        "/api/today/launchpad/ensure".into(),
        json!({}),
    ))
}

#[tokio::test]
async fn a_send_during_the_launchpad_restart_waits_on_the_cards_lock() {
    let (tmp, repo, state) = app_state(AppServer::Running).await;
    let (status, launchpad) = joined(ensure(router(&state)), "the first ensure").await;
    assert_eq!(status, StatusCode::CREATED, "body={launchpad}");
    let planner_card_id = launchpad["planner_card_id"].as_str().unwrap().to_string();
    let old_runtime = repo
        .session_projection_active_for_card(&planner_card_id)
        .await
        .unwrap()
        .expect("the bootstrap started the launchpad Planner")
        .id;
    assert!(
        state.harness.get(&old_runtime).is_some(),
        "premise: the Planner is live"
    );

    // The same server over a moved workspace root: the next ensure re-points the existing card.
    let state = state.with_workspace_root(tmp.path().join("workspaces-moved"));
    let boot = Boot {
        app: router(&state),
        state,
        repo,
        planner_card_id,
        _tmp: tmp,
    };
    let card_id = boot.planner_card_id.clone();
    let starts_before = boot.start_ops().await;

    let thread_start = boot
        .state
        .shared_codex_appserver
        .lock_thread_start_serial_for_test()
        .await;
    let restart = ensure(boot.app.clone());
    // The start has retired the live Planner and is now held in thread/start.
    let deadline = Instant::now() + Duration::from_secs(10);
    while boot.start_ops().await == starts_before || boot.state.harness.get(&old_runtime).is_some()
    {
        assert!(
            Instant::now() < deadline,
            "the ensure never started the existing Planner again"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let held = boot.state.planner_recovery_lock_handles_for_test(&card_id);
    assert!(
        held > 0,
        "the ensure started the existing Planner without holding the card's lock"
    );

    let send = {
        let (app, uri) = (boot.app.clone(), boot.input_uri());
        tokio::spawn(async move { post_input(app, &uri, "good morning", &new_id()).await })
    };
    wait_until("the send queues on the card's lock", || {
        boot.state.planner_recovery_lock_handles_for_test(&card_id) > held
    })
    .await;
    assert!(!send.is_finished(), "the send waits behind the start");
    assert!(!restart.is_finished(), "the start is still in thread/start");

    drop(thread_start);
    let (status, body) = joined(restart, "the ensure finishes").await;
    assert_eq!(status, StatusCode::OK, "body={body}");
    let (status, body) = joined(send, "the send finishes").await;
    assert_eq!(status, StatusCode::OK, "body={body}");
    let active = boot
        .repo
        .session_projection_active_for_card(&card_id)
        .await
        .unwrap()
        .expect("the start left an active session");
    assert_ne!(active.id, old_runtime, "premise: the ensure re-pointed");
    assert_eq!(
        body["worker_session_id"],
        json!(active.id),
        "the send went to the restarted session"
    );
    assert_eq!(boot.start_ops().await, starts_before + 1);
    assert_eq!(boot.active_session_rows().await, 1);
    boot.wait_delivered("good morning").await;
    boot.shutdown().await;
}
