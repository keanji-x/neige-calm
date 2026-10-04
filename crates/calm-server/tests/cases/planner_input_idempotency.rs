//! #2043 — `POST /planner/input` under an `Idempotency-Key`: a retry after a lost answer replays
//! the first answer and queues nothing; a different body under a used key is a 409; a refusal
//! binds nothing, so its key can be sent again.

use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use calm_server::harness::run_loop::{
    PlannerHarnessObservationRaceHook, install_planner_harness_observation_race_hook_for_test,
};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tokio::sync::Notify;
use tower::ServiceExt;

use crate::support::planner_queue_fixture::{
    Boot, boot_with, get, idle_snapshot, post_input_keyed, upload_png,
};

/// The texts of the queue as `GET /planner/run` lists it.
async fn queued_texts(boot: &Boot) -> Vec<String> {
    let card_id = boot.planner_card.id.as_str();
    let (status, run) = get(
        boot.app.clone(),
        format!("/api/cards/{card_id}/planner/run"),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "run={run}");
    run["pending"]
        .as_array()
        .expect("pending is an array")
        .iter()
        .map(|entry| entry["text"].as_str().unwrap_or_default().to_string())
        .collect()
}

async fn enqueued_audits(boot: &Boot) -> i64 {
    sqlx::query_scalar("SELECT COUNT(*) FROM events WHERE kind = 'harness.user_message.enqueued'")
        .fetch_one(boot.repo.pool())
        .await
        .expect("count audit events")
}

async fn bindings(boot: &Boot) -> i64 {
    sqlx::query_scalar("SELECT COUNT(*) FROM planner_input_idempotency WHERE card_id = ?1")
        .bind(boot.planner_card.id.as_str())
        .fetch_one(boot.repo.pool())
        .await
        .expect("count bindings")
}

#[tokio::test]
async fn a_retry_after_a_lost_answer_replays_it_and_queues_one_message() {
    let boot = boot_with(idle_snapshot(vec![])).await;
    let card_id = boot.planner_card.id.as_str().to_string();
    let body = json!({"text": "summarise the report"});

    // The server stored it and answered; the browser never saw this answer.
    let (status, first) = post_input_keyed(boot.app.clone(), &card_id, body.clone(), "k-1").await;
    assert_eq!(status, StatusCode::OK, "body={first}");
    let (status, retried) = post_input_keyed(boot.app.clone(), &card_id, body, "k-1").await;
    assert_eq!(status, StatusCode::OK, "body={retried}");

    assert_eq!(
        retried, first,
        "the retry is answered what the first request was"
    );
    assert_eq!(queued_texts(&boot).await, vec!["summarise the report"]);
    assert_eq!(
        enqueued_audits(&boot).await,
        1,
        "a replay is not a second send"
    );
}

#[tokio::test]
async fn a_used_key_with_a_different_message_is_a_conflict() {
    let boot = boot_with(idle_snapshot(vec![])).await;
    let card_id = boot.planner_card.id.as_str().to_string();
    let (status, _) =
        post_input_keyed(boot.app.clone(), &card_id, json!({"text": "first"}), "k-1").await;
    assert_eq!(status, StatusCode::OK);

    let (status, body) =
        post_input_keyed(boot.app.clone(), &card_id, json!({"text": "second"}), "k-1").await;
    assert_eq!(status, StatusCode::CONFLICT, "body={body}");
    assert_eq!(body["code"], json!("conflict"), "body={body}");
    assert_eq!(queued_texts(&boot).await, vec!["first"]);
}

#[tokio::test]
async fn two_requests_in_flight_under_one_key_queue_one_message() {
    let boot = boot_with(idle_snapshot(vec![])).await;
    let card_id = boot.planner_card.id.as_str().to_string();
    let body = json!({"text": "only once"});
    let (left, right) = tokio::join!(
        post_input_keyed(boot.app.clone(), &card_id, body.clone(), "k-1"),
        post_input_keyed(boot.app.clone(), &card_id, body.clone(), "k-1"),
    );
    assert_eq!(left.0, StatusCode::OK, "body={}", left.1);
    assert_eq!(right.0, StatusCode::OK, "body={}", right.1);
    assert_eq!(
        left.1, right.1,
        "both are answered with the one stored message"
    );
    assert_eq!(queued_texts(&boot).await, vec!["only once"]);
}

