use super::*;
use crate::operation::Tx;
use serde_json::{Value, json};
pub(super) async fn view_tx(tx: &mut Tx<'_>, task: &Task, role: &FileDelivery) -> Result<Value> {
    let binding = super::candidate_input::load_tx(tx, &task.id).await?;
    let publication = if let Some((binding, _, _)) = &binding {
        Some(binding.candidate.publication_operation_id.clone())
    } else {
        let key = match role {
            FileDelivery::CandidateProducer { .. } => &task.key,
            FileDelivery::CandidateConsumer { producer, .. } => producer,
            _ => return Err(conflict("not candidate role")),
        };
        sqlx::query_scalar("SELECT c.operation_id FROM task_file_candidates c JOIN current_tasks t ON t.id=c.producer_attempt_id WHERE t.track_id=?1 AND t.key=?2")
            .bind(&task.track_id).bind(key).fetch_optional(&mut **tx).await?
    };
    let mut view = json!({"contract":role,"candidate":{"state":"waiting"},"verification":{"state":"waiting"},"qualified":false,"scope":"declared-checks-only"});
    let producer_key = match role {
        FileDelivery::CandidateProducer { .. } => &task.key,
        FileDelivery::CandidateConsumer { producer, .. } => producer,
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
        view["candidate"] = json!({"state":"sealed","publication_operation_id":publication,"snapshot":candidate.snapshot});
        let verification: Option<(String,String,Option<String>,Option<String>)> = sqlx::query_as("SELECT id,phase,last_error,tx_output_json FROM operations WHERE kind='candidate-verify' AND idempotency_key=?1")
            .bind(format!("candidate:{publication}")).fetch_optional(&mut **tx).await?;
        if let Some((id, phase, error, output)) = verification {
            let qualification = super::candidate_verify::qualified_tx(tx, &id, &candidate).await;
            view["qualified"] = json!(qualification.is_ok());
            let evidence = output
                .and_then(|raw| serde_json::from_str::<crate::operation::TxOutput>(&raw).ok())
                .map(|o| o.result);
            view["verification"] = json!({"operation_id":id,"state":phase,"failure":error,"passed":evidence.as_ref().and_then(|e|e["verdict"]["passed"].as_bool()),"policy":candidate.policy()?,
                "failing_step":evidence.as_ref().and_then(|e|e["verdict"]["failing_step"].as_str()),
                "exit_code":evidence.as_ref().and_then(|e|e["verdict"]["exit_code"].as_i64()),
                "status_detail":evidence.as_ref().and_then(|e|e["verdict"]["status_detail"].as_str())});
        }
    }
    if let Some((binding, state, _)) = binding {
        view["input"] = json!({"state":state,"publication_operation_id":binding.candidate.publication_operation_id,"verification_operation_id":binding.verification_operation_id,"path":"/workspace/inputs/source"});
    }
    Ok(view)
}
