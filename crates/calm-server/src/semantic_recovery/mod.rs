//! Semantic decisions are bound to immutable, acknowledged provider turns.
//! Registration is capability discovery; only the recovery transaction grants authority.
mod store;
pub(crate) use store::{
    Action, BoundTurn, authenticate_tx, bind_turn, binding_problem, prepare, register, registered,
};

use crate::codex_appserver::{
    CodexAppServer, DynamicToolCallParams, DynamicToolCallResponse, DynamicToolRequest,
};
use crate::db::Repo;
use crate::error::{CalmError, Result};
use crate::event::EventBus;
use crate::ids::ActorId;
use crate::state::WriteContext;
use calm_types::task_recovery::TaskRecoveryRequest;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::sync::Arc;
use std::time::Duration;

pub(crate) const TOOL: &str = "Recover";
pub(crate) fn descriptor() -> Value {
    json!({"type":"function", "name":TOOL,
        "description":"Recover the task from this turn's kernel recovery briefing using only key and reason. Use only when that briefing permits recovery. The kernel binds the original execution and checks current permission. Receipt means accepted/allotted, not Worker started. New workspace; declared immutable inputs retain their binding. Previous Worker outputs are not implicitly inherited. Retry unchanged reason after response loss. Recovery re-runs on the executor of the failed attempt with its unchanged environment and capabilities; for isolated Codex that is the identical execution environment and only the workspace is new. It cannot resolve a failure caused by a missing capability (for example no network); change the task's goal or inputs instead. The response states the actual route as executor_environment.",
        "inputSchema":{"type":"object","additionalProperties":false,"required":["key","reason"],
        "properties":{"key":{"type":"string","minLength":1,"maxLength":200},"reason":{"type":"string","minLength":1,"maxLength":4000}}}})
}

#[derive(Clone)]
pub(crate) struct RecoveryService {
    pub repo: Arc<dyn Repo>,
    pub events: EventBus,
    pub write: WriteContext,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Args {
    key: String,
    reason: String,
}

impl RecoveryService {
    /// Receiver belongs to one connection and retains no client Arc. The transport
    /// cancels its response on EOF; at most 16 independent jobs may wait on ACKs.
    pub(crate) fn install(&self, client: &Arc<CodexAppServer>) -> Result<()> {
        let mut receiver = client.take_dynamic_tool_requests()?;
        let service = self.clone();
        tokio::spawn(async move {
            let mut jobs = tokio::task::JoinSet::new();
            loop {
                tokio::select! {
                    request = receiver.recv(), if jobs.len() < 16 => {
                        let Some(request) = request else { break; };
                        let service = service.clone();
                        jobs.spawn(async move { service.respond(request).await; });
                    }
                    _ = jobs.join_next(), if !jobs.is_empty() => {}
                }
            }
            while jobs.join_next().await.is_some() {}
        });
        Ok(())
    }

    async fn respond(&self, mut request: DynamicToolRequest) {
        if request.is_cancelled() {
            return;
        }
        let params = request.params.clone();
        // Cancellation may race a commit; unchanged retries use the recovery
        // transaction's receipt. No consumer survives EOF or the response budget.
        let result = tokio::select! {
            _ = request.cancelled() => return,
            result = tokio::time::timeout(Duration::from_secs(25), self.execute(&params, || false)) =>
                result.unwrap_or_else(|_| Err(CalmError::ServiceUnavailable("recovery call timed out; retry unchanged to resolve its receipt".into()))),
        };
        let response = match result {
            Ok(value) => DynamicToolCallResponse::text(true, value.to_string()),
            Err(error) => {
                let status = match &error {
                    CalmError::ServiceUnavailable(_) => "not_ready",
                    CalmError::Conflict(_) => "conflict",
                    _ => "precondition",
                };
                DynamicToolCallResponse::text(
                    false,
                    json!({"status":status,"reason":error.to_string()}).to_string(),
                )
            }
        };
        let _ = request.respond(response);
    }

