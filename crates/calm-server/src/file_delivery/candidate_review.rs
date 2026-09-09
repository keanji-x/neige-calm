//! Semantic evidence uses the exact review input and authenticated execution report.
use super::candidate_input::BindingPurpose;
use super::{candidate::Candidate, candidate_input, candidate_verify, *};
use crate::{
    db::sqlite::{task_attempt_current_tx, task_get_tx},
    event::Event,
    operation::Tx,
};
use calm_types::task_execution::CandidateReviewResult;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ReviewEvidence {
    pub review_attempt_id: String,
    pub review_operation_id: String,
    pub report_event_id: i64,
    pub report: CandidateReviewResult,
}

/// Initial selection fixes a verification Operation; candidate comes from its frozen input.
pub(crate) async fn select_input_tx(
    tx: &mut Tx<'_>,
    track: &str,
    producer: &str,
    slot: &str,
) -> Result<candidate_input::Binding> {
    let row: Option<(String, String)> = sqlx::query_as("SELECT o.id,o.tx_output_json FROM task_candidate_verification_allocations a JOIN operations o ON o.operation_key=a.operation_key AND o.kind='candidate-verify' AND o.phase='succeeded' JOIN task_file_candidates c ON c.operation_id=a.publication_operation_id JOIN current_tasks t ON t.id=c.producer_attempt_id WHERE t.track_id=?1 AND t.key=?2 AND c.slot=?3")
        .bind(track).bind(producer).bind(slot).fetch_optional(&mut **tx).await?;
    let (id, output) =
        row.ok_or_else(|| conflict("waiting for successful candidate verification"))?;
    let output: crate::operation::TxOutput = serde_json::from_str(&output)?;
    let frozen: candidate_verify::Frozen = serde_json::from_value(output.data)?;
    candidate_verify::qualified_tx(tx, &id, &frozen.candidate).await?;
    Ok(candidate_input::Binding {
        candidate: frozen.candidate,
        verification_operation_id: id,
        purpose: BindingPurpose::CandidateReviewInput,
    })
}

pub(crate) async fn evidence_tx(
    tx: &mut Tx<'_>,
    candidate: &Candidate,
    verification: &str,
) -> Result<ReviewEvidence> {
    let key = candidate
        .policy()?
        .reviewer()
        .ok_or_else(|| conflict("candidate does not require review"))?;
    let allocation = task_attempt_current_tx(tx, &candidate.source.track_id, key)
        .await?
        .ok_or_else(|| conflict("waiting for candidate reviewer"))?;
    let task = task_get_tx(tx, &allocation.attempt_id)
        .await?
        .ok_or_else(|| conflict("reviewer missing"))?;
    super::repair::validate_task_tx(tx, &task).await?;
    let (binding, state, prepared) = candidate_input::load_tx(tx, &task.id)
        .await?
        .ok_or_else(|| conflict("reviewer input missing"))?;
    if binding.candidate != *candidate
        || binding.verification_operation_id != verification
        || binding.purpose != BindingPurpose::CandidateReviewInput
        || state != "prepared"
    {
        return Err(conflict(
            "reviewer subject does not match candidate verification",
        ));
    }
    candidate_input::validate_source_tx(tx, &task, &binding).await?;
    crate::task_recovery::validate_frozen_contract_tx(tx, &task).await?;
    if task.status != crate::model::TaskStatus::Done {
        return Err(conflict("waiting for completed candidate review"));
    }
    let evidence = crate::routes::isolated_tasks::accepted_report_evidence_tx(
        tx,
        &task.track_id,
        &task.key,
        &task.id,
    )
    .await?
    .ok_or_else(|| conflict("reviewer has no authenticated report"))?;
    if prepared.as_deref() != Some(&evidence.operation_id) {
        return Err(conflict(
            "review report operation does not own prepared input",
        ));
    }
    if crate::isolated_codex::review_settled::outcome_tx(tx, &task, &evidence.operation_id).await?
        != "succeeded"
    {
        return Err(conflict(
            "review Operation failed; candidate is not qualified",
        ));
    }
    let crate::routes::isolated_tasks::AcceptedTaskReport::Completed { result, .. } =
        evidence.report
    else {
        return Err(conflict("candidate review failed"));
    };
    // Re-read the authenticated full result for this exact event, including repair responses.
    let report = super::repair_report::parse_tx(tx, &task, result).await?;
    Ok(ReviewEvidence {
        review_attempt_id: task.id,
        review_operation_id: evidence.operation_id,
        report_event_id: evidence.event_id,
        report,
    })
}

