//! #2212: a create retried under its `Idempotency-Key` resumes the track it minted, and the
//! Planner start it re-runs must not supersede a session the card already has. The message-less
//! resume skips a card with a conversation; the keyed first-message retry refuses one with 409;
//! either answers 503 while another start of the card is in flight.

use axum::http::StatusCode;
use calm_truth::db::sqlite::PLANNER_START_CARD_KEY;

use super::{Boot, boot};

impl Boot {
    /// `planner-harness-start` operations naming `card_id`, in any phase.
    async fn start_ops(&self, card_id: &str) -> i64 {
        sqlx::query_scalar(
            "SELECT COUNT(*) FROM operations WHERE kind = 'planner-harness-start' \
               AND json_extract(payload_json, ?2) = ?1",
        )
        .bind(card_id)
        .bind(format!("$.{PLANNER_START_CARD_KEY}"))
        .fetch_one(self.repo.pool())
        .await
        .unwrap()
    }

    /// The card's active session as `(id, thread_id)`, if it has one.
    async fn live_session(&self, card_id: &str) -> Option<(String, Option<String>)> {
        sqlx::query_as(
            "SELECT id, thread_id FROM worker_sessions WHERE card_id = ?1 \
               AND state IN ('starting','running','idle','turn_pending')",
        )
        .bind(card_id)
        .fetch_optional(self.repo.pool())
        .await
        .unwrap()
    }

    /// The planner card of the track a create answered with.
    async fn planner_card_of(&self, track: &serde_json::Value) -> String {
        sqlx::query_scalar("SELECT id FROM cards WHERE track_id = ?1 AND role = 'planner'")
            .bind(track["id"].as_str().expect("the create answered a track"))
            .fetch_one(self.repo.pool())
            .await
            .unwrap()
    }

    /// A start of `card_id` that another instance holds the lease of, so no driver here takes it.
    async fn start_in_flight_elsewhere(&self, card_id: &str) {
        sqlx::query(
            "INSERT INTO operations (id, operation_key, kind, payload_hash, target_type, \
               target_json, payload_json, phase, lease_owner, lease_until_ms, created_at_ms, \
               updated_at_ms) \
             VALUES ('in-flight', 'in-flight', 'planner-harness-start', 'h', 'track', '{}', ?1, \
               'tx_committed', 'elsewhere', 9223372036854775807, 0, 0)",
        )
        .bind(serde_json::json!({ PLANNER_START_CARD_KEY: card_id }).to_string())
        .execute(self.repo.pool())
        .await
        .unwrap();
    }
}

/// The headline: the first create started the Planner, so its retry starts nothing, and the live
/// session keeps its id and its thread.
#[tokio::test]
async fn a_message_less_retry_keeps_the_live_session() {
    let b = boot().await;
    let (first, track) = b.create_track(Some("idem-2212-live"), None).await;
    assert_eq!(first, StatusCode::CREATED, "body={track}");
    let card = b.planner_card_of(&track).await;
    let (session, thread) = b
        .live_session(&card)
        .await
        .expect("premise: the create's start succeeded");
    assert!(thread.is_some(), "premise: the live session holds a thread");
    assert!(b.state.harness.get(&session).is_some(), "premise: and runs");
    assert_eq!(b.start_ops(&card).await, 1);

    let (retry, body) = b.create_track(Some("idem-2212-live"), None).await;
    assert_eq!(retry, StatusCode::CREATED, "the same 201 as before: {body}");
    assert_eq!(body["id"], track["id"], "the key's own track");
    assert_eq!(
        b.start_ops(&card).await,
        1,
        "a retry onto a card with a conversation submits no start"
    );
    assert_eq!(
        b.live_session(&card).await,
        Some((session.clone(), thread)),
        "the live session survives with its thread"
    );
    assert!(b.state.harness.get(&session).is_some(), "and still runs");
    b.shutdown_harnesses().await;
}

/// The control: a first start that failed left nothing to preserve, so the retry starts it.
#[tokio::test]
async fn a_message_less_retry_after_a_failed_start_starts_it() {
    let b = boot().await;
    b.state
        .shared_codex_appserver
        .fail_next_thread_start_for_test();
    let (first, track) = b.create_track(Some("idem-2212-failed"), None).await;
    assert_eq!(
        first,
        StatusCode::CREATED,
        "a failed start is still a 201: {track}"
    );
    let card = b.planner_card_of(&track).await;
    assert_eq!(
        b.live_session(&card).await,
        None,
        "premise: the start failed"
    );
    assert_eq!(b.start_ops(&card).await, 1);

    let (retry, body) = b.create_track(Some("idem-2212-failed"), None).await;
    assert_eq!(retry, StatusCode::CREATED, "body={body}");
    assert_eq!(body["id"], track["id"]);
    assert_eq!(b.start_ops(&card).await, 2, "the retry starts the Planner");
    assert!(
        b.live_session(&card).await.is_some(),
        "and the card now has a live session"
    );
    b.shutdown_harnesses().await;
}

