//! Exact qualified candidate input through claim, preparation and preturn.
use super::{
    candidate::{self, Candidate},
    candidate_verify, *,
};
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
    pub purpose: CandidateInputPurpose,
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
pub(crate) async fn validate_tx(tx: &mut Tx<'_>, task: &Task, binding: &Binding) -> Result<()> {
    let Some(FileDelivery::CandidateConsumer {
        producer,
        slot,
        purpose,
    }) = selection(task)?
    else {
        return Err(conflict("candidate consumer contract missing"));
    };
    let (source, _) = source_tx(tx, &binding.candidate.source).await?;
    if source.track_id != task.track_id
        || source.key != producer
        || binding.candidate.slot()? != slot
        || binding.purpose != purpose
    {
        return Err(conflict("candidate consumer binding authority changed"));
    }
    candidate_verify::qualified_tx(tx, &binding.verification_operation_id, &binding.candidate)
        .await?;
    Ok(())
}
pub(crate) async fn bind_claim_tx(tx: &mut Tx<'_>, task: &Task) -> Result<()> {
    let Some(FileDelivery::CandidateConsumer {
        producer,
        slot,
        purpose,
    }) = selection(task)?
    else {
        return Ok(());
    };
    if let Some((binding, _, _)) = load_tx(tx, &task.id).await? {
        return validate_tx(tx, task, &binding).await;
    }
    let allocation = task_attempt_get_tx(tx, &task.id)
        .await?
        .ok_or_else(|| conflict("candidate consumer allocation missing"))?;
    let binding = match allocation.origin {
        TaskAttemptOrigin::Recovery {
            previous_attempt_id,
            ..
        } => {
            load_tx(tx, &previous_attempt_id)
                .await?
                .ok_or_else(|| conflict("candidate recovery input missing"))?
                .0
        }
        _ => {
            let row: Option<(String,String)> = sqlx::query_as("SELECT c.operation_id,o.id FROM task_file_candidates c JOIN current_tasks t ON t.id=c.producer_attempt_id JOIN operations o ON o.kind='candidate-verify' AND o.idempotency_key='candidate:' || c.operation_id AND o.phase='succeeded' WHERE t.track_id=?1 AND t.key=?2 AND c.slot=?3")
                .bind(&task.track_id).bind(producer).bind(slot).fetch_optional(&mut **tx).await?;
            let (publication, verification_operation_id) =
                row.ok_or_else(|| conflict("waiting for successful candidate verification"))?;
            Binding {
                candidate: candidate::load_tx(tx, &publication).await?,
                verification_operation_id,
                purpose,
            }
        }
    };
    validate_tx(tx, task, &binding).await?;
    sqlx::query("INSERT INTO task_candidate_input_bindings(attempt_id,track_id,publication_operation_id,verification_operation_id,binding_json,state) VALUES(?1,?2,?3,?4,?5,'bound')")
        .bind(&task.id).bind(&task.track_id).bind(&binding.candidate.publication_operation_id).bind(&binding.verification_operation_id).bind(serde_json::to_string(&binding)?).execute(&mut **tx).await?;
    Ok(())
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
    match selection(task)? {
        Some(FileDelivery::CandidateProducer {
            slot,
            paths,
            policy,
        }) => Ok(format!(
            "Write every declared ordinary file for output `{slot}` under /workspace: {}. After confirmed stop the kernel seals these files and runs these exact machine checks against a working copy: {}. Scope is declared check exit status only; semantic review and repair are unsupported.",
            serde_json::to_string(&paths)?,
            serde_json::to_string(&policy)?
        )),
        Some(FileDelivery::CandidateConsumer { producer, .. }) => {
            let (binding, _, _) = load_tx(tx, &task.id)
                .await?
                .ok_or_else(|| conflict("candidate consumer binding missing"))?;
            validate_tx(tx, task, &binding).await?;
            Ok(format!(
                "Read the exact sealed files from `{producer}` at /workspace/inputs/source. The declared machine checks passed for this candidate. This does not assert full test coverage or semantic review."
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
