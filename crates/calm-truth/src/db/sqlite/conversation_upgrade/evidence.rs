//! Format evidence whose journal may outlive its original resource.
use super::super::{AcpSubmissionInput, CANONICAL_PLANNER_INPUT_BINDING_PREFIX, session_get_tx};
use super::state::refusal;
use crate::{decision_gate::WriteTx, error::Result, planner_binding::PlannerBinding};
use calm_types::{
    harness::{HarnessProfile, retained::RetainedPayload},
    ids::CardId,
    worker::WorkerSessionId,
};
use sqlx::{Sqlite, Transaction};

pub(super) async fn assert_formats(tx: &mut Transaction<'_, Sqlite>) -> Result<()> {
    let receipts: Vec<(String, String)> = sqlx::query_as(
        "SELECT card_id,worker_session_id FROM planner_input_idempotency WHERE payload_hash NOT LIKE ?1"
    ).bind(format!("{CANONICAL_PLANNER_INPUT_BINDING_PREFIX}%")).fetch_all(&mut **tx).await?;
    for (card, worker) in receipts {
        let card = CardId::from(card);
        let session = session_get_tx(tx, &WorkerSessionId::from(worker)).await?;
        let shape = tx.read_card_binding(&card).await?;
        let role = tx.read_card_role(&card).await?;
        let outside =
            if let (Some(session), Some((kind, payload, _)), Some(role)) = (session, shape, role) {
                PlannerBinding::from_shape(&kind, role, &payload).is_some_and(|binding| {
                    binding.profile == HarnessProfile::PlainChat
                        && binding.matches_session_contract(&session)
                        && session.card_id.as_ref() == Some(&card)
                }) && tx.read_card_track(&card).await?.as_ref() == Some(&session.track_id)
            } else {
                false
            };
        if !outside {
            return Err(refusal("unproven historical send receipt"));
        }
    }
    let inputs: Vec<String> = sqlx::query_scalar("SELECT input_json FROM acp_submissions")
        .fetch_all(&mut **tx)
        .await?;
    for input in inputs {
        if !matches!(
            AcpSubmissionInput::decode(&input),
            Ok(AcpSubmissionInput::Canonical(_))
        ) {
            return Err(refusal("historical/unproven ACP input"));
        }
    }
    let operations: Vec<(String, String)> = sqlx::query_as(
        "SELECT payload_json,phase FROM operations WHERE kind='planner-harness-start'",
    )
    .fetch_all(&mut **tx)
    .await?;
    for (raw, phase) in operations {
        let value: serde_json::Value = serde_json::from_str(&raw)?;
        // The retained owner discriminates first, including malformed/future wires.
        let wire = match RetainedPayload::decode(&value)
            .map_err(|_| refusal("historical/unproven activation format"))?
        {
            Some(wire) => wire,
            None if value.get("profile")
                == Some(&serde_json::to_value(HarnessProfile::PlainChat)?) =>
            {
                continue;
            }
            None => return Err(refusal("historical/unproven activation format")),
        };
        if phase == "succeeded" {
            continue;
        }
        let bound: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM worker_sessions ws JOIN cards c ON c.id=ws.card_id
             WHERE ws.id=?1 AND c.id=?2 AND c.role IN ('planner','assistant'))",
        )
        .bind(&wire.worker_session_id)
        .bind(&wire.card_id)
        .fetch_one(&mut **tx)
        .await?;
        if !bound {
            return Err(refusal("unproven unresolved activation resource"));
        }
    }
    Ok(())
}
