use super::*;
use crate::operation::Tx;
use serde_json::{Value, json};
pub(super) async fn view_tx(tx: &mut Tx<'_>, task: &Task, role: &FileDelivery) -> Result<Value> {
    let binding = super::candidate_input::load_tx(tx, &task.id).await?;
    let publication = if let Some((binding, _, _)) = &binding
        && !matches!(role, FileDelivery::CandidateProducer { .. })
    {
        Some(binding.candidate.publication_operation_id.clone())
    } else {
        let key = match role {
            FileDelivery::CandidateProducer { .. } => &task.key,
            FileDelivery::CandidateConsumer { producer, .. }
            | FileDelivery::CandidateReviewer { producer, .. } => producer,
            _ => return Err(conflict("not candidate role")),
        };
        sqlx::query_scalar("SELECT c.operation_id FROM task_file_candidates c JOIN current_tasks t ON t.id=c.producer_attempt_id WHERE t.track_id=?1 AND t.key=?2")
            .bind(&task.track_id).bind(key).fetch_optional(&mut **tx).await?
    };
    let mut view = json!({"contract":role,"candidate":{"state":"waiting"},"verification":{"state":"waiting"},"qualified":false,"scope":"declared-checks-only"});
    if let Some(receipt) = super::repair::for_task_tx(tx, task).await? {
        view["repair"] = super::repair_view::view_tx(tx, &receipt).await?;
    } else if let Some(receipt) = super::repair::lookup_tx(
        tx,
        &task.track_id,
        match role {
            FileDelivery::CandidateReviewer { producer, .. } => producer,
            _ => &task.key,
        },
    )
    .await?
    {
        view["repair"] = super::repair_view::view_tx(tx, &receipt).await?;
    }
    let producer_key = match role {
        FileDelivery::CandidateProducer { .. } => &task.key,
        FileDelivery::CandidateConsumer { producer, .. }
        | FileDelivery::CandidateReviewer { producer, .. } => producer,
        _ => return Err(conflict("not candidate role")),
    };
    let publication_state: Option<(String, String, Option<String>)> = if let Some(id) = &publication
    {
        sqlx::query_as("SELECT id,phase,last_error FROM operations WHERE id=?1 AND kind='task-file-publication'")
            .bind(id).fetch_optional(&mut **tx).await?
    } else {
        sqlx::query_as("SELECT o.id,o.phase,o.last_error FROM operations o JOIN current_tasks t ON o.idempotency_key='file:' || t.id WHERE o.kind='task-file-publication' AND t.track_id=?1 AND t.key=?2")
            .bind(&task.track_id).bind(producer_key).fetch_optional(&mut **tx).await?
    };
    view["publication"] = match publication_state {
        Some((id, state, error)) => json!({"operation_id":id,"state":state,"failure":error}),
        None => json!({"state":"waiting"}),
    };
    if let Some(publication) = publication {
        let candidate = super::candidate::load_tx(tx, &publication).await?;
        view["scope"] = json!(candidate.policy()?.scope());
        view["decision"] = super::candidate_qualification::decision_view_tx(tx, &candidate).await?;
        view["candidate"] = json!({"state":"sealed","publication_operation_id":publication,"snapshot":candidate.snapshot});
        let verification: Option<(String,String,Option<String>,Option<String>)> = sqlx::query_as("SELECT id,phase,last_error,tx_output_json FROM operations WHERE kind='candidate-verify' AND idempotency_key=?1")
            .bind(format!("candidate:{publication}")).fetch_optional(&mut **tx).await?;
        if let Some((id, phase, error, output)) = verification {
            view["review"] = super::candidate_review::view_tx(tx, &candidate, &id).await?;
            let qualification = if let Some((binding, _, _)) = &binding
                && !matches!(role, FileDelivery::CandidateProducer { .. })
            {
                if matches!(role, FileDelivery::CandidateConsumer { .. }) {
                    super::candidate_input::validate_tx(tx, task, binding).await
                } else {
                    super::candidate_qualification::qualified_tx(tx, &candidate, &id)
                        .await
                        .map(|_| ())
                }
            } else {
                super::candidate_qualification::qualified_tx(tx, &candidate, &id)
                    .await
                    .map(|_| ())
            };
            let reason = match qualification {
                Ok(_) => None,
                Err(CalmError::Conflict(reason) | CalmError::Forbidden(reason)) => Some(reason),
                Err(error) => return Err(error),
            };
            view["qualified"] = json!(reason.is_none());
            view["qualification"] = json!({"qualified":reason.is_none(),"reason":reason});
            let evidence = output
                .map(|raw| serde_json::from_str::<crate::operation::TxOutput>(&raw))
                .transpose()?
                .map(|o| o.result);
            view["verification"] = json!({"operation_id":id,"state":phase,"failure":error,"passed":evidence.as_ref().and_then(|e|e["verdict"]["passed"].as_bool()),"policy":candidate.policy()?,
                "failing_step":evidence.as_ref().and_then(|e|e["verdict"]["failing_step"].as_str()),
                "exit_code":evidence.as_ref().and_then(|e|e["verdict"]["exit_code"].as_i64()),
                "status_detail":evidence.as_ref().and_then(|e|e["verdict"]["status_detail"].as_str())});
        }
    }
    if let Some((binding, state, _)) = binding {
        let decision: Option<i64> = sqlx::query_scalar(
            "SELECT decision_event_id FROM task_candidate_decision_bindings WHERE attempt_id=?1",
        )
        .bind(&task.id)
        .fetch_optional(&mut **tx)
        .await?;
        view["input"] = json!({"state":state,"publication_operation_id":binding.candidate.publication_operation_id,"verification_operation_id":binding.verification_operation_id,"decision_event_id":decision,"purpose":binding.purpose,"path":"/workspace/inputs/source"});
    }
    Ok(view)
}
