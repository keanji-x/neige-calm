//! Resolve and authenticate a Report target under the writer lock.
use super::ReportEditTarget;
use crate::error::CalmError;
use crate::event::EditAuthor;
use crate::mcp_server::registry::ToolCallIdentity;
use crate::model::CardRole;
use calm_truth::decision_gate::{GateDecision, PrincipalDecisionGate};
use sqlx::{Sqlite, Transaction};

pub(crate) enum Access {
    Read,
    Write,
}

pub(crate) async fn resolve_tx(
    tx: &mut Transaction<'_, Sqlite>,
    identity: &ToolCallIdentity,
    access: Access,
) -> Result<ReportEditTarget, CalmError> {
    let caller = crate::db::sqlite::card_for_update_tx(tx, &identity.card_id).await?;
    if matches!(access, Access::Write) {
        use crate::recorder_admission::{
            CardDecisionSinkRecorderAdmissionGate, RecorderAdmissionDecisionKind,
            RecorderAdmissionGate,
        };
        CardDecisionSinkRecorderAdmissionGate::for_identity(identity, caller.track_id.clone())
            .require_admission(tx, RecorderAdmissionDecisionKind::ReportWrite)
            .await?;
    }
    let principal = identity
        .to_principal()
        .ok_or_else(|| CalmError::Forbidden("agent session required".into()))?;
    match PrincipalDecisionGate::new(principal)
        .decide_card_write(tx, &caller.track_id, &caller.id, identity.role)
        .await?
    {
        GateDecision::Allow => (),
        GateDecision::Deny(reason) => return Err(CalmError::Forbidden(reason)),
    }
    if !matches!(
        identity.role,
        crate::model::CardRole::Planner | crate::model::CardRole::Assistant
    ) {
        return Err(CalmError::Forbidden(
            "Report requires a Planner or Assistant".into(),
        ));
    }
    let track = crate::db::sqlite::track_get_tx(tx, &caller.track_id).await?;
    let report_id: String =
        sqlx::query_scalar("SELECT id FROM cards WHERE track_id=? AND kind='track-report'")
            .bind(track.id.as_str())
            .fetch_one(&mut **tx)
            .await?;
    let report_card = crate::db::sqlite::card_for_update_tx(tx, &report_id).await?;
    let current_payload = serde_json::from_value(report_card.payload.clone())
        .map_err(|e| CalmError::Internal(format!("Report payload: {e}")))?;
    ReportEditTarget::for_resolved_parts(track, report_card, current_payload)
}

/// The role → attribution decision for every write reaching [`crate::decision_sink::CardDecisionSink::commit_report_op`].
/// Exhaustive on purpose: a future role must state its own verdict rather than inherit the planner's. `Worker` and `ReportCard` are refused outright rather than folded in with `Planner`.
pub(crate) fn report_op_attribution(role: CardRole) -> Result<EditAuthor, CalmError> {
    Ok(match role {
        CardRole::Assistant => EditAuthor::Assistant,
        CardRole::Planner => EditAuthor::Planner,
        role @ (CardRole::Worker | CardRole::ReportCard) => {
            return Err(CalmError::Forbidden(format!(
                "card role {role:?} may not write the track report"
            )));
        }
    })
}
