//! Exact qualified candidate input through claim, preparation and preturn.
use super::{candidate::Candidate, candidate_verify, *};
use crate::{
    db::sqlite::{task_attempt_get_tx, task_get_tx},
    db::{RouteRepo, write_in_tx_typed},
    operation::{Operation, Tx},
};
use calm_types::{task_execution::CandidateInputPurpose, task_recovery::TaskAttemptOrigin};
use serde::{Deserialize, Serialize};
use std::path::Path;
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Binding {
    pub candidate: Candidate,
    pub verification_operation_id: String,
    pub purpose: BindingPurpose,
}
/// Persisted purpose is separate from the authorable ordinary-consumer purpose.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) enum BindingPurpose {
    #[serde(rename = "verified-candidate-input")]
    VerifiedCandidateInput,
    #[serde(rename = "candidate-review-input")]
    CandidateReviewInput,
    #[serde(rename = "candidate-repair-input")]
    CandidateRepairInput,
}
impl From<CandidateInputPurpose> for BindingPurpose {
    fn from(value: CandidateInputPurpose) -> Self {
        match value {
            CandidateInputPurpose::VerifiedCandidateInput => Self::VerifiedCandidateInput,
        }
    }
}
pub(crate) async fn load_tx(
    tx: &mut Tx<'_>,
    id: &str,
) -> Result<Option<(Binding, String, Option<String>)>> {
    let row: Option<(String, String, Option<String>)> = sqlx::query_as("SELECT binding_json,state,prepared_operation_id FROM task_candidate_input_bindings WHERE attempt_id=?1")
        .bind(id).fetch_optional(&mut **tx).await?;
    row.map(|(raw, state, op)| Ok((serde_json::from_str(&raw)?, state, op)))
        .transpose()
}
async fn input_contract_tx(
    tx: &mut Tx<'_>,
    task: &Task,
) -> Result<Option<(String, String, BindingPurpose, bool)>> {
    if let Some(receipt) = super::repair::validate_contract_tx(tx, task).await?
        && task.key == receipt.repair.key
    {
        return Ok(Some((
            receipt.args.producer,
            receipt.input.candidate.slot()?.into(),
            BindingPurpose::CandidateRepairInput,
            true,
        )));
    }
    Ok(match selection(task)? {
        Some(FileDelivery::CandidateConsumer {
            producer,
            slot,
            purpose,
        }) => Some((producer, slot, purpose.into(), false)),
        Some(FileDelivery::CandidateReviewer { producer, slot, .. }) => {
            Some((producer, slot, BindingPurpose::CandidateReviewInput, true))
        }
        _ => None,
    })
}
pub(crate) async fn validate_source_tx(
    tx: &mut Tx<'_>,
    task: &Task,
    binding: &Binding,
) -> Result<()> {
    let (producer, slot, purpose, _) = input_contract_tx(tx, task)
        .await?
        .ok_or_else(|| conflict("candidate input contract missing"))?;
    let reviewer = purpose == BindingPurpose::CandidateReviewInput;
    if purpose == BindingPurpose::CandidateRepairInput {
        let receipt = super::repair::for_task_tx(tx, task)
            .await?
            .ok_or_else(|| conflict("repair receipt missing"))?;
        if receipt.input != *binding {
            return Err(conflict("repair input differs from exact C1 receipt"));
        }
    }
    let (source, _) = source_tx(tx, &binding.candidate.source).await?;
    if source.track_id != task.track_id
        || source.key != producer
        || binding.candidate.slot()? != slot
        || binding.purpose != purpose
        || (reviewer && binding.candidate.policy()?.reviewer() != Some(task.key.as_str()))
    {
        return Err(conflict("candidate input binding authority changed"));
    }
    candidate_verify::qualified_tx(tx, &binding.verification_operation_id, &binding.candidate)
        .await?;
    Ok(())
}
pub(crate) async fn validate_tx(tx: &mut Tx<'_>, task: &Task, binding: &Binding) -> Result<()> {
    crate::task_recovery::validate_frozen_contract_tx(tx, task).await?;
    validate_source_tx(tx, task, binding).await?;
    if matches!(
        selection(task)?,
        Some(FileDelivery::CandidateConsumer { .. })
    ) {
        super::candidate_qualification::validate_binding_tx(tx, task, binding).await?;
    } else {
        super::repair::validate_task_tx(tx, task).await?;
    }
    Ok(())
}
pub(crate) async fn bind_claim_tx(tx: &mut Tx<'_>, task: &Task) -> Result<()> {
    let Some((producer, slot, purpose, reviewer)) = input_contract_tx(tx, task).await? else {
        return Ok(());
    };
    if let Some((binding, _, _)) = load_tx(tx, &task.id).await? {
        return validate_tx(tx, task, &binding).await;
    }
    let allocation = task_attempt_get_tx(tx, &task.id)
        .await?
        .ok_or_else(|| conflict("candidate input allocation missing"))?;
    let predecessor = match allocation.origin {
        TaskAttemptOrigin::Recovery {
            previous_attempt_id,
            ..
        } => Some(previous_attempt_id),
        _ => None,
    };
    let binding = if let Some(previous) = &predecessor {
        load_tx(tx, previous)
            .await?
            .ok_or_else(|| conflict("candidate recovery input missing"))?
            .0
    } else if purpose == BindingPurpose::CandidateRepairInput {
        super::repair::for_task_tx(tx, task)
            .await?
            .ok_or_else(|| conflict("repair receipt missing"))?
            .input
    } else {
        let mut binding =
            super::candidate_review::select_input_tx(tx, &task.track_id, &producer, &slot).await?;
        binding.purpose = purpose;
        binding
    };
    validate_source_tx(tx, task, &binding).await?;
    sqlx::query("INSERT INTO task_candidate_input_bindings(attempt_id,track_id,publication_operation_id,verification_operation_id,binding_json,state) VALUES(?1,?2,?3,?4,?5,'bound')")
        .bind(&task.id).bind(&task.track_id).bind(&binding.candidate.publication_operation_id).bind(&binding.verification_operation_id).bind(serde_json::to_string(&binding)?).execute(&mut **tx).await?;
    if !reviewer {
        super::candidate_qualification::bind_tx(tx, task, &binding, predecessor.as_deref()).await?;
    }
    validate_tx(tx, task, &binding).await
}
async fn authorized_tx(
    tx: &mut Tx<'_>,
    op: &Operation,
) -> Result<(Binding, String, Option<String>)> {
    crate::isolated_codex::admission::validate_start_tx(tx, op).await?;
    let task = task_get_tx(
        tx,
        op.idempotency_key
            .as_deref()
            .ok_or_else(|| conflict("candidate consumer task missing"))?,
    )
    .await?
    .ok_or_else(|| conflict("candidate consumer missing"))?;
    let binding = load_tx(tx, &task.id)
        .await?
        .ok_or_else(|| conflict("candidate consumer binding missing"))?;
    validate_tx(tx, &task, &binding.0).await?;
    Ok(binding)
}
pub(crate) async fn prepare(repo: &dyn RouteRepo, op: &Operation, workspace: &Path) -> Result<()> {
    let owned = op.clone();
    let (binding, state, prepared) = write_in_tx_typed(repo, move |tx| {
        Box::pin(async move { authorized_tx(tx, &owned).await })
    })
    .await?;
    if state == "prepared" && prepared.as_deref() != Some(&op.id) {
        return Err(conflict("candidate preparation operation changed"));
    }
    let path = workspace.join("inputs");
    tokio::task::spawn_blocking(move || binding.candidate.prepare(&path))
        .await
        .map_err(|_| conflict("candidate input preparation interrupted"))??;
    let op = op.clone();
    write_in_tx_typed(repo,move |tx| Box::pin(async move {
        crate::isolated_codex::journal::require_owner_tx(tx,&op).await?;
        let (_,state,_) = authorized_tx(tx,&op).await?;
        if state == "bound" {
            sqlx::query("UPDATE task_candidate_input_bindings SET state='prepared',prepared_operation_id=?1 WHERE attempt_id=?2 AND state='bound'")
                .bind(&op.id).bind(op.idempotency_key.as_deref()).execute(&mut **tx).await?;
        }
        Ok(())
    })).await
}
pub(crate) async fn verify(tx: &mut Tx<'_>, op: &Operation, workspace: &Path) -> Result<()> {
    let (binding, state, prepared) = authorized_tx(tx, op).await?;
    if state != "prepared" || prepared.as_deref() != Some(&op.id) {
        return Err(conflict("candidate input is not prepared"));
    }
    let path = workspace.join("inputs");
    tokio::task::spawn_blocking(move || binding.candidate.verify(&path))
        .await
        .map_err(|_| conflict("candidate preturn check interrupted"))?
}
pub(crate) async fn prompt(tx: &mut Tx<'_>, task: &Task) -> Result<String> {
    if let Some(receipt) = super::repair::validate_contract_tx(tx, task).await? {
        let (binding, _, _) = load_tx(tx, &task.id)
            .await?
            .ok_or_else(|| conflict("repair input missing"))?;
        validate_tx(tx, task, &binding).await?;
        let lineage = serde_json::to_string(&receipt.public())?;
        if task.key == receipt.repair.key {
            return Ok(format!(
                "Repair the exact C1 files at /workspace/inputs/source. Keep these inputs unchanged. Write every complete declared C2 output under /workspace, including unchanged files. Original goal: {}. Original acceptance: {}. Exact original review findings and lineage: {}. Inherited output contract and machine checks: {}. C2 needs fresh checks, its new Reviewer and explicit Planner acceptance.",
                receipt.source_payload["goal"],
                receipt.source_payload["acceptance"],
                lineage,
                serde_json::to_string(&selection(task)?)?
            ));
        }
        return Ok(format!(
            "Review exact C2 at /workspace/inputs/source against this task's acceptance and every original R1 finding. Original findings and kernel lineage: {lineage}. Complete with exactly passed (boolean), blocking_findings (array), finding_responses (array). Each finding_responses entry requires finding_index (original zero-based array index), status (resolved or unresolved), evidence (nonempty specific explanation). Answer every original finding exactly once, no duplicates or extra indices. passed=true requires all resolved and no blockers; passed=false requires blockers. The kernel binds original report_event_id and exact C2; do not supply identities. Report acceptance does not accept the implementation."
        ));
    }
    match selection(task)? {
        Some(FileDelivery::CandidateProducer {
            slot,
            paths,
            policy,
        }) => Ok(format!(
            "Write every declared ordinary file for output `{slot}` under /workspace: {}. After confirmed stop the kernel seals these files and runs these exact machine checks against a working copy: {}. Machine scope is declared check exit status only. A review-required policy additionally waits for its named Reviewer and Planner acceptance; a settled blocking review may be eligible for one Planner-requested linked repair.",
            serde_json::to_string(&paths)?,
            serde_json::to_string(&policy)?
        )),
        Some(FileDelivery::CandidateReviewer { producer, .. }) => {
            let (binding, _, _) = load_tx(tx, &task.id)
                .await?
                .ok_or_else(|| conflict("reviewer input missing"))?;
            validate_tx(tx, task, &binding).await?;
            let evidence = candidate_verify::qualified_tx(
                tx,
                &binding.verification_operation_id,
                &binding.candidate,
            )
            .await?;
            Ok(format!(
                "Review the exact sealed files from `{producer}` at /workspace/inputs/source. Machine verification Operation {} checked this same candidate under policy {}. Machine verdict: {}. Review against the task acceptance requirements. Complete with result containing exactly passed (boolean) and blocking_findings (array of specific reasons). passed=true requires no blockers; passed=false requires at least one blocker. Do not supply a subject, hash or operation identity; the kernel binds your report to this input. A report completion does not accept the implementation.",
                binding.verification_operation_id,
                serde_json::to_string(&evidence.policy)?,
                serde_json::to_string(
                    &serde_json::json!({"passed":evidence.verdict.passed,"exit_code":evidence.verdict.exit_code,"failing_step":evidence.verdict.failing_step,"status_detail":evidence.verdict.status_detail,"log_tail":evidence.verdict.log_tail})
                )?
            ))
        }
        Some(FileDelivery::CandidateConsumer { producer, .. }) => {
            let (binding, _, _) = load_tx(tx, &task.id)
                .await?
                .ok_or_else(|| conflict("candidate consumer binding missing"))?;
            validate_tx(tx, task, &binding).await?;
            Ok(format!(
                "Read the exact sealed files from `{producer}` at /workspace/inputs/source. The declared machine checks passed for this candidate. The kernel checked the frozen qualification policy, including exact Reviewer/Planner evidence when required. This does not assert full test coverage."
            ))
        }
        _ => Err(conflict("not a candidate delivery")),
    }
}
pub(crate) async fn require_recovery(tx: &mut Tx<'_>, task: &Task) -> Result<()> {
    let (binding, _, _) = load_tx(tx, &task.id)
        .await?
        .ok_or_else(|| conflict("candidate recovery input missing"))?;
    validate_tx(tx, task, &binding).await
}
