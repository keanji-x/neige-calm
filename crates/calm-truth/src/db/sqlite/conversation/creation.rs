//! Immutable original acceptance metadata. The kernel authenticates its source
//! and owns fresh Track allocation; this record is never an execution grant.
use crate::error::{CalmError, Result};
use calm_types::conversation::{ConversationMessageId, MessageRevisionRef};
use serde::{Deserialize, Serialize};
use sqlx::{Row, Sqlite, Transaction};
use std::num::NonZeroU32;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "version", rename_all = "snake_case", deny_unknown_fields)]
pub enum HumanTrackCreationAck {
    V1 {
        track_id: String,
        planner_card_id: String,
        report_card_id: String,
        origin_runtime_id: String,
        first_message: MessageRevisionRef,
        presentation_sequence: i64,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HumanTrackCreationRecord {
    pub source_user_id: String,
    pub create_request_sha256: String,
    pub first_message_sha256: String,
    pub acknowledgement: HumanTrackCreationAck,
}

pub async fn human_track_creation_get_tx(
    tx: &mut Transaction<'_, Sqlite>,
    area_id: &str,
    raw_key: &str,
) -> Result<Option<HumanTrackCreationRecord>> {
    let row = sqlx::query(
        "SELECT source_user_id,create_request_sha256,first_message_sha256,ack_version,
                track_id,planner_card_id,report_card_id,origin_runtime_id,message_id,
                message_revision,presentation_sequence
         FROM conversation_human_track_creations WHERE area_id=?1 AND idempotency_key=?2",
    )
    .bind(area_id)
    .bind(raw_key)
    .fetch_optional(&mut **tx)
    .await?;
    row.map(|row| {
        if row.try_get::<i64, _>("ack_version")? != 1 {
            return Err(CalmError::Internal(
                "unknown Human creation acknowledgement version".into(),
            ));
        }
        let revision = u32::try_from(row.try_get::<i64, _>("message_revision")?)
            .ok()
            .and_then(NonZeroU32::new)
            .ok_or_else(|| CalmError::Internal("invalid original creation revision".into()))?;
        Ok(HumanTrackCreationRecord {
            source_user_id: row.try_get("source_user_id")?,
            create_request_sha256: row.try_get("create_request_sha256")?,
            first_message_sha256: row.try_get("first_message_sha256")?,
            acknowledgement: HumanTrackCreationAck::V1 {
                track_id: row.try_get("track_id")?,
                planner_card_id: row.try_get("planner_card_id")?,
                report_card_id: row.try_get("report_card_id")?,
                origin_runtime_id: row.try_get("origin_runtime_id")?,
                first_message: MessageRevisionRef {
                    message_id: ConversationMessageId::parse(row.try_get("message_id")?)
                        .map_err(|error| CalmError::Internal(error.to_string()))?,
                    revision,
                },
                presentation_sequence: row.try_get("presentation_sequence")?,
            },
        })
    })
    .transpose()
}

pub async fn human_track_creation_insert_tx(
    tx: &mut Transaction<'_, Sqlite>,
    area_id: &str,
    raw_key: &str,
    record: &HumanTrackCreationRecord,
) -> Result<()> {
    let HumanTrackCreationAck::V1 {
        track_id,
        planner_card_id,
        report_card_id,
        origin_runtime_id,
        first_message,
        presentation_sequence,
    } = &record.acknowledgement;
    sqlx::query(
        "INSERT INTO conversation_human_track_creations
         (area_id,idempotency_key,source_user_id,create_request_sha256,first_message_sha256,
          ack_version,track_id,planner_card_id,report_card_id,origin_runtime_id,
          message_id,message_revision,presentation_sequence,created_at_ms)
         VALUES(?1,?2,?3,?4,?5,1,?6,?7,?8,?9,?10,?11,?12,?13)",
    )
    .bind(area_id)
    .bind(raw_key)
    .bind(&record.source_user_id)
    .bind(&record.create_request_sha256)
    .bind(&record.first_message_sha256)
    .bind(track_id)
    .bind(planner_card_id)
    .bind(report_card_id)
    .bind(origin_runtime_id)
    .bind(first_message.message_id.as_str())
    .bind(i64::from(first_message.revision.get()))
    .bind(presentation_sequence)
    .bind(crate::model::now_ms())
    .execute(&mut **tx)
    .await
    .map_err(crate::error::idempotency_binding_insert_error)?;
    Ok(())
}
