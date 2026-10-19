//! Typed RPC entry points with a caller-owned asynchronous pre-send check.
use super::*;
use futures_util::future::BoxFuture;

/// Opaque caller policy, invoked with the outbound sink held and before sending.
/// A refusal proves this request did not enter send; the provider interprets no policy.
/// The gate must not issue requests on this connection while holding its sink.
pub struct PreSendGate<'a> {
    check: Box<dyn Fn() -> BoxFuture<'a, Result<()>> + Send + Sync + 'a>,
}

impl<'a> PreSendGate<'a> {
    pub fn new(check: impl Fn() -> BoxFuture<'a, Result<()>> + Send + Sync + 'a) -> Self {
        Self {
            check: Box::new(check),
        }
    }

    // Only the transport request core may consume the policy. Callers can
    // carry this value across preparation awaits, never extract a prior grant.
    pub(super) async fn run(&self) -> Result<()> {
        (self.check)().await
    }
}

pub(super) fn allow_send() -> BoxFuture<'static, Result<()>> {
    Box::pin(async { Ok(()) })
}

impl CodexAppServer {
    pub async fn thread_start_with_params_gated(
        &self,
        params: ThreadStartParams,
        gate: &PreSendGate<'_>,
    ) -> std::result::Result<ThreadResult, RpcAttemptFailure> {
        self.request_attempt_gated("thread/start", thread_start_params(params), gate)
            .await
    }

    pub async fn thread_read_full_gated(
        &self,
        thread_id: &str,
        gate: &PreSendGate<'_>,
    ) -> Result<ThreadResult> {
        self.request_gated(
            "thread/read",
            json!({"threadId":thread_id,"includeTurns":true}),
            gate,
        )
        .await
    }

    pub async fn thread_resume_with_config_gated(
        &self,
        thread_id: &str,
        config: Option<serde_json::Value>,
        gate: &PreSendGate<'_>,
    ) -> Result<ThreadResult> {
        let mut value = json!({ "threadId": thread_id });
        if let Some(config) = config {
            value["config"] = config;
        }
        self.request_gated("thread/resume", value, gate).await
    }

    pub async fn turn_start_attempt_gated(
        &self,
        thread_id: &str,
        input: Vec<InputItem>,
        selection: &TurnModelSelection,
        approvals: TurnApprovals,
        client_user_message_id: Option<&str>,
        gate: &PreSendGate<'_>,
    ) -> std::result::Result<TurnStartResult, RpcAttemptFailure> {
        self.request_attempt_gated(
            "turn/start",
            turn_start_params(
                thread_id,
                &input,
                selection,
                approvals,
                client_user_message_id,
            ),
            gate,
        )
        .await
    }

    pub async fn model_list_gated(
        &self,
        cursor: Option<&str>,
        deadline: tokio::time::Instant,
        gate: &PreSendGate<'_>,
    ) -> Result<ModelListPage> {
        let mut params = json!({ "includeHidden": false });
        if let Some(cursor) = cursor {
            params["cursor"] = Value::String(cursor.to_string());
        }
        self.request_until_gated("model/list", params, deadline, gate)
            .await
    }

    pub async fn config_read_gated(
        &self,
        cwd: Option<&str>,
        deadline: tokio::time::Instant,
        gate: &PreSendGate<'_>,
    ) -> Result<ConfigReadResponse> {
        let mut params = json!({ "includeLayers": false });
        if let Some(cwd) = cwd {
            params["cwd"] = Value::String(cwd.to_string());
        }
        self.request_until_gated("config/read", params, deadline, gate)
            .await
    }

    pub async fn turn_steer_attempt_gated(
        &self,
        thread_id: &str,
        expected_turn_id: &str,
        input: Vec<InputItem>,
        client_user_message_id: Option<&str>,
        gate: &PreSendGate<'_>,
    ) -> std::result::Result<TurnSteerResult, RpcAttemptFailure> {
        self.request_attempt_gated(
            "turn/steer",
            turn_steer_params(thread_id, expected_turn_id, &input, client_user_message_id),
            gate,
        )
        .await
    }
}

// The typed gated and existing entry points share one protocol builder.
fn thread_start_params(params: ThreadStartParams) -> Value {
    let mut value = json!({
        "cwd": params.cwd,
        "approvalPolicy": params.approval_policy,
        "sandbox": params.sandbox_mode,
    });
    if let Some(prompt) = params.developer_instructions {
        value["developerInstructions"] = Value::String(prompt);
    }
    if let Some(config) = params.config {
        value["config"] = config;
    }
    value
}