    async fn execute(
        &self,
        params: &DynamicToolCallParams,
        cancelled: impl Fn() -> bool,
    ) -> Result<Value> {
        if params.tool != TOOL || params.namespace.is_some() {
            return Err(CalmError::BadRequest(
                "unsupported semantic tool or namespace".into(),
            ));
        }
        for id in [&params.thread_id, &params.turn_id, &params.call_id] {
            if id.is_empty() || id.len() > 512 {
                return Err(CalmError::BadRequest(
                    "invalid provider call identity".into(),
                ));
            }
        }
        let args: Args = serde_json::from_value(params.arguments.clone())?;
        if !calm_types::report_blocks::tasks::key_is_valid(&args.key)
            || args.key.len() > 200
            || args.reason.trim().is_empty()
            || args.reason.len() > 4000
        {
            return Err(CalmError::BadRequest(
                "valid key and reason (1-4000 bytes) required".into(),
            ));
        }
        let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
        let bound = loop {
            if cancelled() {
                return Err(CalmError::ServiceUnavailable(
                    "call cancelled; no recovery attempted".into(),
                ));
            }
            if let Some(bound) = tokio::time::timeout_at(
                deadline,
                store::lookup(self.repo.as_ref(), &params.thread_id, &params.turn_id),
            )
            .await
            .map_err(|_| {
                CalmError::ServiceUnavailable("original turn binding not ready".into())
            })?? {
                break bound;
            }
            if tokio::time::Instant::now() >= deadline {
                return Err(CalmError::ServiceUnavailable("original turn binding not ready; retry this decision unchanged, never substitute an execution".into()));
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        };
        let action = bound
            .actions
            .iter()
            .find(|action| action.key == args.key)
            .ok_or_else(|| {
                CalmError::Conflict("task has no action in this turn's briefing".into())
            })?;
        store::record_call(
            self.repo.as_ref(),
            &bound,
            params,
            serde_json::to_vec(&args)?,
        )
        .await?;
        if !action.capability.allowed {
            return Err(CalmError::Forbidden(format!(
                "{}: {}",
                action.capability.code, action.capability.reason
            )));
        }
        if cancelled() {
            return Err(CalmError::ServiceUnavailable(
                "call cancelled before recovery".into(),
            ));
        }
        let receipt = crate::task_recovery::recover_failed_task_bound(
            crate::task_recovery::RecoveryContext {
                repo: self.repo.as_ref(),
                events: &self.events,
                write: &self.write,
            },
            &bound.track_id,
            &args.key,
            TaskRecoveryRequest {
                expected_attempt_id: action.expected_attempt_id.clone(),
                idempotency_key: action.request_key.clone(),
                reason: args.reason,
            },
            ActorId::AiPlannerSession(bound.session_id.clone().into()),
            bound.clone(),
        )
        .await?;
        let statement =
            crate::task_recovery::executor_statement_for_receipt(self.repo.as_ref(), &receipt)
                .await?;
        Ok(json!({
            "status": "accepted",
            "receipt": receipt,
            "limitations": "Accepted/allotted does not prove Worker startup. New workspace; declared immutable inputs retain their binding. Previous Worker outputs are not implicitly inherited.",
            "executor_environment": statement.environment,
            "recover_changes": statement.recover_changes,
        }))
    }
}

#[cfg(test)]
mod tests;

/// Production service seams for the isolated stack regression; no fake business logic.
#[cfg(feature = "fixtures")]
pub mod test_support {
    use super::*;
    pub async fn register_thread(repo: &dyn Repo, card: &str, thread: &str) -> Result<()> {
        register(repo, card, thread).await
    }
    pub async fn connection(
        repo: Arc<dyn Repo>,
        events: EventBus,
        write: WriteContext,
    ) -> (
        Arc<CodexAppServer>,
        crate::codex_appserver::NotificationStream,
        tokio_tungstenite::WebSocketStream<tokio::net::UnixStream>,
    ) {
        let (client, notifications, server) = CodexAppServer::connect_pair_for_test().await;
        let client = Arc::new(client);
        RecoveryService {
            repo,
            events,
            write,
        }
        .install(&client)
        .unwrap();
        (client, notifications, server)
    }
    pub async fn call(
        repo: Arc<dyn Repo>,
        events: EventBus,
        write: WriteContext,
        params: DynamicToolCallParams,
    ) -> Result<Value> {
        RecoveryService {
            repo,
            events,
            write,
        }
        .execute(&params, || false)
        .await
    }
}
