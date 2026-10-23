//! One exact objective standard, approved by the user through the shared ask transaction.
use super::{StoredAsk, forge_approval};
use crate::error::{CalmError, Result};
use crate::event::{AskAction, AskAnswer, AskDelivery, AskQuestion, Event, EventScope};
use crate::ids::{CardId, TrackId};
use calm_truth::objective_host::{self, InputSource, ObjectiveInput};
use objective::ids::ObjectiveKey;
use objective::standard::{Approval, standard_hash};
use sqlx::{Sqlite, Transaction};

const OPTIONS: [&str; 2] = ["批准", "不批准"];

pub(crate) enum ObjectiveApprovalRead {
    Approved,
    Awaiting(i64),
    Ask(EventScope, Box<Event>),
}

/// Reuses the shared ask writer and open predicate, as forge approval does. The caller writes
/// a new ask as this track's Planner in the same transaction; repeats reuse the open ask.
pub(crate) async fn request_tx(
    tx: &mut Transaction<'_, Sqlite>,
    track: &TrackId,
    card: &CardId,
    key: &ObjectiveKey,
) -> Result<ObjectiveApprovalRead> {
    objective_host::capability::require_tx(tx, track).await?;
    let planner: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM cards WHERE id=? AND track_id=? AND role='planner')",
    )
    .bind(card.as_str())
    .bind(track.as_str())
    .fetch_one(&mut **tx)
    .await?;
    if !planner {
        return Err(CalmError::Forbidden(
            "objective approval needs this track's Planner".into(),
        ));
    }
    let (_, state) = objective_host::load_state_tx(tx, track).await?;
    let objective = state
        .objectives
        .get(key)
        .ok_or_else(|| CalmError::NotFound(format!("objective {key}")))?;
    if objective.is_approved() {
        return Ok(ObjectiveApprovalRead::Approved);
    }
    let hash = standard_hash(&objective.definition);
    let pending: Option<i64> = sqlx::query_scalar(concat!(
        "SELECT r.id FROM events r WHERE r.scope_track=?1 AND r.kind='ask.requested' \
           AND json_extract(r.payload,'$.action.kind')='objective_approval' \
           AND json_extract(r.payload,'$.action.objective')=?2 \
           AND json_extract(r.payload,'$.action.standard_hash')=?3 \
           AND json_extract(r.actor,'$.kind')='AiPlanner' \
           AND json_extract(r.actor,'$.id')=?4 AND ",
        super::ask_open_sql!(),
        " ORDER BY r.id LIMIT 1"
    ))
    .bind(track.as_str())
    .bind(key.as_str())
    .bind(hash.as_str())
    .bind(card.as_str())
    .fetch_optional(&mut **tx)
    .await?;
    if let Some(id) = pending {
        let ask = super::stored_ask_tx(tx, track, id).await?;
        require_contract(&ask)?;
        return Ok(ObjectiveApprovalRead::Awaiting(id));
    }
    // Show the complete standard being approved; never truncate a criterion into a different
    // question. Oversized standards are refused by the shared ask bounds.
    let title = format!(
        "批准 KR {}「{}」的细则？\n{}",
        key,
        objective.definition.title,
        serde_json::to_string_pretty(&objective.definition)?
    );
    let (scope, event) = super::requested_tx(
        tx,
        card,
        vec![AskQuestion {
            title,
            options: OPTIONS.iter().map(|v| (*v).into()).collect(),
        }],
        AskDelivery::Wake,
        None,
        Some(AskAction::ObjectiveApproval {
            objective: key.as_str().into(),
            standard_hash: hash.as_str().into(),
        }),
    )
    .await?;
    Ok(ObjectiveApprovalRead::Ask(scope, Box::new(event)))
}
fn require_contract(ask: &StoredAsk) -> Result<()> {
    if !forge_approval::is_approval_contract(&ask.questions, ask.delivery, &OPTIONS) {
        return Err(CalmError::BadRequest(
            "invalid objective approval question contract".into(),
        ));
    }
    Ok(())
}

/// ask.answered is appended after this closure returns. The host proves the still-open ask's
/// identity and standard; calm-server alone interprets the answer, just like forge approval.
pub(super) async fn answer_events_tx(
    tx: &mut Transaction<'_, Sqlite>,
    track: &TrackId,
    ask_id: i64,
    ask: &StoredAsk,
    answers: &[AskAnswer],
) -> Result<()> {
    let Some(AskAction::ObjectiveApproval {
        objective,
        standard_hash,
    }) = &ask.action
    else {
        return Ok(());
    };
    require_contract(ask)?;
    if !forge_approval::grants(answers) {
        return Ok(());
    }
    objective_host::react_tx(
        tx,
        &ObjectiveInput {
            track_id: track.clone(),
            source: InputSource::AskAnswered { ask_id },
            event: objective::Event::ApprovalGranted {
                objective: ObjectiveKey::new(objective),
                approval: Approval {
                    standard_hash: serde_json::from_value(serde_json::Value::String(
                        standard_hash.clone(),
                    ))?,
                    approver: format!("ask:{ask_id}"),
                },
            },
        },
        crate::model::now_ms(),
    )
    .await?;
    Ok(())
}
