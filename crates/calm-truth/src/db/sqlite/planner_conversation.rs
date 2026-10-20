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
        const COLUMNS: &str = "id, worker_session_id, card_id, track_id, thread_id, turn_id, \
                               item_uuid, item_type, method, params, input_segments, \
                               created_at_ms";
        let (comparison, order, cursor) = if descending {
            ("<", "DESC", if after_id == 0 { i64::MAX } else { after_id })
        } else {
            (">", "ASC", after_id)
        };
        let sql = format!(
            "SELECT {COLUMNS} FROM harness_items \
             WHERE card_id = ?1 AND id {comparison} ?2{method_predicate} \
               AND id > ?4 AND (?5 IS NULL OR id <= ?5) \
             ORDER BY id {order} LIMIT ?3"
        );
        let mut rows = sqlx::query_as::<_, crate::db::rows::HarnessItemRow>(&sql)
            .bind(card_id)
            .bind(cursor)
            .bind(limit)
            .bind(range.floor)
            .bind(range.ceiling)
            .fetch_all(self.db.read_pool())
            .await?;
        if descending {
            rows.reverse();
        }
        rows.into_iter()
            .map(HarnessItem::try_from)
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(|error| {
                CalmError::Internal(format!("transcript input_segments decode: {error}"))
            })
    }

    /// The range of conversation `ordinal`, or of the current one for `None`; `None` when the
    /// card has no such conversation.
    async fn planner_conversation_range(
        &self,
        card_id: &str,
        ordinal: Option<i64>,
    ) -> Result<Option<ItemRange>> {
        let floors: Vec<(i64, i64)> = sqlx::query_as(
            "SELECT ordinal, after_item_id FROM planner_conversation_starts \
              WHERE card_id=?1 ORDER BY ordinal",
        )
        .bind(card_id)
        .fetch_all(self.db.read_pool())
        .await?;
        let current = floors.last().map_or(0, |(ordinal, _)| *ordinal);
        let ordinal = ordinal.unwrap_or(current);
        if !(0..=current).contains(&ordinal) {
            return Ok(None);
        }
        let floor_of = |k: i64| {
            floors
                .iter()
                .find(|(ordinal, _)| *ordinal == k)
                .map(|(_, after)| *after)
        };
        Ok(Some(ItemRange {
            floor: if ordinal == 0 {
                0
            } else {
                floor_of(ordinal).unwrap_or(0)
            },
            ceiling: floor_of(ordinal + 1),
        }))
    }

    pub(super) async fn planner_conversation_transcript_page(
        &self,
        card_id: &str,
        conversation: Option<i64>,
        after_id: i64,
        limit: i64,
        descending: bool,
    ) -> Result<Option<Vec<HarnessItem>>> {
        let Some(range) = self
            .planner_conversation_range(card_id, conversation)
            .await?
        else {
            return Ok(None);
        };
        Ok(Some(
            self.transcript_page(
                card_id,
                after_id,
                limit,
                descending,
                TRANSCRIPT_METHOD_PREDICATE,
                range,
            )
            .await?,
        ))
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
