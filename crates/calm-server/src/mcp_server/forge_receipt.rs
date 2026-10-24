//! The one renderer of a plugin-backed forge action's MCP result, regardless of tool name: a
//! recorded outcome answers through [`outcome`] for the call itself, its replay and
//! `neige_op_show` alike; a parked call and `neige_op_show` both read the stored operation through
//! [`show`].
use serde::Serialize;
use serde_json::{Value, json};

use crate::mcp_server::result::ToolResult;
use crate::operation::forge_action_adapter::completion_event;
use crate::operation::{Operation, OperationOutcome, OperationResult, operation_result_from};

pub(crate) mod output;

pub(super) const PENDING_GUIDANCE: &str = include_str!("../../prompts/forge-action/pending.md");

#[derive(Serialize)]
#[serde(rename_all = "snake_case")]
enum PendingStatus {
    Pending,
}

#[derive(Serialize)]
struct PendingReceipt<'a> {
    op_id: &'a str,
    parked: bool,
    status: PendingStatus,
    message: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    completion_event: Option<&'a str>,
}

pub(crate) fn pending(op_id: &str, completion_event: Option<&str>) -> Value {
    serde_json::to_value(PendingReceipt {
        op_id,
        parked: true,
        status: PendingStatus::Pending,
        message: PENDING_GUIDANCE.trim(),
        completion_event,
    })
    .expect("pending receipt contains only strings, a bool, and an enum")
}

/// A stored forge-action operation as recorded: its outcome, or, while it has none, its pending
/// receipt naming the completion event its payload declares.
pub(crate) async fn show(op: &Operation) -> crate::error::Result<ToolResult> {
    if let Some(result) = operation_result_from(op)? {
        return Ok(output::receipt(result).await);
    }
    let completion_event = completion_event(op)?;
    Ok(ToolResult::structured(pending(
        &op.id,
        completion_event.as_deref(),
    )))
}

/// A forge action's recorded outcome, under the op_id it records.
pub(crate) fn outcome(result: OperationResult) -> ToolResult {
    let op_id = result.op_id;
    match result.outcome {
        OperationOutcome::Succeeded { result } => ToolResult::structured(json!({
            "op_id": op_id,
            "parked": false,
            "status": "succeeded",
            "result": result,
        })),
        OperationOutcome::SucceededViaCollision {
            existing_op_id,
            result,
        } => ToolResult::structured(json!({
            "op_id": op_id,
            "parked": false,
            "status": "succeeded",
            "result": {
                "outcome": "succeeded_via_collision",
                "existing_op_id": existing_op_id,
                "result": result,
            },
        })),
        OperationOutcome::Failed {
            last_error,
            from_phase,
            last_error_class,
        } => {
            let message = format!("forge action operation {op_id} failed: {last_error}");
            ToolResult::structured_error(
                json!({
                    "error": message,
                    "op_id": op_id,
                    "parked": false,
                    "outcome": "failed",
                    "status": "failed",
                    "last_error": last_error,
                    "from_phase": from_phase.as_str(),
                    "last_error_class": last_error_class,
                }),
                message,
            )
        }
        OperationOutcome::Stuck { reason, from_phase } => {
            let message = format!("forge action operation {op_id} stuck: {reason}");
            ToolResult::structured_error(
                json!({
                    "error": message,
                    "op_id": op_id,
                    "parked": false,
                    "outcome": "stuck",
                    "status": "stuck",
                    "reason": reason,
                    "from_phase": from_phase.as_str(),
                }),
                message,
            )
        }
    }
}

/// An approval-gated forge action held at the user's open ask `ask_id`: nothing ran. The answer
/// wakes the Planner, which repeats the identical call.
pub(crate) fn awaiting_approval(ask_id: i64) -> ToolResult {
    ToolResult::structured(json!({
        "status": "awaiting_approval",
        "ask_id": ask_id,
        "message": "Nothing ran: the user is asked to approve this exact call. When the answer \
                    wakes you, repeat the identical call; do not ask separately.",
    }))
}

/// An approval-gated forge action the user declined in ask `ask_id`: it never runs.
pub(crate) fn declined(ask_id: i64) -> ToolResult {
    let message = format!(
        "the user declined this call in ask {ask_id}; nothing ran. A new request needs a new \
         idempotency key and a new approval."
    );
    ToolResult::structured_error(
        json!({ "error": message, "status": "declined", "ask_id": ask_id }),
        message,
    )
}

