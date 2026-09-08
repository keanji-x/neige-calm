use super::*;
use crate::operation::Tx;
use serde_json::{Value, json};
/// Public projections contain no storage paths, provider credentials or manual hashes.
pub(crate) async fn view_tx(tx: &mut Tx<'_>, task: &Task) -> Result<Value> {
    match crate::isolated_codex::lookup::recorded_worker_kind_tx(tx, &task.id).await {
        Ok(Some(kind)) if kind != crate::isolated_codex::OPERATION_KIND => return Ok(Value::Null),
        Err(CalmError::Conflict(_)) => {
            return Ok(json!({"state":"invalid","failure":"Ambiguous recorded execution backend"}));
        }
        Err(error) => return Err(error),
        _ => {}
    }
    let role = match selection(task) {
        Ok(Some(role)) => role,
        Ok(None) => return Ok(Value::Null),
        Err(_) => {
            return Ok(
                json!({"state":"invalid","failure":"Invalid explicit file delivery contract"}),
            );
        }
    };
    if matches!(
        role,
        FileDelivery::CandidateProducer { .. }
            | FileDelivery::CandidateConsumer { .. }
            | FileDelivery::CandidateReviewer { .. }
    ) {
        return super::candidate_view::view_tx(tx, task, &role).await;
    }
    let binding: Option<(String, String)> = sqlx::query_as(
        "SELECT binding_json,state FROM task_file_input_bindings WHERE attempt_id=?1",
    )
    .bind(&task.id)
    .fetch_optional(&mut **tx)
    .await?;
    let bound_publication = binding
        .as_ref()
        .map(|(raw, _)| {
            let value: Value = serde_json::from_str(raw)?;
            value["receipt"]["publication_operation_id"]
                .as_str()
                .map(str::to_owned)
                .ok_or_else(|| conflict("input publication identity missing"))
        })
        .transpose()?;
    let producer_id = match &role {
        FileDelivery::CandidateProducer { .. }
        | FileDelivery::CandidateConsumer { .. }
        | FileDelivery::CandidateReviewer { .. } => {
            unreachable!("routed above")
        }
        FileDelivery::Producer { .. } => Some(task.id.clone()),
        FileDelivery::Consumer { producer, .. } => {
            sqlx::query_scalar("SELECT id FROM current_tasks WHERE track_id=?1 AND key=?2")
                .bind(&task.track_id)
                .bind(producer)
                .fetch_optional(&mut **tx)
                .await?
        }
    };
    let operation: Option<(String, String, Option<String>)> = if let Some(id) = bound_publication {
        sqlx::query_as("SELECT id,phase,last_error FROM operations WHERE id=?1 AND kind='task-file-publication'")
            .bind(id).fetch_optional(&mut **tx).await?
    } else if let Some(id) = producer_id {
        sqlx::query_as("SELECT id,phase,last_error FROM operations WHERE kind='task-file-publication' AND idempotency_key=?1")
            .bind(format!("file:{id}")).fetch_optional(&mut **tx).await?
    } else {
        None
    };
    let authority_error = if let Some((id, phase, _)) = &operation {
        if phase == "succeeded" {
            let raw: String = sqlx::query_scalar("SELECT payload_json FROM operations WHERE id=?1")
                .bind(id)
                .fetch_one(&mut **tx)
                .await?;
            let payload: PublicationPayload = serde_json::from_str(&raw)?;
            source_tx(tx, &payload)
                .await
                .err()
                .map(|error| error.to_string())
        } else {
            None
        }
    } else {
        None
    };
    let mut view = json!({"contract":role,"authority_failure":authority_error});
    view["publication"] = match operation {
        Some((id, phase, error)) => json!({"operation_id":id,"state":phase,"failure":error}),
        None => {
            json!({"state":"waiting","reason":"Waiting for the declared producer to complete and confirm stop"})
        }
    };
    if let Some((binding, state)) = binding {
        let binding: Value = serde_json::from_str(&binding)?;
        view["input"] = json!({"state":state,"producer_attempt_id":binding["receipt"]["source"]["task_id"],
            "publication_operation_id":binding["receipt"]["publication_operation_id"],
            "path":format!("/workspace/inputs/source/{}", binding["receipt"]["contract"]["path"].as_str().ok_or_else(|| conflict("input path missing"))?)});
    }
    if matches!(role, FileDelivery::Consumer { .. }) {
        let failure: Option<Option<String>> = sqlx::query_scalar("SELECT last_error FROM operations WHERE kind='codex-isolated-worker' AND idempotency_key=?1")
            .bind(&task.id).fetch_optional(&mut **tx).await?;
        view["preparation_failure"] =
            json!(failure.flatten().map(|reason| public_failure(&reason)));
    }
    Ok(view)
}

fn public_failure(reason: &str) -> String {
    // Provider errors can contain host paths; only expose recognized delivery categories.
    for detail in [
        "integrity mismatch",
        "input destination conflicts",
        "input binding missing",
        "input has not been prepared",
        "authority changed",
        "source or sealed storage is unavailable",
        "invalid file source or request",
        "file exceeds delivery limits",
    ] {
        if reason.contains(detail) {
            return format!("File input preparation failed: {detail}");
        }
    }
    "File input preparation failed; inspect the current task execution status.".into()
}
