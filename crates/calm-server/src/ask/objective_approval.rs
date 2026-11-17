//! One exact objective standard, approved by the user through the shared ask transaction.
use super::{StoredAsk, forge_approval};
use crate::error::{CalmError, Result};
use crate::event::{AskAction, AskAnswer, AskDelivery, AskQuestion, Event, EventScope};
use crate::ids::{CardId, TrackId};
use crate::model::CardRole;
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
/// a new ask with its real conversation/session author; repeats reuse its own open ask.
pub(crate) async fn request_tx(
    tx: &mut Transaction<'_, Sqlite>,
    track: &TrackId,
    card: &CardId,
    session: &str,
    key: &ObjectiveKey,
) -> Result<ObjectiveApprovalRead> {
    objective_host::capability::require_tx(tx, track).await?;
    let role: Option<String> =
        sqlx::query_scalar("SELECT role FROM cards WHERE id=? AND track_id=?")
            .bind(card.as_str())
            .bind(track.as_str())
            .fetch_optional(&mut **tx)
            .await?;
    if !role
        .and_then(|role| CardRole::try_from(role).ok())
        .is_some_and(|role| role.capability_role() == CardRole::Planner)
    {
        return Err(CalmError::Forbidden(
            "objective approval needs a conversation on this Track".into(),
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
    let pending: Vec<(i64, String, Option<String>)> = sqlx::query_as(concat!(
        "SELECT r.id,r.actor,ws.card_id FROM events r \
           LEFT JOIN worker_sessions ws ON ws.id=json_extract(r.actor,'$.id') AND ws.track_id=r.scope_track \
           WHERE r.scope_track=?1 AND r.kind='ask.requested' \
           AND json_extract(r.payload,'$.action.kind') IN ('objective_approval','objective_approval_v2') \
           AND json_extract(r.payload,'$.action.objective')=?2 \
           AND json_extract(r.payload,'$.action.standard_hash')=?3 \
           AND ",
        super::ask_open_sql!(),
        " ORDER BY r.id"
    ))
    .bind(track.as_str())
    .bind(key.as_str())
    .bind(hash.as_str())
    .fetch_all(&mut **tx)
    .await?;
    for (id, actor, source_card) in pending {
        let actor = serde_json::from_str(&actor)?;
        if !super::source_target(&actor, source_card.map(Into::into))
            .is_some_and(|target| target.is_for(card, session))
        {
            continue;
        }
        let ask = super::stored_ask_tx(tx, track, id).await?;
        require_contract(&ask)?;
        return Ok(ObjectiveApprovalRead::Awaiting(id));
    }
    // Freeze the complete definition with the ask; presentation bounds must not limit the
    // standard. User-supplied names may themselves exhaust the title budget.
    let detail = serde_json::to_string_pretty(&objective.definition)?;
    let (scope, event) = super::requested_tx(
        tx,
        card,
        vec![AskQuestion {
            detail: Some(detail),
            title: include_str!("../../prompts/objective_approval_title.md")
                .trim_end()
                .into(),
            options: OPTIONS.iter().map(|v| (*v).into()).collect(),
        }],
        AskDelivery::Wake,
        None,
        Some(AskAction::ObjectiveApprovalV2 {
            objective: key.as_str().into(),
            standard_hash: hash.as_str().into(),
        }),
    )
    .await?;
    Ok(ObjectiveApprovalRead::Ask(scope, Box::new(event)))
}
fn require_contract(ask: &StoredAsk) -> Result<()> {
    let invalid = || CalmError::BadRequest("invalid objective approval question contract".into());
    if !forge_approval::is_approval_contract(&ask.questions, ask.delivery, &OPTIONS) {
        return Err(invalid());
    }
    let (key, hash, versioned) = match &ask.action {
        Some(AskAction::ObjectiveApprovalV2 {
            objective,
            standard_hash,
        }) => (objective, standard_hash, true),
        Some(AskAction::ObjectiveApproval {
            objective,
            standard_hash,
        }) => (objective, standard_hash, false),
        _ => return Err(invalid()),
    };
    let question = &ask.questions[0];
    let legacy_json;
    let snapshot = if versioned {
        question.detail.as_deref().ok_or_else(invalid)?
    } else {
        // The original writer put pretty JSON after a heading. Names may themselves
        // contain newlines; the final unindented object start is the snapshot delimiter.
        if question.detail.is_some() {
            return Err(invalid());
        }
        let (_, json) = question.title.rsplit_once("\n{").ok_or_else(invalid)?;
        legacy_json = format!("{{{json}");
        &legacy_json
    };
    let value = serde_json::from_str(snapshot).map_err(|_| invalid())?;
    let definition =
        calm_types::report_blocks::objectives::parse_objective(&value).map_err(|_| invalid())?;
    if definition.key.as_str() != key || standard_hash(&definition).as_str() != hash {
        return Err(invalid());
    }
    if !versioned
        && question.title
            != format!(
                include_str!("../../prompts/objective_approval_legacy_title.md"),
                definition.key,
                definition.title,
                serde_json::to_string_pretty(&definition)?
            )
            .trim_end()
    {
        return Err(invalid());
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
    let Some(
        AskAction::ObjectiveApproval {
            objective,
            standard_hash,
        }
        | AskAction::ObjectiveApprovalV2 {
            objective,
            standard_hash,
        },
    ) = &ask.action
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