/// The keyed first-message create's start failed, a reset then gave the card a live session, and
/// only then did the create retry: the retry must not replace that session. It answers 409 and
/// leaves the message to the ordinary send.
#[tokio::test]
async fn a_keyed_retry_onto_a_card_started_since_is_a_conflict() {
    const SENTENCE: &str = "the sentence of a create whose start failed";
    let b = boot().await;
    b.state
        .shared_codex_appserver
        .fail_next_thread_start_for_test();
    let (first, body) = b
        .create_track(Some("idem-2212-keyed"), Some(SENTENCE))
        .await;
    assert!(first.is_server_error(), "premise: the start failed: {body}");
    let (_, card) = b.only_runtime().await;
    assert_eq!(b.live_session(&card).await, None, "premise: nothing runs");

    let (reset, reset_body) = b.reset_planner(&card).await;
    assert_eq!(
        reset,
        StatusCode::OK,
        "premise: the reset starts it: {reset_body}"
    );
    let live = b
        .live_session(&card)
        .await
        .expect("premise: the reset left a live session");
    let starts = b.start_ops(&card).await;

    let (retry, body) = b
        .create_track(Some("idem-2212-keyed"), Some(SENTENCE))
        .await;
    assert_eq!(retry, StatusCode::CONFLICT, "body={body}");
    assert_eq!(body["code"], "conflict", "body={body}");
    assert!(
        body.to_string().contains("already has a session")
            && body.to_string().contains("planner/input"),
        "the 409 states what it found and names the send path the message belongs to: {body}"
    );
    assert_eq!(b.start_ops(&card).await, starts, "the retry starts nothing");
    assert_eq!(
        b.live_session(&card).await,
        Some(live),
        "the reset's session survives with its thread"
    );
    b.shutdown_harnesses().await;
}

/// A start of the card that has not finished: the message-less retry answers 503 and submits no
/// second start, which would supersede whatever that one leaves.
#[tokio::test]
async fn a_message_less_retry_while_a_start_is_in_flight_is_503() {
    let b = boot().await;
    b.state
        .shared_codex_appserver
        .fail_next_thread_start_for_test();
    let (first, track) = b.create_track(Some("idem-2212-in-flight"), None).await;
    assert_eq!(first, StatusCode::CREATED, "body={track}");
    let card = b.planner_card_of(&track).await;
    b.start_in_flight_elsewhere(&card).await;
    assert_eq!(b.start_ops(&card).await, 2, "premise: one start in flight");

    let (retry, body) = b.create_track(Some("idem-2212-in-flight"), None).await;
    assert_eq!(retry, StatusCode::SERVICE_UNAVAILABLE, "body={body}");
    assert_eq!(b.start_ops(&card).await, 2, "no start was submitted");
    assert_eq!(b.live_session(&card).await, None);
    b.shutdown_harnesses().await;
}

/// The keyed twin: a genuine retry after a failed start waits for an in-flight start too.
#[tokio::test]
async fn a_keyed_retry_while_a_start_is_in_flight_is_503() {
    const SENTENCE: &str = "the sentence of a create retried during a start";
    let b = boot().await;
    b.state
        .shared_codex_appserver
        .fail_next_thread_start_for_test();
    let (first, body) = b
        .create_track(Some("idem-2212-keyed-in-flight"), Some(SENTENCE))
        .await;
    assert!(first.is_server_error(), "premise: the start failed: {body}");
    let (_, card) = b.only_runtime().await;
    b.start_in_flight_elsewhere(&card).await;
    let starts = b.start_ops(&card).await;

    let (retry, body) = b
        .create_track(Some("idem-2212-keyed-in-flight"), Some(SENTENCE))
        .await;
    assert_eq!(retry, StatusCode::SERVICE_UNAVAILABLE, "body={body}");
    assert_eq!(b.start_ops(&card).await, starts, "no start was submitted");
    assert_eq!(b.live_session(&card).await, None);
    b.shutdown_harnesses().await;
}
