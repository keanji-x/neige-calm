//! Event-linked Planner acceptance. JSON alone is never decision provenance.
use super::{
    candidate::Candidate,
    candidate_input::Binding,
    candidate_review::{self, ReviewEvidence},
    candidate_verify, *,
};
use crate::{db::sqlite::task_get_tx, event::Event, operation::Tx};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Acceptance {
    pub publication_operation_id: String,
    pub policy: calm_types::task_execution::CandidateMachinePolicy,
    pub verification_operation_id: String,
    pub review: ReviewEvidence,
}

/// Read-only acceptance preflight, shared by Planner verdict admission and R2 briefings.
/// Only CardDecisionSink appends the resulting event and records decision provenance.
pub(crate) async fn prepare_verdict_tx(
    tx: &mut Tx<'_>,
    track: &str,
    event: &mut Event,
) -> Result<bool> {
    let id = match event {
        Event::TaskCompleted {
            idempotency_key, ..
        }
        | Event::TaskFailed {
            idempotency_key, ..
        } => idempotency_key.clone(),
        _ => return Ok(false),
    };
    let Some(task) = task_get_tx(tx, &id).await? else {
        return Ok(false);
    };
    let Some(FileDelivery::CandidateProducer { policy, .. }) = selection(&task)? else {
        return Ok(false);
    };
    if policy.reviewer().is_none() {
        return Ok(false);
    }
    if task.track_id != track {
        return Err(conflict("candidate verdict Track mismatch"));
    }
    let current = crate::db::sqlite::task_attempt_current_tx(tx, track, &task.key)
        .await?
        .ok_or_else(|| conflict("candidate verdict attempt missing"))?;
    if current.attempt_id != id {
        return Err(conflict("candidate verdict is for an obsolete attempt"));
    }
    if let Event::TaskCompleted { result, .. } = event {
        if result["status"] != "accepted" {
            return Err(conflict("candidate verdict must explicitly accept"));
        }
        let publication: String = sqlx::query_scalar(
            "SELECT operation_id FROM task_file_candidates WHERE producer_attempt_id=?1",
        )
        .bind(&id)
        .fetch_optional(&mut **tx)
        .await?
        .ok_or_else(|| conflict("waiting for sealed candidate"))?;
        let candidate = super::candidate::load_tx(tx, &publication).await?;
        let verification: String = sqlx::query_scalar("SELECT o.id FROM task_candidate_verification_allocations a JOIN operations o ON o.operation_key=a.operation_key WHERE a.publication_operation_id=?1")
            .bind(publication).fetch_optional(&mut **tx).await?.ok_or_else(|| conflict("waiting for candidate verification"))?;
        candidate_verify::qualified_tx(tx, &verification, &candidate).await?;
        let review = candidate_review::evidence_tx(tx, &candidate, &verification).await?;
        if !review.report.passed {
            return Err(conflict(
                "candidate review has unresolved blocking findings",
            ));
        }
        result["candidate_acceptance"] = serde_json::to_value(Acceptance {
            publication_operation_id: candidate.publication_operation_id.clone(),
            policy: candidate.policy()?.clone(),
            verification_operation_id: verification,
            review,
        })?;
    }
    Ok(true)
}

/// Identical accepted evidence retains its original event ID even if wording changes.
/// Rejection breaks this equivalence; reacceptance cannot revive an older consumer binding.
pub(crate) async fn is_repeat_tx(tx: &mut Tx<'_>, track: &str, event: &Event) -> Result<bool> {
    let Event::TaskCompleted { result, .. } = event else {
        return Ok(false);
    };
    let acceptance: Acceptance = serde_json::from_value(result["candidate_acceptance"].clone())?;
    let candidate = super::candidate::load_tx(tx, &acceptance.publication_operation_id).await?;
    if candidate.source.track_id != track {
        return Err(conflict("candidate decision scope changed"));
    }
    let Some((id, previous)) = latest_tx(tx, &candidate).await? else {
        return Ok(false);
    };
    let Event::TaskCompleted { result: old, .. } = &previous else {
        return Ok(false);
    };
    if old["candidate_acceptance"] != result["candidate_acceptance"] {
        return Ok(false);
    }
    let receipt: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM task_candidate_decisions WHERE event_id=?1)",
    )
    .bind(id)
    .fetch_one(&mut **tx)
    .await?;
    if !receipt {
        return Ok(false);
    }
    validate_decision_tx(
        tx,
        id,
        &previous,
        &candidate,
        &acceptance.verification_operation_id,
    )
    .await?;
    Ok(true)
}

/// Receipt insertion follows the actual gated append in the same transaction.
pub(crate) async fn record_decision_tx(
    tx: &mut Tx<'_>,
    track: &str,
    id: i64,
    event: &Event,
) -> Result<()> {
    let attempt = match event {
        Event::TaskCompleted {
            idempotency_key, ..
        }
        | Event::TaskFailed {
            idempotency_key, ..
        } => idempotency_key,
        _ => return Err(conflict("not a candidate decision")),
    };
    sqlx::query("INSERT INTO task_candidate_decisions(event_id,track_id,producer_attempt_id,event_json) VALUES(?1,?2,?3,?4)")
        .bind(id).bind(track).bind(attempt).bind(serde_json::to_string(event)?).execute(&mut **tx).await?;
    Ok(())
}

