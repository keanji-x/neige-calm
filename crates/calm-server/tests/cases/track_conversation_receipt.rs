//! Receipt binding is immutable; observation is separate, read-only, and scoped.
use super::boot;
use axum::http::StatusCode;
use calm_server::conversation_keys::derive_track_conversation_operation_key_for_test;
use serde_json::json;

#[tokio::test]
async fn initial_input_receipt_lookup_is_scoped_and_shutdown_is_not_absence() {
    let b = boot().await;
    let track = b.create_track("receipt-observation").await;
    let (status, created) = b
        .create_conversation(&track, "original", "original words")
        .await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    let card = created["id"].as_str().unwrap();
    let uri = format!("/api/tracks/{track}/conversations/{card}/initial-input");
    assert_eq!(
        b.request("GET", &uri, None, None).await.0,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        b.request("GET", &uri, Some("another-key"), None).await.0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        b.request(
            "GET",
            &format!("/api/tracks/another-track/conversations/{card}/initial-input"),
            Some("original"),
            None
        )
        .await
        .0,
        StatusCode::NOT_FOUND
    );
    let (status, observed) = b.request("GET", &uri, Some("original"), None).await;
    assert_eq!(status, StatusCode::OK, "{observed}");
    assert!(
        matches!(observed["kind"].as_str(), Some("queued" | "unavailable")),
        "{observed}"
    );
    assert_eq!(observed["receipt"], created["initialInput"]["receipt"]);
    b.shutdown_harnesses().await;
    let (status, unavailable) = b.request("GET", &uri, Some("original"), None).await;
    assert_eq!(status, StatusCode::OK, "{unavailable}");
    assert_eq!(unavailable["kind"], "unavailable", "{unavailable}");
    assert_eq!(
        unavailable["receipt"], created["initialInput"]["receipt"],
        "lost POST recovery still has exact acceptance after shutdown"
    );
}

#[tokio::test]
async fn initial_input_receipt_legacy_and_corrupt_journals_never_manufacture_acceptance() {
    let b = boot().await;
    let track = b.create_track("receipt-journal").await;
    let (status, created) = b
        .create_conversation(&track, "journal", "journal words")
        .await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    let key = derive_track_conversation_operation_key_for_test(&track, "journal");
    let card = created["id"].as_str().unwrap();
    let uri = format!("/api/tracks/{track}/conversations/{card}/initial-input");
    // This is a historical journal shape, not a fixture that invents a receipt from a queue.
    sqlx::query(concat!(
        "UPDATE operations SET tx_output_json = ",
        "json_remove(tx_output_json, '$.data.initial_input_receipt') ",
        "WHERE idempotency_key = ?1"
    ))
    .bind(&key)
    .execute(b.repo.pool())
    .await
    .unwrap();
    let (status, replay) = b
        .create_conversation(&track, "journal", "journal words")
        .await;
    assert_eq!(status, StatusCode::CREATED, "{replay}");
    assert_eq!(replay["initialInput"], json!({"kind":"legacy-unavailable"}));
    assert_eq!(
        b.request("GET", &uri, Some("journal"), None).await.1,
        json!({"kind":"unavailable","reason":"legacy-operation","receipt":null})
    );
    sqlx::query(concat!(
        "UPDATE operations SET tx_output_json = ",
        "json_set(tx_output_json, '$.data.initial_input_receipt', json(?2)) ",
        "WHERE idempotency_key = ?1"
    ))
    .bind(&key)
    .bind(json!({"version":"unknown-version"}).to_string())
    .execute(b.repo.pool())
    .await
    .unwrap();
    assert_eq!(
        b.request("GET", &uri, Some("journal"), None).await.1,
        json!({"kind":"unavailable","reason":"integrity","receipt":null})
    );
    for (field, invalid) in [
        ("textSha256", json!("incorrect-digest")),
        ("initialRuntimeId", json!("different-runtime")),
        ("entryId", json!("")),
        ("queuedAtMs", json!(-1)),
    ] {
        let mut receipt = created["initialInput"]["receipt"].clone();
        receipt[field] = invalid;
        sqlx::query(concat!(
            "UPDATE operations SET tx_output_json = ",
            "json_set(tx_output_json, '$.data.initial_input_receipt', json(?2)) ",
            "WHERE idempotency_key = ?1"
        ))
        .bind(&key)
        .bind(receipt.to_string())
        .execute(b.repo.pool())
        .await
        .unwrap();
        assert_eq!(
            b.request("GET", &uri, Some("journal"), None).await.1,
            json!({"kind":"unavailable","reason":"integrity","receipt":null}),
            "invalid {field}"
        );
    }
    b.shutdown_harnesses().await;
}

