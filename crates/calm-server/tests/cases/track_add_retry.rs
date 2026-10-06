//! #2212 on `neige_track_add`: a retried add resumes the Track its key minted, and refuses to
//! start that Track's Planner again once it has a conversation.

use axum::http::StatusCode;
use calm_truth::db::sqlite::PLANNER_START_CARD_KEY;
use serde_json::json;

use super::{boot, send};

/// The added Track's start failed, a reset then started its Planner, and the creator retried the
/// add: a typed `-32409`, with no second start of the card.
#[tokio::test]
async fn track_add_retry_onto_a_planner_started_since_is_a_conflict() {
    let boot = boot(16).await;
    let (_, planner) = boot.user_track("portfolio").await;
    boot.codex.fail_next_thread_start_for_test();
    boot.add(&planner, boot.args("k1"))
        .await
        .expect_err("premise: the added Track's start failed");
    let added: String = boot
        .scalar("SELECT id FROM tracks WHERE creator_key = ?1", "k1")
        .await;
    let card: String = boot
        .scalar(
            "SELECT id FROM cards WHERE track_id = ?1 AND role = 'planner'",
            &added,
        )
        .await;
    let (status, reset) = send(
        boot.app.clone(),
        "POST",
        &format!("/api/cards/{card}/planner/reset"),
        Some(json!({})),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::OK,
        "premise: the reset starts it: {reset}"
    );
    let starts = || async {
        sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(*) FROM operations WHERE kind = 'planner-harness-start' \
               AND json_extract(payload_json, ?2) = ?1",
        )
        .bind(&card)
        .bind(format!("$.{PLANNER_START_CARD_KEY}"))
        .fetch_one(boot.repo.pool())
        .await
        .unwrap()
    };
    let before = starts().await;

    let error = boot
        .add(&planner, boot.args("k1"))
        .await
        .expect_err("a retry onto a Planner with a conversation starts nothing");
    assert_eq!(error.code, -32409, "{error:?}");
    assert!(
        error.message.contains("already has a session")
            && error.message.contains(calm_server::mail::TOOL_MAIL_SEND)
            && !error.message.contains("planner/input"),
        "the observed fact, and a send path an agent can take: {error:?}"
    );
    assert_eq!(starts().await, before, "no second start of the card");
}

/// The added Track's start failed and another start of its Planner is still running (another
/// instance holds its lease): the retried add starts nothing and answers a typed `-32503`.
#[tokio::test]
async fn track_add_retry_while_a_start_is_in_flight_is_unavailable() {
    let boot = boot(16).await;
    let (_, planner) = boot.user_track("portfolio").await;
    boot.codex.fail_next_thread_start_for_test();
    boot.add(&planner, boot.args("k1"))
        .await
        .expect_err("premise: the added Track's start failed");
    let added: String = boot
        .scalar("SELECT id FROM tracks WHERE creator_key = ?1", "k1")
        .await;
    let card: String = boot
        .scalar(
            "SELECT id FROM cards WHERE track_id = ?1 AND role = 'planner'",
            &added,
        )
        .await;
    sqlx::query(
        "INSERT INTO operations (id, operation_key, kind, payload_hash, target_type, \
           target_json, payload_json, phase, lease_owner, lease_until_ms, created_at_ms, \
           updated_at_ms) \
         VALUES ('in-flight', 'in-flight', 'planner-harness-start', 'h', 'track', '{}', ?1, \
           'tx_committed', 'elsewhere', 9223372036854775807, 0, 0)",
    )
    .bind(json!({ PLANNER_START_CARD_KEY: card }).to_string())
    .execute(boot.repo.pool())
    .await
    .unwrap();
    let starts = || async {
        sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(*) FROM operations WHERE kind = 'planner-harness-start' \
               AND json_extract(payload_json, ?2) = ?1",
        )
        .bind(&card)
        .bind(format!("$.{PLANNER_START_CARD_KEY}"))
        .fetch_one(boot.repo.pool())
        .await
        .unwrap()
    };
    let before = starts().await;

    let error = boot
        .add(&planner, boot.args("k1"))
        .await
        .expect_err("a retry during another start waits for it");
    assert_eq!(error.code, -32503, "{error:?}");
    assert_eq!(starts().await, before, "no start was submitted");
}