/// The runtime leaves the active set while the send is between its harness lookup and its
/// snapshot write: the text is not stored, so the key must stay unbound and reusable.
#[tokio::test]
async fn a_refused_send_binds_nothing_and_its_key_can_be_sent_again() {
    let boot = boot_with(idle_snapshot(vec![])).await;
    let card_id = boot.planner_card.id.as_str().to_string();
    let entered = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    install_planner_harness_observation_race_hook_for_test(
        &boot.worker_session_id,
        PlannerHarnessObservationRaceHook {
            entered: entered.clone(),
            release: release.clone(),
        },
    );
    let app = boot.app.clone();
    let refused_card = card_id.clone();
    let refused = tokio::spawn(async move {
        post_input_keyed(app, &refused_card, json!({"text": "say it once"}), "k-1").await
    });
    tokio::time::timeout(Duration::from_secs(5), entered.notified())
        .await
        .expect("the send reached the queue");
    sqlx::query("UPDATE worker_sessions SET state = 'failed' WHERE id = ?1")
        .bind(&boot.worker_session_id)
        .execute(boot.repo.pool())
        .await
        .unwrap();
    release.notify_one();
    let (status, body) = refused.await.unwrap();
    assert_eq!(status, StatusCode::CONFLICT, "body={body}");
    assert_eq!(
        body["code"],
        json!("planner_harness_runtime_superseded"),
        "body={body}"
    );
    assert_eq!(
        bindings(&boot).await,
        0,
        "nothing was stored, so nothing is bound"
    );

    sqlx::query("UPDATE worker_sessions SET state = 'idle' WHERE id = ?1")
        .bind(&boot.worker_session_id)
        .execute(boot.repo.pool())
        .await
        .unwrap();
    let (status, body) = post_input_keyed(
        boot.app.clone(),
        &card_id,
        json!({"text": "say it once"}),
        "k-1",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body={body}");
    assert_eq!(queued_texts(&boot).await, vec!["say it once"]);
    assert_eq!(bindings(&boot).await, 1);
}

/// The binding is durable: with the harness gone from the registry, as after a restart, the
/// retry is answered from the binding and recovers nothing.
#[tokio::test]
async fn a_retry_after_the_harness_is_gone_replays_without_recovering_it() {
    let boot = boot_with(idle_snapshot(vec![])).await;
    let card_id = boot.planner_card.id.as_str().to_string();
    let body = json!({"text": "outlive the harness"});
    let (status, first) = post_input_keyed(boot.app.clone(), &card_id, body.clone(), "k-1").await;
    assert_eq!(status, StatusCode::OK, "body={first}");
    boot.registry
        .remove(&boot.worker_session_id)
        .expect("the harness was registered")
        .shutdown()
        .await
        .unwrap();

    let (status, retried) = post_input_keyed(boot.app.clone(), &card_id, body, "k-1").await;
    assert_eq!(status, StatusCode::OK, "body={retried}");
    assert_eq!(retried, first);
    assert!(
        boot.registry.get(&boot.worker_session_id).is_none(),
        "a replay must not lazily recover the harness"
    );
    assert_eq!(enqueued_audits(&boot).await, 1);
}

#[tokio::test]
async fn a_retry_with_attachments_replays_without_binding_them_again() {
    let boot = boot_with(idle_snapshot(vec![])).await;
    let card_id = boot.planner_card.id.as_str().to_string();
    let (status, uploaded) = upload_png(boot.app.clone(), &card_id, b"idem").await;
    assert_eq!(status, StatusCode::CREATED, "body={uploaded}");
    let attachment = uploaded["attachmentId"].as_str().unwrap().to_string();
    let body = json!({"text": "", "attachments": [attachment]});

    let (status, first) = post_input_keyed(boot.app.clone(), &card_id, body.clone(), "k-1").await;
    assert_eq!(status, StatusCode::OK, "body={first}");
    let (status, retried) = post_input_keyed(boot.app.clone(), &card_id, body, "k-1").await;
    assert_eq!(status, StatusCode::OK, "body={retried}");
    assert_eq!(retried, first);
    assert_eq!(queued_texts(&boot).await.len(), 1);
}

#[tokio::test]
async fn a_send_without_a_key_is_refused() {
    let boot = boot_with(idle_snapshot(vec![])).await;
    let card_id = boot.planner_card.id.as_str();
    let response = boot
        .app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(format!("/api/cards/{card_id}/planner/input"))
                .header("content-type", "application/json")
                .header("x-calm-actor", "user")
                .body(Body::from(json!({"text": "unkeyed"}).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let body: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    assert_eq!(status, StatusCode::BAD_REQUEST, "body={body}");
    assert!(queued_texts(&boot).await.is_empty());
}