#[tokio::test]
async fn initial_input_receipt_observation_waits_for_the_entire_issuance_command() {
    use axum::extract::FromRef;
    use calm_server::{shared_codex_appserver::TurnStartReturnHook, state::CodexShellState};
    use std::{sync::Arc, time::Duration};
    use tokio::sync::Notify;

    let b = boot().await;
    let track = b.create_track("receipt-serialized-inspection").await;
    let worker = CodexShellState::from_ref(&b.state);
    let entered = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    worker
        .shared_codex_appserver
        .install_turn_start_return_hook_for_test(TurnStartReturnHook {
            entered: entered.clone(),
            release: release.clone(),
        });
    let (status, created) = b
        .create_conversation(&track, "held-issue", "held inside real turn start")
        .await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    tokio::time::timeout(Duration::from_secs(5), entered.notified())
        .await
        .expect("real turn/start reached its return boundary");
    let card = created["id"].as_str().unwrap();
    let uri = format!("/api/tracks/{track}/conversations/{card}/initial-input");
    let mut observation = std::pin::pin!(b.request("GET", &uri, Some("held-issue"), None));
    tokio::select! {
        answer = &mut observation => { release.notify_one(); panic!("inspection escaped the serialized turn/start: {answer:?}"); }
        _ = tokio::time::sleep(Duration::from_millis(40)) => {}
    }
    release.notify_one();
    let (status, observed) = tokio::time::timeout(Duration::from_secs(5), observation)
        .await
        .unwrap();
    assert_eq!(status, StatusCode::OK, "{observed}");
    assert_eq!(observed["kind"], "unavailable", "{observed}");
    assert_eq!(observed["reason"], "runtime-unavailable", "{observed}");
    assert_eq!(observed["receipt"], created["initialInput"]["receipt"]);
    // The pre-start row is readable, but only the real provider completion binds
    // its exact segments to an accepted turn and makes this receipt certifiable.
    let harness = b
        .state
        .harness
        .get(
            created["initialInput"]["receipt"]["initialRuntimeId"]
                .as_str()
                .unwrap(),
        )
        .unwrap();
    let snapshot = harness.snapshot().await;
    worker.shared_codex_appserver.emit_notification_for_test(calm_server::codex_appserver::Notification::Item {
        method: "item/completed".into(),
        params: json!({"threadId": snapshot.last_thread_id.unwrap(), "turn":{"id":snapshot.last_turn_id.unwrap()},
            "item":{"id":"provider-confirmed-first","clientId":created["initialInput"]["receipt"]["entryId"],"type":"userMessage",
            "content":[{"type":"text","text":"held inside real turn start"}]}}),
    });
    let confirmed = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let (status, observed) = b.request("GET", &uri, Some("held-issue"), None).await;
            assert_eq!(status, StatusCode::OK, "{observed}");
            if observed["kind"] == "projected" {
                break observed;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("provider completion certifies the exact original receipt");
    assert_eq!(confirmed["receipt"], created["initialInput"]["receipt"]);
    let (_, replay) = b
        .create_conversation(&track, "held-issue", "held inside real turn start")
        .await;
    assert_eq!(replay["initialInput"], created["initialInput"]);
    b.shutdown_harnesses().await;
}
