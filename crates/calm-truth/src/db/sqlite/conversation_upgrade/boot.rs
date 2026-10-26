//! A conservative boot fence, not U13-B's inventory or migration qualifier.
//! Lifecycle selection is intentionally absent: closed/archived cards, retired
//! carriers and consumed keys carry the same persistent format facts.
use super::super::{SqliteDb, begin_immediate_tx, conversation};
use super::state::{self, refusal};
use crate::error::Result;
use calm_types::harness::snapshot::CanonicalHarnessSnapshot;

type NativeHistoryRow = (
    String,
    String,
    Option<String>,
    Option<String>,
    Option<String>,
    String,
    String,
);

pub(crate) async fn assert_ready(db: &SqliteDb) -> Result<()> {
    let mut tx = begin_immediate_tx(db).await?;
    if let Some(marker) = state::read(&mut tx).await? {
        let deployment: Option<String> =
            sqlx::query_scalar("SELECT id FROM database_identity WHERE singleton=1")
                .fetch_optional(&mut **tx)
                .await?;
        marker.validate(deployment.as_deref().unwrap_or(""))?;
        return Err(refusal("offline preparation is required/incomplete"));
    }

    // Prove canonical frames using their owning released decoder before the
    // canonical revision backfill gets a chance to mutate anything. No V1
    // decoding, defaulting or qualification is provided here.
    let frames: Vec<(String, Option<String>)> = sqlx::query_as(
        "SELECT ws.id,ws.handle_state_json FROM worker_sessions ws
         LEFT JOIN cards c ON c.id=ws.card_id
         WHERE ws.contract IN ('planner','assistant') OR c.role IN ('planner','assistant')",
    )
    .fetch_all(&mut **tx)
    .await?;
    for (worker, raw) in frames {
        let frame = raw
            .as_deref()
            .and_then(|raw| serde_json::from_str(raw).ok())
            .and_then(|value| CanonicalHarnessSnapshot::from_released(value).ok());
        if frame.is_none() {
            return Err(refusal(format!(
                "marker missing for historical/unproven carrier {worker}"
            )));
        }
    }

    // Creation receipts grant no format proof. All surviving frames were
    // checked above; a card without one needs actual canonical content.
    let unproven: Option<String> = sqlx::query_scalar(
        "SELECT c.id FROM cards c WHERE c.role IN ('planner','assistant')
         AND NOT EXISTS(SELECT 1 FROM worker_sessions WHERE card_id=c.id)
         AND NOT EXISTS(SELECT 1 FROM conversation_messages WHERE card_id=c.id)
         AND NOT EXISTS(SELECT 1 FROM conversation_turns WHERE card_id=c.id)
         AND NOT EXISTS(SELECT 1 FROM conversation_presentations WHERE card_id=c.id)
         AND NOT EXISTS(SELECT 1 FROM planner_input_idempotency
             WHERE card_id=c.id AND payload_hash LIKE ?1) LIMIT 1",
    )
    .bind(format!(
        "{}%",
        super::super::CANONICAL_PLANNER_INPUT_BINDING_PREFIX
    ))
    .fetch_optional(&mut **tx)
    .await?;
    if let Some(card) = unproven {
        return Err(refusal(format!(
            "marker missing for historical/unproven card {card}"
        )));
    }

    // A canonical current frame alone cannot prove that raw history was already
    // indexed. Use the presentation owner's source identity (not text equality
    // or a second native identity algorithm); started/completed revisions share
    // that identity even when only the latest raw row is referenced.
    let native: Vec<NativeHistoryRow> = sqlx::query_as(
        "SELECT h.card_id,h.thread_id,h.item_uuid,h.item_type,h.turn_id,h.method,h.params
            FROM harness_items h JOIN cards c ON c.id=h.card_id
            WHERE c.role IN ('planner','assistant')",
    )
    .fetch_all(&mut **tx)
    .await?;
    for (card, thread, item, kind, turn, method, raw) in native {
        let params = serde_json::from_str(&raw).map_err(|_| refusal("malformed native history"))?;
        // The terminal recorder stores a turn fact, not a presentation item.
        // Keep this exact shape separate; unknown/no-identity item rows still fail.
        if method == "turn/completed"
            && turn.is_some()
            && !thread.is_empty()
            && item.is_none()
            && kind.is_none()
        {
            continue;
        }
        let identity = conversation::presentation::native_source_identity(
            &thread,
            item.as_deref(),
            kind.as_deref(),
            &method,
            &params,
        )
        .map_err(|_| refusal("unproven native history requires offline upgrade"))?;
        let indexed: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM conversation_presentations
            WHERE card_id=?1 AND source_kind='native_item' AND source_identity=?2)",
        )
        .bind(&card)
        .bind(identity)
        .fetch_one(&mut **tx)
        .await?;
        if !indexed {
            return Err(refusal("unindexed native history requires offline upgrade"));
        }
    }

    // Positive canonical ownership is required for evidence whose resource may
    // have gone away. In particular a canonical current card cannot hide a V1
    // receipt or a retired carrier. Worker/PlainChat evidence remains outside
    // this upgrade when its actual owning card/session proves that scope.
    super::evidence::assert_formats(&mut tx).await?;
    let historical: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM worker_sessions ws LEFT JOIN cards c ON c.id=ws.card_id
             WHERE (ws.contract IN ('planner','assistant') OR c.role IN ('planner','assistant'))
               AND (c.id IS NULL OR c.track_id IS NOT ws.track_id
                    OR c.role NOT IN ('planner','assistant') OR ws.mode!='resumable'
                    OR ws.contract NOT IN ('planner','assistant')))
         OR EXISTS(SELECT 1 FROM harness_items h
             LEFT JOIN cards c ON c.id=h.card_id
             LEFT JOIN worker_sessions ws ON ws.id=h.worker_session_id
             WHERE c.id IS NULL OR (c.role IN ('planner','assistant')
                 AND (ws.id IS NULL OR ws.card_id IS NOT h.card_id OR ws.track_id IS NOT h.track_id)))
         OR EXISTS(SELECT 1 FROM acp_managed_sessions a
             LEFT JOIN worker_sessions ws ON ws.id=a.worker_session_id WHERE ws.id IS NULL)
         OR EXISTS(SELECT 1 FROM acp_submissions a
             LEFT JOIN worker_sessions ws ON ws.id=a.worker_session_id WHERE ws.id IS NULL)
        ",
    )
    .fetch_one(&mut **tx).await?;
    if historical {
        return Err(refusal(
            "marker missing for historical/unproven deployment evidence",
        ));
    }
    tx.commit().await?;
    Ok(())
}
