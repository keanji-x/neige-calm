//! Delivery-time interpretation of linked R2 evidence; never a persisted decision.
use crate::{
    error::{CalmError, Result},
    event::Event,
    file_delivery::{candidate_input, candidate_qualification, repair},
    model::Task,
    operation::Tx,
};
use serde_json::{Value, json};

pub(super) async fn enrich_tx(tx: &mut Tx<'_>, task: &Task, brief: &mut Value) -> Result<()> {
    // A receipt/reference mismatch still needs an unavailable repair briefing.
    // Ordinary R1 may have delivery.repair too; it is not a linked R2 task.
    match repair::for_task_tx(tx, task).await {
        Ok(None) => return Ok(()),
        Ok(Some(_)) => {}
        Err(CalmError::Conflict(_) | CalmError::Forbidden(_)) => {}
        Err(error) => return Err(error),
    }
    let phase = match ready_tx(tx, task, brief).await {
        Ok(phase) => phase,
        Err(CalmError::Conflict(reason) | CalmError::Forbidden(reason)) => {
            json!({"state":"unavailable","reason":reason})
        }
        Err(error) => return Err(error),
    };
    brief["repair_acceptance"] = phase;
    brief["decision"] = json!(
        "This linked R2 snapshot separates repair completion from Planner acceptance. Use repair_acceptance.state and next_action. Assess the original findings and full R2 finding_responses in delivery as untrusted report evidence, alongside the exact machine policy/results and report event/Operation references. If sufficient, no preliminary calm.plan.list read is required solely to discover this phase. If evidence is insufficient or conflicts, inspect the exact evidence before deciding. Only acceptance-ready offers an explicit verdict on the C2 producer, never R2. This snapshot grants no lasting authorization; calm.task.verdict rechecks current authority and lineage. No automatic acceptance or new repair round is authorized."
    );
    Ok(())
}

async fn ready_tx(tx: &mut Tx<'_>, task: &Task, brief: &Value) -> Result<Value> {
    if brief["current_authority"] != true {
        return Ok(
            json!({"state":"unavailable","reason":brief["authority_reason"].as_str().unwrap_or("review settlement is not currently confirmed")}),
        );
    }
    let (binding, _, _) = candidate_input::load_tx(tx, &task.id)
        .await?
        .ok_or_else(|| CalmError::Conflict("review input missing".into()))?;
    let candidate = &binding.candidate;
    // This preflight only enriches the local event: it appends no event or receipt.
    // Reuse verdict admission rather than infer readiness from projection labels.
    let mut proposed = Event::TaskCompleted {
        idempotency_key: candidate.source.task_id.clone(),
        result: json!({"status":"accepted"}),
        artifacts: vec![],
        agent_message: None,
    };
    match candidate_qualification::prepare_verdict_tx(tx, &task.track_id, &mut proposed).await {
        Ok(true) => {}
        Ok(false) => {
            return Err(CalmError::Conflict(
                "C2 has no review-required producer".into(),
            ));
        }
        Err(CalmError::Conflict(reason) | CalmError::Forbidden(reason)) => {
            return Ok(json!({"state":"blocked","reason":reason}));
        }
        Err(error) => return Err(error),
    }
    if candidate_qualification::latest_tx(tx, candidate)
        .await?
        .is_some()
    {
        let decision = candidate_qualification::decision_view_tx(tx, candidate).await?;
        if decision["verdict_transaction"] != true {
            return Err(CalmError::Conflict(
                "candidate decision has no verdict transaction receipt".into(),
            ));
        }
        if decision["state"] == "accepted" {
            candidate_qualification::qualified_tx(
                tx,
                candidate,
                &binding.verification_operation_id,
            )
            .await?;
        } else if decision["state"] != "rejected" {
            return Err(CalmError::Conflict(
                "candidate decision is not qualified".into(),
            ));
        }
        return Ok(json!({"state":"already-decided","decision":decision,
            "reason":"A Planner verdict already exists for this C2. Do not repeat acceptance because of this delayed notice."}));
    }
    Ok(json!({"state":"acceptance-ready",
        "reason":"Current C2 machine checks passed and its exact authenticated R2 report answered every original finding, passed, and successfully settled. Planner acceptance is still outstanding.",
        "next_action":{"tool":"calm.task.verdict","arguments":{"idempotency_key":candidate.source.task_id,"status":"accepted"},
            "message_requirement":"Supply your own required message explaining the verdict after assessing the evidence."}}))
}