/// Same-outcome report retries must not quietly accept different review content.
/// Existing report admission and Event/Operation provenance still own authentication.
pub(crate) async fn validate_report_tx(tx: &mut Tx<'_>, track: &str, event: &Event) -> Result<()> {
    let Event::TaskCompleted {
        idempotency_key,
        result,
        artifacts,
        ..
    } = event
    else {
        return Ok(());
    };
    let Some(task) = task_get_tx(tx, idempotency_key).await? else {
        return Ok(());
    };
    super::repair::validate_task_tx(tx, &task).await?;
    if !matches!(
        selection(&task)?,
        Some(FileDelivery::CandidateReviewer { .. })
    ) {
        return Ok(());
    }
    if task.track_id != track {
        return Err(conflict("review report Track mismatch"));
    }
    super::repair_report::parse_tx(tx, &task, result.clone()).await?;
    let current = task_attempt_current_tx(tx, track, &task.key)
        .await?
        .ok_or_else(|| conflict("review attempt missing"))?;
    if current.attempt_id != task.id {
        return Err(conflict("obsolete review attempt"));
    }
    crate::task_recovery::validate_frozen_contract_tx(tx, &task).await?;
    let (binding, state, _) = candidate_input::load_tx(tx, &task.id)
        .await?
        .ok_or_else(|| conflict("review input missing"))?;
    if state != "prepared" {
        return Err(conflict("review input not prepared"));
    }
    candidate_input::validate_source_tx(tx, &task, &binding).await?;
    if let Some(previous) =
        crate::routes::isolated_tasks::accepted_report_tx(tx, track, &task.key, &task.id).await?
    {
        match previous {
            crate::routes::isolated_tasks::AcceptedTaskReport::Completed {
                result: old,
                artifacts: old_artifacts,
            } if old == *result
                && old_artifacts == artifacts.iter().map(|a| a.0.clone()).collect::<Vec<_>>() => {}
            _ => return Err(conflict("review report replay changed its content")),
        }
    }
    Ok(())
}

/// Historical report facts are independent of current qualification/withdrawal.
pub(crate) async fn view_tx(
    tx: &mut Tx<'_>,
    candidate: &Candidate,
    verification: &str,
) -> Result<serde_json::Value> {
    use serde_json::json;
    let Some(key) = candidate.policy()?.reviewer() else {
        return Ok(json!({"state":"not-required"}));
    };
    let row: Option<(String, Option<String>)> = sqlx::query_as("SELECT b.attempt_id,b.prepared_operation_id FROM task_candidate_input_bindings b JOIN task_attempt_allocations a ON a.attempt_id=b.attempt_id WHERE b.publication_operation_id=?1 AND b.verification_operation_id=?2 AND a.track_id=?3 AND a.key=?4 AND json_extract(b.binding_json,'$.purpose')='candidate-review-input' ORDER BY a.generation DESC LIMIT 1")
        .bind(&candidate.publication_operation_id).bind(verification).bind(&candidate.source.track_id).bind(key).fetch_optional(&mut **tx).await?;
    let Some((attempt, prepared)) = row else {
        return Ok(json!({"state":"waiting","reviewer":key}));
    };
    let Some(evidence) = crate::routes::isolated_tasks::accepted_report_evidence_tx(
        tx,
        &candidate.source.track_id,
        key,
        &attempt,
    )
    .await?
    else {
        return Ok(
            json!({"state":"waiting","reviewer":key,"review_attempt_id":attempt,"review_operation_id":prepared}),
        );
    };
    let mut view = json!({"reviewer":key,"review_attempt_id":attempt,"review_operation_id":evidence.operation_id,"report_event_id":evidence.event_id});
    let operation: (String, Option<String>) =
        sqlx::query_as("SELECT phase,last_error FROM operations WHERE id=?1")
            .bind(&evidence.operation_id)
            .fetch_one(&mut **tx)
            .await?;
    view["operation"] = json!({"state":operation.0,"failure":operation.1});
    match evidence.report {
        crate::routes::isolated_tasks::AcceptedTaskReport::Completed { result, .. } => {
            let task = task_get_tx(tx, &attempt)
                .await?
                .ok_or_else(|| conflict("reviewer missing"))?;
            let responses = result.get("finding_responses").cloned();
            match super::repair_report::parse_history_tx(tx, &task, result).await {
                Ok(report) => {
                    if let Some(responses) = responses {
                        view["finding_responses"] = responses;
                    }
                    view["state"] = json!(if report.passed { "passed" } else { "blocking" });
                    view["passed"] = json!(report.passed);
                    view["blocking_findings"] = json!(report.blocking_findings);
                }
                _ => {
                    view["state"] = json!("malformed");
                }
            }
        }
        crate::routes::isolated_tasks::AcceptedTaskReport::Failed { reason } => {
            view["state"] = json!("failed");
            view["reason"] = json!(reason);
        }
    }
    Ok(view)
}
