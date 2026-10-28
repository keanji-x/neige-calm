//! Runtime eligibility for registered original Assistants, composed with Planner tenure.
use super::*;
use crate::db::sqlite::conversation;
use crate::error::{CalmError, Result};
use calm_types::system_assistant::SystemAssistantSource;
use sqlx::{Sqlite, Transaction};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct AssistantAdmission {
    pub card: CardId,
    pub track: TrackId,
    pub worker: String,
    pub baseline: i64,
    pub source: SystemAssistantSource,
}
impl AssistantAdmission {
    pub(super) async fn load_tx(tx: &mut Transaction<'_, Sqlite>, card: &str) -> Result<Self> {
        let (track, worker, baseline, source): (String, String, i64, String) = sqlx::query_as(
            "SELECT e.track_id,e.original_worker,e.creation_floor,e.source
             FROM conversation_system_assistant_entries e JOIN cards c ON c.id=e.card_id
             JOIN tracks t ON t.id=c.track_id
             WHERE e.card_id=?1 AND c.track_id=e.track_id AND c.session_id=e.original_worker
               AND c.role='assistant' AND t.closed_at IS NULL
               AND (t.purpose IS NULL OR t.purpose<>?2)",
        )
        .bind(card)
        .bind(crate::AREA_CHAT_PURPOSE)
        .fetch_optional(&mut **tx)
        .await?
        .ok_or_else(|| {
            CalmError::Forbidden("registered Assistant target is no longer current".into())
        })?;
        let source = serde_json::from_value(serde_json::Value::String(source))?;
        crate::db::sqlite::require_conversation_unarchived_tx(tx, card).await?;
        let carrier = conversation::current_input_carrier_tx(tx, card, &worker).await?;
        let (binding, _) = crate::harness::profile::input_carrier_binding_tx(tx, &carrier).await?;
        if binding.profile != crate::harness::profile::HarnessProfile::Assistant
            || binding.provider != crate::session_projection_repo::AgentProvider::Codex
        {
            return Err(CalmError::Forbidden(
                "registered Assistant binding changed".into(),
            ));
        }
        Ok(Self {
            card: card.into(),
            track: track.into(),
            worker,
            baseline,
            source,
        })
    }
}

#[derive(Clone)]
pub(super) enum EventAdmission {
    Planner(planner_admission::PlannerAdmission),
    Assistant(AssistantAdmission),
}
impl EventAdmission {
    pub(super) async fn load_tx(tx: &mut Transaction<'_, Sqlite>, card: &str) -> Result<Self> {
        let targeted: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM conversation_system_assistant_entries WHERE card_id=?1)",
        )
        .bind(card)
        .fetch_one(&mut **tx)
        .await?;
        if targeted {
            Ok(Self::Assistant(
                AssistantAdmission::load_tx(tx, card).await?,
            ))
        } else {
            Ok(Self::Planner(
                planner_admission::PlannerAdmission::load_tx(tx, card).await?,
            ))
        }
    }
    pub(super) async fn authorize_tx(&self, tx: &mut Transaction<'_, Sqlite>) -> Result<()> {
        match self {
            Self::Planner(value) => value.authorize_tx(tx).await,
            Self::Assistant(value) => {
                if AssistantAdmission::load_tx(tx, value.card.as_str()).await? != *value {
                    return Err(CalmError::Forbidden("registered Assistant changed".into()));
                }
                Ok(())
            }
        }
    }
    pub(super) fn card(&self) -> &CardId {
        match self {
            Self::Planner(value) => &value.card,
            Self::Assistant(value) => &value.card,
        }
    }
    pub(super) fn worker(&self) -> &str {
        match self {
            Self::Planner(v) => &v.worker,
            Self::Assistant(v) => &v.worker,
        }
    }
    pub(super) fn track(&self) -> &TrackId {
        match self {
            Self::Planner(v) => &v.track,
            Self::Assistant(v) => &v.track,
        }
    }
    pub(super) fn baseline(&self) -> i64 {
        match self {
            Self::Planner(v) => v.baseline,
            Self::Assistant(v) => v.baseline,
        }
    }
    pub(super) fn kinds(&self) -> &[&'static str] {
        match self {
            Self::Planner(_) => &PLANNER_CATCH_UP_KINDS,
            Self::Assistant(_) => TARGETED_ASSISTANT_KINDS,
        }
    }
    pub(super) async fn qualify(
        &self,
        repo: &dyn Repo,
        fact: &calm_truth::db::sqlite::event_inputs::EventInputFact,
    ) -> Result<Option<HarnessObservation>> {
        match self {
            Self::Planner(v) => {
                qualification::qualified_observation(repo, &v.track, &fact.envelope).await
            }
            Self::Assistant(v) => match &fact.envelope.event {
                Event::AssistantInputRequested { card_id, text, .. } if *card_id == v.card => {
                    Ok(Some(HarnessObservation::SystemContext {
                        text: text.clone(),
                    }))
                }
                Event::AssistantInputRequested {
                    card_id,
                    source,
                    subject,
                    ..
                } => {
                    // A foreign target is skippable only as an original accepted fact.
                    // Its ACK grants no source or execution authority on this carrier.
                    let db = repo.sqlite_db().ok_or_else(|| {
                        CalmError::Internal("targeted prefix requires SQLite".into())
                    })?;
                    let mut tx = db.read_pool().begin().await?;
                    let original = conversation::system_assistant_receipt_tx(
                        &mut tx,
                        card_id.as_str(),
                        *source,
                        subject,
                    )
                    .await?;
                    tx.rollback().await?;
                    if original.is_some_and(|receipt| receipt.event_id == fact.envelope.id) {
                        Ok(None)
                    } else {
                        Err(CalmError::Forbidden(
                            "unconfirmed foreign targeted source holds this prefix".into(),
                        ))
                    }
                }
                _ => Err(CalmError::Forbidden("targeted prefix kind changed".into())),
            },
        }
    }
}
