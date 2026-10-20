//! Planner conversation floors (#2774) and the transcript pages they bound. Conversation 0 is
//! implicit; conversation `k` holds the card's rows with `after_item_id(k) < id <=
//! after_item_id(k+1)`, and the highest ordinal is current and unbounded above. `harness_items.id`
//! is `AUTOINCREMENT`, so every row written after a floor falls in the conversation it starts.
use sqlx::{Sqlite, Transaction};

use super::SqlxRepo;
use crate::error::{CalmError, Result};
use crate::model::HarnessItem;
use calm_types::planner_conversation::PlannerConversation;

/// Explicit allowlist of `harness_items` methods a transcript can render.
/// Spliced, not bound: a fixed literal with no caller input.
pub(super) const TRANSCRIPT_METHOD_PREDICATE: &str =
    " AND method IN ('item/started', 'item/completed', 'turn/completed')";

/// The transcript ids one conversation holds: `floor < id <= ceiling`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct ItemRange {
    floor: i64,
    ceiling: Option<i64>,
}

impl ItemRange {
    /// Every row of the card, whatever its conversation.
    pub(super) const ALL: Self = Self {
        floor: 0,
        ceiling: None,
    };
}

/// Start the card's next conversation at the card's newest transcript row, read in the caller's
/// write transaction so no row can land between the read and the floor. Returns its ordinal.
pub async fn planner_conversation_start_tx(
    tx: &mut Transaction<'_, Sqlite>,
    card_id: &str,
    now_ms: i64,
) -> Result<i64> {
    Ok(sqlx::query_scalar(
        "INSERT INTO planner_conversation_starts(card_id,ordinal,after_item_id,started_at_ms) \
         SELECT ?1, COALESCE(MAX(ordinal),0)+1, \
                (SELECT COALESCE(MAX(id),0) FROM harness_items WHERE card_id=?1), ?2 \
           FROM planner_conversation_starts WHERE card_id=?1 RETURNING ordinal",
    )
    .bind(card_id)
    .bind(now_ms)
    .fetch_one(&mut **tx)
    .await?)
}

impl SqlxRepo {
    pub(super) async fn transcript_page(
        &self,
        card_id: &str,
        after_id: i64,
        limit: i64,
        descending: bool,
        method_predicate: &str,
        range: ItemRange,
    ) -> Result<Vec<HarnessItem>> {
        let sql = format!(
            "WITH selected AS (SELECT ?4 AS floor, ?5 AS ceiling) {}",
            transcript_page_sql(descending, method_predicate)
        );
        let rows = sqlx::query_as::<_, crate::db::rows::HarnessItemRow>(&sql)
            .bind(card_id)
            .bind(page_cursor(after_id, descending))
            .bind(limit)
            .bind(range.floor)
            .bind(range.ceiling)
            .fetch_all(self.db.read_pool())
            .await?;
        decode_page(rows, descending)
    }

    pub(super) async fn planner_conversation_transcript_page(
        &self,
        card_id: &str,
        conversation: Option<i64>,
        after_id: i64,
        limit: i64,
        descending: bool,
    ) -> Result<Option<(i64, Vec<HarnessItem>)>> {
        use sqlx::{FromRow, Row};
        // Bounds, ordinal and rows share one statement snapshot, including an empty page.
        // No deferred transaction parks holding shared-cache read locks (#930).
        let order = if descending { "DESC" } else { "ASC" };
        let sql = format!(
            "WITH bounds AS ( \
               SELECT 0 AS ordinal, 0 AS after_item_id \
               UNION ALL SELECT ordinal, after_item_id FROM planner_conversation_starts WHERE card_id=?1), \
             spans AS (SELECT ordinal, after_item_id AS floor, \
               LEAD(after_item_id) OVER (ORDER BY ordinal) AS ceiling FROM bounds), \
             selected AS (SELECT * FROM spans WHERE ordinal=COALESCE(?4,(SELECT MAX(ordinal) FROM spans))), \
             page AS ({}) \
             SELECT selected.ordinal AS conversation_ordinal, page.* \
               FROM selected LEFT JOIN page ON 1=1 ORDER BY page.id {order}",
            transcript_page_sql(descending, TRANSCRIPT_METHOD_PREDICATE)
        );
        let rows = sqlx::query(&sql)
            .bind(card_id)
            .bind(page_cursor(after_id, descending))
            .bind(limit)
            .bind(conversation)
            .fetch_all(self.db.read_pool())
            .await?;
        #[cfg(feature = "fixtures")]
        crate::test_seam::pause_point("planner_conversation_range_read", card_id).await;
        let Some(first) = rows.first() else {
            return Ok(None);
        };
        let ordinal = first.try_get("conversation_ordinal")?;
        let mut items = Vec::with_capacity(rows.len());
        for row in rows {
            // LEFT JOIN preserves the selected ordinal when the requested page has no row.
            if row.try_get::<Option<i64>, _>("id")?.is_some() {
                items.push(crate::db::rows::HarnessItemRow::from_row(&row)?);
            }
        }
        Ok(Some((ordinal, decode_page(items, descending)?)))
    }

