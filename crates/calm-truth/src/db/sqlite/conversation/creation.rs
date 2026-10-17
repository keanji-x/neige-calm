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

/// Only facts added to the permanent resource/fingerprint binding at acceptance.
/// No caller supplies a second copy of the original resource IDs or hashes.
pub struct HumanTrackCreationAcceptance {
    pub source_user_id: String,
    pub origin_runtime_id: String,
    pub first_message: MessageRevisionRef,
    pub presentation_sequence: i64,
}

pub async fn human_track_creation_get_tx(
    tx: &mut Transaction<'_, Sqlite>,
    area_id: &str,
    raw_key: &str,
) -> Result<Option<HumanTrackCreationRecord>> {
    let row = sqlx::query(
        "SELECT c.source_user_id,b.create_request_sha256,b.first_message_sha256,c.ack_version,
                b.track_id,b.planner_card_id,b.report_card_id,c.origin_runtime_id,c.message_id,
                c.message_revision,c.presentation_sequence,b.request_fingerprint_version
         FROM conversation_human_track_creations c
         LEFT JOIN track_create_idempotency b
           ON b.area_id=c.area_id AND b.idempotency_key=c.idempotency_key
         WHERE c.area_id=?1 AND c.idempotency_key=?2",
    )
    .bind(area_id)
    .bind(raw_key)
    .fetch_optional(&mut **tx)
    .await?;
    row.map(|row| {
        if row.try_get::<Option<i64>, _>("request_fingerprint_version")? != Some(1) {
            return Err(CalmError::Internal(
                "canonical creation has no original message-carrying binding".into(),
            ));
        }
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
    acceptance: &HumanTrackCreationAcceptance,
) -> Result<HumanTrackCreationRecord> {
    sqlx::query(
        "INSERT INTO conversation_human_track_creations
         (area_id,idempotency_key,source_user_id,ack_version,origin_runtime_id,
          message_id,message_revision,presentation_sequence)
         VALUES(?1,?2,?3,1,?4,?5,?6,?7)",
    )
    .bind(area_id)
    .bind(raw_key)
    .bind(&acceptance.source_user_id)
    .bind(&acceptance.origin_runtime_id)
    .bind(acceptance.first_message.message_id.as_str())
    .bind(i64::from(acceptance.first_message.revision.get()))
    .bind(acceptance.presentation_sequence)
    .execute(&mut **tx)
    .await
    .map_err(crate::error::idempotency_binding_insert_error)?;
    // Both fresh acceptance and replay use this one ACK assembly owner. LEFT
    // JOIN above turns a missing/corrupt parent into an error, never a vacant key.
    human_track_creation_get_tx(tx, area_id, raw_key)
        .await?
        .ok_or_else(|| CalmError::Internal("accepted Human creation receipt is missing".into()))
}
