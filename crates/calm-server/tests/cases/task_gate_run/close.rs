//! Close inventory includes gates after their worker has been canceled.
use super::*;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn track_close_refuses_an_active_worker_gate_without_canceling_the_attempt() {
    close_with_gate(false).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn track_close_refuses_the_independent_gate_of_a_canceled_worker() {
    close_with_gate(true).await;
}

async fn close_with_gate(canceled: bool) {
    use axum::{
        body::Body,
        http::{Request, StatusCode},
    };
    use tower::ServiceExt;
    let fx = world(LONG_IDLE).await;
    let flag = outside(&fx, "close-release");
    let a = running(
        &fx,
        "close-gate",
        json!([{"name":"block","cmd":wait_for(&flag)}]),
    )
    .await;
    let app = crate::support::authenticated_router::application(
        crate::task_projection_acceptance::route_state(&fx.boot).await,
    )
    .await;
    let close = || {
        app.clone().oneshot(
            Request::patch(format!("/api/tracks/{}", fx.track()))
                .header("content-type", "application/json")
                .body(Body::from(json!({"closed":true}).to_string()))
                .unwrap(),
        )
    };
    let (answer, close) = tokio::join!(
        gate_run(&fx, &a, Some("test: checkpoint Close refusal")),
        async {
            wait_step(&fx, &a.task.id, 1, "block").await;
            if canceled {
                call_tool(&fx.boot, "neige_task_cancel", planner_identity(&fx.boot),
                    json!({"key":"close-gate","message":"Cancel the worker while its gate is running"}))
                    .await.unwrap();
                fx.scheduler().sweep_all().await;
            }
            // Retire the fixture-only companions through their normal terminal transition.
            // The independently running gate remains in the Close inventory.
            calm_server::db::write_in_tx_typed(fx.boot.repo.as_ref(), |tx| {
                Box::pin(async move {
                    for id in [
                        "planner-session",
                        crate::mcp_track_report::WORKER_SESSION_ID,
                    ] {
                        calm_server::db::sqlite::session_complete_tx(
                            tx,
                            &id.to_owned(),
                            calm_types::worker::WorkerSessionState::Exited,
                        )
                        .await?;
                    }
                    Ok(())
                })
            })
            .await
            .unwrap();
            let result = close().await.unwrap().status();
            let status = current(&fx.boot, "close-gate").await.status;
            touch(&flag);
            (result, status)
        }
    );
    assert_eq!(
        close.0,
        StatusCode::CONFLICT,
        "unsafe worker gate activity must refuse the whole Close"
    );
    assert_eq!(
        close.1,
        if canceled {
            TaskStatus::Canceled
        } else {
            TaskStatus::Running
        }
    );
    answer.unwrap();
    let cleared = app
        .oneshot(
            Request::patch(format!("/api/tracks/{}", fx.track()))
                .header("content-type", "application/json")
                .body(Body::from(json!({"closed":true}).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        cleared.status(),
        StatusCode::OK,
        "the same inventory is closable after the gate settles"
    );
}
