//! #2212 on a Claude Planner: a create retried under its `Idempotency-Key` after its OWN start
//! failed must start the Planner again. A Claude start binds the row's thread with no RPC, before
//! `spawn_side_effect` can fail, so the failed row it leaves names a thread that never ran a turn.
//! Such a row is no conversation: the retry starts, and the first message is delivered.

use std::sync::Arc;
use std::time::Duration;

use axum::http::StatusCode;
use calm_server::operation::planner_harness_start_adapter::claude_spawn_failure;
use calm_server::test_seams::{
    OPERATION_DEDUP_MISSED, PausePoint, TRACK_CREATE_BEFORE_PLANNER_START, install_pause_for_test,
};
use calm_truth::db::sqlite::PLANNER_START_CARD_KEY;
use serde_json::{Value, json};
use tokio::sync::Notify;

use super::claude_planner_stack_fixture::{Root, Stack};

const BUDGET: Duration = Duration::from_secs(30);

/// The create's start operation key, as `POST /api/tracks` derives it for a keyed create.
fn create_operation_key(area_id: &str, key: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(format!("track-create:{area_id}:{key}"));
    format!("track-create-{}", hex::encode(hasher.finalize()))
}

fn create_body(area_id: &str, first_message: Option<&str>) -> Value {
    let mut body = json!({
        "planner_provider": "claude",
        "area_id": area_id,
        "title": "claude planner",
        "theme": {"fg": [216, 219, 226], "bg": [15, 20, 24]},
    });
    if let Some(text) = first_message {
        body["first_message"] = json!(text);
    }
    body
}

impl Stack {
    /// `POST /api/tracks` under `key`, as the person.
    async fn keyed_create(&self, key: &str, body: Value) -> (StatusCode, Value) {
        use axum::body::Body;
        use axum::http::Request;
        use http_body_util::BodyExt;
        use tower::ServiceExt;
        let request = Request::builder()
            .method("POST")
            .uri("/api/tracks")
            .header("x-calm-actor", "user")
            .header("content-type", "application/json")
            .header("idempotency-key", key)
            .body(Body::from(body.to_string()))
            .expect("request");
        let response = self.app.clone().oneshot(request).await.expect("response");
        let status = response.status();
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        (
            status,
            serde_json::from_slice(&bytes).unwrap_or(Value::Null),
        )
    }

    /// The area's one Planner card.
    async fn planner_card_in(&self, area_id: &str) -> String {
        let pool = self.repo().sqlite_pool().expect("pool");
        sqlx::query_scalar(
            "SELECT c.id FROM cards c JOIN tracks t ON t.id = c.track_id \
              WHERE t.area_id = ?1 AND c.role = 'planner'",
        )
        .bind(area_id)
        .fetch_one(&pool)
        .await
        .expect("the create minted a Planner card")
    }

    /// Every session row of the card as `(state, thread_id)`.
    async fn session_rows(&self, card_id: &str) -> Vec<(String, Option<String>)> {
        let pool = self.repo().sqlite_pool().expect("pool");
        sqlx::query_as("SELECT state, thread_id FROM worker_sessions WHERE card_id = ?1")
            .bind(card_id)
            .fetch_all(&pool)
            .await
            .expect("rows")
    }

    async fn start_ops(&self, card_id: &str) -> i64 {
        let pool = self.repo().sqlite_pool().expect("pool");
        sqlx::query_scalar(
            "SELECT COUNT(*) FROM operations WHERE kind = 'planner-harness-start' \
               AND json_extract(payload_json, ?2) = ?1",
        )
        .bind(card_id)
        .bind(format!("$.{PLANNER_START_CARD_KEY}"))
        .fetch_one(&pool)
        .await
        .expect("count")
    }