/// A forge action that never reached a recorded outcome, such as a refused submit.
pub(crate) fn error(message: String) -> ToolResult {
    ToolResult::structured_error(json!({ "error": message }), message)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::operation::{OperationKey, OperationRepo, PhaseTag, SqlxOperationRepo};

    fn wire(op_id: &str, recorded: OperationOutcome) -> Value {
        serde_json::to_value(outcome(OperationResult {
            op_id: op_id.to_owned(),
            outcome: recorded,
        }))
        .unwrap()
    }

    #[test]
    fn outcome_names_the_status_of_every_outcome() {
        let succeeded = wire(
            "op-s",
            OperationOutcome::Succeeded {
                result: json!({ "n": 1 }),
            },
        );
        assert_eq!(succeeded["isError"], false);
        assert_eq!(
            succeeded["structuredContent"],
            json!({ "op_id": "op-s", "parked": false, "status": "succeeded", "result": { "n": 1 } })
        );
        assert_eq!(
            succeeded["content"],
            json!([{ "type": "text", "text": succeeded["structuredContent"].to_string() }])
        );

        let collision = wire(
            "op-c",
            OperationOutcome::SucceededViaCollision {
                existing_op_id: "op-e".into(),
                result: json!({ "n": 2 }),
            },
        );
        assert_eq!(collision["isError"], false);
        assert_eq!(collision["structuredContent"]["status"], "succeeded");
        assert_eq!(
            collision["structuredContent"]["result"],
            json!({ "outcome": "succeeded_via_collision", "existing_op_id": "op-e", "result": { "n": 2 } })
        );

        let failed = wire(
            "op-f",
            OperationOutcome::Failed {
                last_error: "boom".into(),
                from_phase: PhaseTag::Parked,
                last_error_class: Some("permanent".into()),
            },
        );
        let failed_message = "forge action operation op-f failed: boom";
        assert_eq!(failed["isError"], true);
        assert_eq!(
            failed["content"],
            json!([{ "type": "text", "text": failed_message }])
        );
        assert_eq!(
            failed["structuredContent"],
            json!({
                "error": failed_message,
                "op_id": "op-f",
                "parked": false,
                "outcome": "failed",
                "status": "failed",
                "last_error": "boom",
                "from_phase": PhaseTag::Parked.as_str(),
                "last_error_class": "permanent",
            })
        );

        let stuck = wire(
            "op-t",
            OperationOutcome::Stuck {
                reason: "lost".into(),
                from_phase: PhaseTag::SpawnStarted,
            },
        );
        assert_eq!(stuck["isError"], true);
        assert_eq!(stuck["structuredContent"]["outcome"], "stuck");
        assert_eq!(stuck["structuredContent"]["status"], "stuck");
        assert_eq!(stuck["structuredContent"]["reason"], "lost");
        assert_eq!(
            stuck["structuredContent"]["error"],
            "forge action operation op-t stuck: lost"
        );
    }

    async fn stored(payload: Value) -> (SqlxOperationRepo, Operation) {
        let truth = crate::db::sqlite::SqlxRepo::open("sqlite::memory:")
            .await
            .unwrap();
        let repo = SqlxOperationRepo::new(truth.db().clone());
        let op_id = repo
            .insert_operation(
                crate::operation::forge_action_adapter::FORGE_ACTION_KIND,
                OperationKey {
                    operation_key: crate::model::new_id(),
                    idempotency_key: None,
                    payload_hash: "hash".into(),
                },
                payload,
            )
            .await
            .unwrap();
        let op = repo.get_operation(&op_id).await.unwrap().unwrap();
        (repo, op)
    }

    /// A recorded outcome never depends on decoding the payload: only a pending receipt reads it.
    #[tokio::test]
    async fn show_renders_a_recorded_outcome_without_decoding_the_payload() {
        let (repo, op) = stored(json!({ "track_id": "track-a" })).await;
        assert!(
            show(&op).await.is_err(),
            "a pending receipt needs the payload"
        );
        sqlx::query("UPDATE operations SET phase = 'failed', last_error = 'boom' WHERE id = ?1")
            .bind(&op.id)
            .execute(repo.sqlite_db().writer_pool())
            .await
            .unwrap();
        let failed = repo.get_operation(&op.id).await.unwrap().unwrap();
        let shown = serde_json::to_value(show(&failed).await.unwrap()).unwrap();
        assert_eq!(shown["isError"], true);
        assert_eq!(shown["structuredContent"]["status"], "failed");
        assert_eq!(shown["structuredContent"]["last_error"], "boom");
    }
    #[tokio::test]
    async fn recorded_artifact_missing_and_bounded_failure_references_are_honest() {
        use crate::operation::{Phase, TxOutput};
        let (_repo, mut op) = stored(json!({"track_id":"track-a"})).await;
        op.phase = Phase::Succeeded;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("artifact");
        std::fs::write(&path, "<untrusted>\u{202e}").unwrap();
        op.tx_output = Some(TxOutput::new(
            "track",
            None,
            json!({"event":{"artifact_path":path}}),
        ));
        let shown = serde_json::to_value(show(&op).await.unwrap()).unwrap();
        assert_eq!(
            shown["structuredContent"]["result"]["stdout"],
            "<untrusted>\u{202e}"
        );
        assert!(!shown["content"][0]["text"].as_str().unwrap().contains('<'));
        std::fs::remove_file(&path).unwrap();
        let shown = show(&op).await.unwrap().into_structured();
        assert_eq!(shown["result"]["output_unavailable"], true);
        assert!(shown["result"].get("output_ref").is_none());
        assert!(output::read(&op).await.is_err());
        op.phase = Phase::Failed;
        op.last_error = Some("<boom>界".repeat(9000));
        let shown = serde_json::to_value(show(&op).await.unwrap()).unwrap();
        assert_eq!(shown["isError"], true);
        assert!(shown.to_string().len() < 30000);
        assert_eq!(
            shown["structuredContent"]["output_ref"],
            format!("ops/{}/result", op.id)
        );
        let full: Value = serde_json::from_str(&output::read(&op).await.unwrap()).unwrap();
        assert_eq!(full["last_error"], op.last_error.unwrap());
    }
}