    pub(super) async fn planner_conversation_history(
        &self,
        card_id: &str,
    ) -> Result<Vec<PlannerConversation>> {
        // Each earlier conversation is bounded by the next floor; only one holding a row the
        // transcript renders is listed, so `/planner/reset`'s delete leaves none behind. The
        // spliced predicate's bare `method` can only name `harness_items`.
        let sql = format!(
            "WITH bounds AS ( \
               SELECT 0 AS ordinal, 0 AS after_item_id, NULL AS started_at_ms \
               UNION ALL SELECT ordinal, after_item_id, started_at_ms \
                 FROM planner_conversation_starts WHERE card_id=?1), \
             spans AS ( \
               SELECT b.ordinal, b.after_item_id AS floor, b.started_at_ms, \
                      n.after_item_id AS ceiling, n.started_at_ms AS ended_at_ms \
                 FROM bounds b JOIN bounds n ON n.ordinal = b.ordinal + 1) \
             SELECT s.ordinal, COALESCE(s.started_at_ms, MIN(h.created_at_ms)), s.ended_at_ms \
               FROM spans s JOIN harness_items h ON h.card_id=?1 \
                AND h.id > s.floor AND h.id <= s.ceiling{TRANSCRIPT_METHOD_PREDICATE} \
              GROUP BY s.ordinal ORDER BY s.ordinal"
        );
        let rows: Vec<(i64, i64, i64)> = sqlx::query_as(&sql)
            .bind(card_id)
            .fetch_all(self.db.read_pool())
            .await?;
        Ok(rows
            .into_iter()
            .map(
                |(ordinal, started_at_ms, ended_at_ms)| PlannerConversation {
                    ordinal,
                    started_at_ms,
                    ended_at_ms,
                },
            )
            .collect())
    }
}

fn page_cursor(after_id: i64, descending: bool) -> i64 {
    if descending && after_id == 0 {
        i64::MAX
    } else {
        after_id
    }
}

/// Both card-wide and bounded readers use the same transcript selection and ordering.
fn transcript_page_sql(descending: bool, method_predicate: &str) -> String {
    let (comparison, order) = if descending {
        ("<", "DESC")
    } else {
        (">", "ASC")
    };
    format!(
        "SELECT id, worker_session_id, card_id, track_id, thread_id, turn_id, \
                item_uuid, item_type, method, params, input_segments, created_at_ms \
           FROM harness_items JOIN selected ON id > floor AND (ceiling IS NULL OR id <= ceiling) \
          WHERE card_id=?1 AND id {comparison} ?2{method_predicate} \
          ORDER BY id {order} LIMIT ?3"
    )
}

fn decode_page(
    mut rows: Vec<crate::db::rows::HarnessItemRow>,
    descending: bool,
) -> Result<Vec<HarnessItem>> {
    if descending {
        rows.reverse();
    }
    rows.into_iter()
        .map(HarnessItem::try_from)
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(|error| CalmError::Internal(format!("transcript input_segments decode: {error}")))
}