    /// Run `create` while the card's first Claude spawn fails: `point` pauses the create once its
    /// card exists, so the failure is armed on that card before its start runs.
    async fn create_with_failed_spawn(
        &self,
        area_id: &str,
        point: &str,
        point_key: &str,
        create: impl std::future::Future<Output = (StatusCode, Value)>,
    ) -> (StatusCode, Value, String) {
        let hold = PausePoint {
            entered: Arc::new(Notify::new()),
            release: Arc::new(Notify::new()),
        };
        install_pause_for_test(point, point_key, hold.clone());
        let arm = async {
            hold.entered.notified().await;
            let failure = claude_spawn_failure::arm(&self.planner_card_in(area_id).await);
            failure.release.notify_one();
            hold.release.notify_one();
        };
        let ((status, body), ()) =
            tokio::time::timeout(BUDGET, async { tokio::join!(create, arm) })
                .await
                .expect("the first create settles");
        let card_id = self.planner_card_in(area_id).await;
        (status, body, card_id)
    }

    /// The premise both tests stand on: the failed start's row names a thread nothing ran on.
    async fn assert_only_a_threaded_failed_start(&self, card_id: &str) {
        let rows = self.session_rows(card_id).await;
        assert!(
            matches!(rows.as_slice(), [(state, Some(_))] if state == "failed"),
            "premise: one failed row that names a thread: {rows:?}"
        );
        assert_eq!(self.start_ops(card_id).await, 1);
    }
}

/// The keyed first-message create: the retry starts the Planner and delivers the message, where a
/// 409 would leave it undeliverable (the card's creator owns its start, so a send is dormant).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_keyed_retry_after_the_creates_own_claude_spawn_failure_starts_and_delivers() {
    const SENTENCE: &str = "the first message of a create whose spawn failed";
    let root = Root::new("exit");
    let stack = Stack::boot(&root).await;
    let area = stack.area().await;
    let (first, body, card) = stack
        .create_with_failed_spawn(
            &area,
            OPERATION_DEDUP_MISSED,
            &create_operation_key(&area, "claude-keyed"),
            stack.keyed_create("claude-keyed", create_body(&area, Some(SENTENCE))),
        )
        .await;
    assert!(first.is_server_error(), "premise: the spawn failed: {body}");
    stack.assert_only_a_threaded_failed_start(&card).await;

    let (retry, body) = tokio::time::timeout(
        BUDGET,
        stack.keyed_create("claude-keyed", create_body(&area, Some(SENTENCE))),
    )
    .await
    .expect("the retry settles");
    assert_eq!(retry, StatusCode::CREATED, "the retry starts it: {body}");
    assert_eq!(stack.start_ops(&card).await, 2, "one more start");
    stack.runtime(&card).await;
    let outcomes = stack.wait_outcomes(&card, 1).await;
    assert_eq!(
        outcomes[0]["status"], "completed",
        "the first message reached the Planner: {outcomes:?}"
    );
}

/// The message-less twin: the retry starts the Planner instead of answering 201 over a card
/// nothing runs.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_message_less_retry_after_the_creates_own_claude_spawn_failure_starts_it() {
    let root = Root::new("exit");
    let stack = Stack::boot(&root).await;
    let area = stack.area().await;
    let (first, body, card) = stack
        .create_with_failed_spawn(
            &area,
            TRACK_CREATE_BEFORE_PLANNER_START,
            &area,
            stack.keyed_create("claude-message-less", create_body(&area, None)),
        )
        .await;
    assert_eq!(
        first,
        StatusCode::CREATED,
        "a failed start is a 201: {body}"
    );
    stack.assert_only_a_threaded_failed_start(&card).await;

    let (retry, body) = tokio::time::timeout(
        BUDGET,
        stack.keyed_create("claude-message-less", create_body(&area, None)),
    )
    .await
    .expect("the retry settles");
    assert_eq!(retry, StatusCode::CREATED, "body={body}");
    assert_eq!(stack.start_ops(&card).await, 2, "the retry starts it");
    stack.runtime(&card).await;
}