pub(crate) async fn latest_tx(
    tx: &mut Tx<'_>,
    candidate: &Candidate,
) -> Result<Option<(i64, Event)>> {
    // Broad Track event selection only invalidates; it cannot authenticate acceptance.
    let row: Option<(i64,String,String)> = sqlx::query_as("SELECT id,kind,payload FROM events WHERE scope_kind='track' AND scope_track=?1 AND kind IN ('task.completed','task.failed') AND json_extract(payload,'$.idempotency_key')=?2 AND json_extract(actor,'$.kind') != 'KernelDispatcher' ORDER BY id DESC LIMIT 1")
        .bind(&candidate.source.track_id).bind(&candidate.source.task_id).fetch_optional(&mut **tx).await?;
    row.map(|(id, kind, raw)| {
        Ok((
            id,
            Event::from_kind_and_payload(&kind, serde_json::from_str(&raw)?)?,
        ))
    })
    .transpose()
}

pub(crate) async fn qualified_tx(
    tx: &mut Tx<'_>,
    candidate: &Candidate,
    verification: &str,
) -> Result<Option<i64>> {
    candidate_verify::qualified_tx(tx, verification, candidate).await?;
    if candidate.policy()?.reviewer().is_none() {
        return Ok(None);
    }
    let (id, event) = latest_tx(tx, candidate)
        .await?
        .ok_or_else(|| conflict("waiting for Planner candidate acceptance"))?;
    validate_decision_tx(tx, id, &event, candidate, verification).await?;
    Ok(Some(id))
}
async fn validate_decision_tx(
    tx: &mut Tx<'_>,
    id: i64,
    event: &Event,
    candidate: &Candidate,
    verification: &str,
) -> Result<()> {
    let receipt: Option<String> = sqlx::query_scalar("SELECT event_json FROM task_candidate_decisions WHERE event_id=?1 AND track_id=?2 AND producer_attempt_id=?3")
        .bind(id).bind(&candidate.source.track_id).bind(&candidate.source.task_id).fetch_optional(&mut **tx).await?;
    if receipt
        .as_deref()
        .map(serde_json::from_str::<Value>)
        .transpose()?
        != Some(serde_json::to_value(event)?)
    {
        return Err(conflict(
            "candidate decision has no matching verdict transaction receipt",
        ));
    }
    let Event::TaskCompleted { result, .. } = event else {
        return Err(conflict("Planner rejected this candidate"));
    };
    if result["status"] != "accepted" {
        return Err(conflict("Planner has not accepted this candidate"));
    }
    let acceptance: Acceptance = serde_json::from_value(result["candidate_acceptance"].clone())
        .map_err(|_| conflict("candidate acceptance has no exact evidence"))?;
    let review = candidate_review::evidence_tx(tx, candidate, verification).await?;
    if acceptance.publication_operation_id != candidate.publication_operation_id
        || acceptance.policy != *candidate.policy()?
        || acceptance.verification_operation_id != verification
        || acceptance.review != review
        || !review.report.passed
    {
        return Err(conflict("candidate acceptance evidence mismatch"));
    }
    Ok(())
}

pub(crate) async fn bind_tx(
    tx: &mut Tx<'_>,
    task: &Task,
    binding: &Binding,
    predecessor: Option<&str>,
) -> Result<()> {
    let Some(current) =
        qualified_tx(tx, &binding.candidate, &binding.verification_operation_id).await?
    else {
        return Ok(());
    };
    let decision = if let Some(previous) = predecessor {
        sqlx::query_scalar(
            "SELECT decision_event_id FROM task_candidate_decision_bindings WHERE attempt_id=?1",
        )
        .bind(previous)
        .fetch_optional(&mut **tx)
        .await?
        .ok_or_else(|| conflict("candidate recovery decision binding missing"))?
    } else {
        current
    };
    if current != decision {
        return Err(conflict("candidate recovery acceptance changed"));
    }
    sqlx::query(
        "INSERT INTO task_candidate_decision_bindings(attempt_id,decision_event_id) VALUES(?1,?2)",
    )
    .bind(&task.id)
    .bind(decision)
    .execute(&mut **tx)
    .await?;
    Ok(())
}
pub(crate) async fn validate_binding_tx(
    tx: &mut Tx<'_>,
    task: &Task,
    binding: &Binding,
) -> Result<()> {
    let Some(current) =
        qualified_tx(tx, &binding.candidate, &binding.verification_operation_id).await?
    else {
        return Ok(());
    };
    let bound: Option<i64> = sqlx::query_scalar(
        "SELECT decision_event_id FROM task_candidate_decision_bindings WHERE attempt_id=?1",
    )
    .bind(&task.id)
    .fetch_optional(&mut **tx)
    .await?;
    if bound != Some(current) {
        return Err(conflict("candidate consumer acceptance changed or missing"));
    }
    Ok(())
}

pub(crate) async fn decision_view_tx(tx: &mut Tx<'_>, candidate: &Candidate) -> Result<Value> {
    let Some((id, event)) = latest_tx(tx, candidate).await? else {
        return Ok(json!({"state":"waiting"}));
    };
    let receipt: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM task_candidate_decisions WHERE event_id=?1)",
    )
    .bind(id)
    .fetch_one(&mut **tx)
    .await?;
    Ok(match event {
        Event::TaskCompleted { result, .. } => {
            json!({"event_id":id,"state":if receipt && result["status"] == "accepted" {"accepted"} else {"unqualified-event"},"verdict_transaction":receipt,"review":result.get("candidate_acceptance").and_then(|v|v.get("review"))})
        }
        Event::TaskFailed { reason, .. } => {
            json!({"event_id":id,"state":"rejected","verdict_transaction":receipt,"reason":reason})
        }
        _ => unreachable!(),
    })
}
