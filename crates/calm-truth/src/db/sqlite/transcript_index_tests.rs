//! #1722 S1b — `idx_transcript_card_method_created_at` (migration 0110) is
//! the index the activity projector's per-track evidence reads (the E1 shape
//! of the design's §4.3) and the `last_turn_completed` subquery both walk.
//!
//! `EXPLAIN QUERY PLAN` is the only cheap witness: a scan and an index range
//! return the same rows, so a row-level test cannot see the difference.
//! Dropping the index (or filtering the transcript table on `track_id`,
//! which has no index) turns the plan into a `SCAN` and these red.

use sqlx::{Row, SqlitePool};

use super::SqlxRepo;
use crate::session_projection_row::LAST_TURN_COMPLETED_MS_SUBQUERY;

const INDEX: &str = "USING INDEX idx_transcript_card_method_created_at";

async fn plan(pool: &SqlitePool, sql: &str) -> Vec<String> {
    sqlx::query(&format!("EXPLAIN QUERY PLAN {sql}"))
        .bind("track-1")
        .fetch_all(pool)
        .await
        .unwrap()
        .into_iter()
        .map(|row| row.get::<String, _>("detail"))
        .collect()
}

#[tokio::test]
async fn e1_query_plan_uses_transcript_index() {
    let repo = SqlxRepo::open("sqlite::memory:").await.unwrap();
    let e1 = "SELECT MAX(created_at_ms) FROM harness_items \
              WHERE card_id IN (SELECT id FROM cards WHERE track_id = ?1) \
                AND method = 'turn/completed' \
                AND COALESCE(json_extract(params, '$.status'), '') <> 'interrupted'";
    let details = plan(repo.pool(), e1).await;
    assert!(
        details.iter().any(|detail| detail.contains(INDEX)),
        "E1 must be an index range per card, got plan {details:?}"
    );
    assert!(
        !details.iter().any(|detail| detail.starts_with("SCAN")),
        "E1 must not scan any table, got plan {details:?}"
    );
}

/// The correlated subquery the projection SELECTs and the conversation list
/// embed walks the same index, one range per card.
#[tokio::test]
async fn last_turn_completed_subquery_uses_transcript_index() {
    let repo = SqlxRepo::open("sqlite::memory:").await.unwrap();
    let sql = format!(
        "SELECT c.id, {LAST_TURN_COMPLETED_MS_SUBQUERY} AS last_turn_completed_ms \
           FROM cards c WHERE c.track_id = ?1"
    );
    let details = plan(repo.pool(), &sql).await;
    assert!(
        details.iter().any(|detail| detail.contains(INDEX)),
        "the subquery must be an index range per card, got plan {details:?}"
    );
}
