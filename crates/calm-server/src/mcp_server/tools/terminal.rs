//! Planner Terminal tools route through the same card operation and renderer as UI.
use crate::mcp_server::framing::RpcError;
use crate::mcp_server::registry::{
    AppContext, ToolCallIdentity, ToolDescriptor, ToolHandler, ToolHandlerFuture, ToolRegistry,
    require_role,
};
use crate::mcp_server::result::ToolResult;
use crate::model::{Card, CardRole, new_id};
use crate::operation::terminal_adapter::{
    TerminalCreateOperationPayload, TerminalCreateRequestPayload, normalize_terminal_create_request,
};
use crate::operation::{OperationKey, OperationOutcome};
use crate::routes::terminal_cards::stable_payload_hash;
use crate::terminal_interaction::{
    CodexTaskWorkerInputRefused, InputOptions, Target, TerminalInteraction, WaitFor, WaitPlan,
    receipt_summary, summary_line,
};
use serde::Deserialize;
use serde_json::{Value, json};
use std::sync::Arc;
use uuid::Uuid;

pub fn register_into(registry: &mut ToolRegistry) {
    for (name, description, properties, required) in [
        (
            "neige.terminal.resolve",
            include_str!("../../../prompts/tools/neige.terminal.resolve.md").trim_end(),
            json!({"terminal_id":{"type":"string"},"attempt_id":{"type":"string"}}),
            vec![],
        ),
        (
            "neige.terminal.open",
            include_str!("../../../prompts/tools/neige.terminal.open.md").trim_end(),
            json!({"request_id":{"type":"string","minLength":1,"maxLength":128},"title":{"type":"string","maxLength":200},"program":{"type":"string","minLength":1,"maxLength":4096},"claim":{"type":"boolean","default":false},"wait_ms":{"type":"integer","minimum":0,"maximum":20000},"wait_for":{"type":"string","enum":["elapsed","change","signal","text"],"default":"elapsed"},"signal_events":{"type":"array","minItems":1,"items":{"type":"string"}},"wait_text":{"type":"array","minItems":1,"maxItems":8,"items":{"type":"string","minLength":1,"maxLength":200}},"wait_text_absent":{"type":"array","minItems":1,"maxItems":8,"items":{"type":"string","minLength":1,"maxLength":200}},"settle_ms":{"type":"integer","minimum":0,"maximum":2000,"default":150},"repaint_ms":{"type":"integer","minimum":0,"maximum":5000}}),
            vec!["request_id"],
        ),
        (
            "neige.terminal.observe",
            include_str!("../../../prompts/tools/neige.terminal.observe.md").trim_end(),
            json!({"terminal_id":{"type":"string"},"attempt_id":{"type":"string"},"scroll_offset":{"type":"integer","minimum":0,"maximum":2000},"wait_ms":{"type":"integer","minimum":0,"maximum":20000},"wait_for":{"type":"string","enum":["elapsed","change","signal","text"],"default":"elapsed"},"signal_events":{"type":"array","minItems":1,"items":{"type":"string"}},"wait_text":{"type":"array","minItems":1,"maxItems":8,"items":{"type":"string","minLength":1,"maxLength":200}},"wait_text_absent":{"type":"array","minItems":1,"maxItems":8,"items":{"type":"string","minLength":1,"maxLength":200}},"settle_ms":{"type":"integer","minimum":0,"maximum":2000,"default":150},"repaint_ms":{"type":"integer","minimum":0,"maximum":5000}}),
            vec![],
        ),
        (
            "neige.terminal.control",
            include_str!("../../../prompts/tools/neige.terminal.control.md").trim_end(),
            json!({"terminal_id":{"type":"string"},"attempt_id":{"type":"string"},"action":{"type":"string","enum":["claim","release","detach"]},"observe":{"type":"boolean","default":false},"wait_ms":{"type":"integer","minimum":0,"maximum":20000},"wait_for":{"type":"string","enum":["elapsed","change","signal","text"],"default":"elapsed"},"signal_events":{"type":"array","minItems":1,"items":{"type":"string"}},"wait_text":{"type":"array","minItems":1,"maxItems":8,"items":{"type":"string","minLength":1,"maxLength":200}},"wait_text_absent":{"type":"array","minItems":1,"maxItems":8,"items":{"type":"string","minLength":1,"maxLength":200}},"settle_ms":{"type":"integer","minimum":0,"maximum":2000,"default":150},"repaint_ms":{"type":"integer","minimum":0,"maximum":5000}}),
            vec!["action"],
        ),
        (
            "neige.terminal.input",
            include_str!("../../../prompts/tools/neige.terminal.input.md").trim_end(),
            json!({"terminal_id":{"type":"string"},"attempt_id":{"type":"string"},"observation_id":{"type":"string","format":"uuid"},"request_id":{"type":"string","minLength":1,"maxLength":128},"observe":{"type":"boolean","default":false},"wait_ms":{"type":"integer","minimum":0,"maximum":20000},"wait_for":{"type":"string","enum":["elapsed","change","signal","text"],"default":"elapsed"},"signal_events":{"type":"array","minItems":1,"items":{"type":"string"}},"wait_text":{"type":"array","minItems":1,"maxItems":8,"items":{"type":"string","minLength":1,"maxLength":200}},"wait_text_absent":{"type":"array","minItems":1,"maxItems":8,"items":{"type":"string","minLength":1,"maxLength":200}},"settle_ms":{"type":"integer","minimum":0,"maximum":2000,"default":150},"repaint_ms":{"type":"integer","minimum":0,"maximum":5000},"allow_output_since_observation":{"type":"boolean","default":false},"claim":{"type":"boolean","default":false},"release":{"type":"boolean","default":false},
            "action":{"anyOf":[
                {"type":"object","required":["type","text"],"additionalProperties":false,"properties":{"type":{"enum":["text","submit"]},"text":{"type":"string","minLength":1,"maxLength":16384}}},
                {"type":"object","required":["type","key"],"additionalProperties":false,"properties":{"type":{"const":"key"},"key":{"type":"string"},"repeat":{"type":"integer","minimum":1,"maximum":32,"default":1}}},
                {"type":"object","required":["type","steps"],"additionalProperties":false,"properties":{"type":{"const":"sequence"},"steps":{"type":"array","minItems":2,"maxItems":8,"items":{"type":"object"}}}}
            ]}}),
            vec!["request_id", "action"],
        ),
    ] {
        let tool = name.to_owned();
        let handler: ToolHandler = Arc::new(move |ctx, identity, args| -> ToolHandlerFuture {
            let name = tool.clone();
            Box::pin(async move { call(&name, ctx, identity, args).await })
        });
        // No `terminal_id`/`attempt_id` selector arms: duplicating every root property pushed the schema past the 4000-byte compaction threshold; exactly-one targeting is enforced server-side.
        let input_schema = json!({"type":"object","additionalProperties":false,"properties":properties,"required":required});
        registry.register(ToolDescriptor { name:name.into(),description:description.into(),
            input_schema,
            // Terminal programs may reach network/filesystem; no auto-approval annotation.
            annotations:Some(json!({"readOnlyHint":matches!(name,"neige.terminal.observe"|"neige.terminal.resolve"),"destructiveHint":!matches!(name,"neige.terminal.observe"|"neige.terminal.resolve"),"openWorldHint":true})),
            visible_to_roles:&[CardRole::Planner],
        },handler);
    }
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Open {
    request_id: String,
    title: Option<String>,
    program: Option<String>,
    #[serde(default)]
    claim: bool,
    wait_ms: Option<u64>,
    wait_for: Option<WaitFor>,
    settle_ms: Option<u64>,
    signal_events: Option<Vec<String>>,
    repaint_ms: Option<u64>,
    wait_text: Option<Vec<String>>,
    wait_text_absent: Option<Vec<String>>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Observe {
    terminal_id: Option<String>,
    attempt_id: Option<String>,
    #[serde(default)]
    scroll_offset: usize,
    wait_ms: Option<u64>,
    wait_for: Option<WaitFor>,
    settle_ms: Option<u64>,
    signal_events: Option<Vec<String>>,
    repaint_ms: Option<u64>,
    wait_text: Option<Vec<String>>,
    wait_text_absent: Option<Vec<String>>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Control {
    terminal_id: Option<String>,
    attempt_id: Option<String>,
    action: String,
    #[serde(default)]
    observe: bool,
    wait_ms: Option<u64>,
    wait_for: Option<WaitFor>,
    settle_ms: Option<u64>,
    signal_events: Option<Vec<String>>,
    repaint_ms: Option<u64>,
    wait_text: Option<Vec<String>>,
    wait_text_absent: Option<Vec<String>>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Input {
    terminal_id: Option<String>,
    attempt_id: Option<String>,
    observation_id: Option<Uuid>,
    request_id: String,
    action: Value,
    #[serde(default)]
    observe: bool,
    wait_ms: Option<u64>,
    wait_for: Option<WaitFor>,
    settle_ms: Option<u64>,
    signal_events: Option<Vec<String>>,
    repaint_ms: Option<u64>,
    wait_text: Option<Vec<String>>,
    wait_text_absent: Option<Vec<String>>,
    #[serde(default)]
    allow_output_since_observation: bool,
    #[serde(default)]
    claim: bool,
    #[serde(default)]
    release: bool,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Resolve {
    terminal_id: Option<String>,
    attempt_id: Option<String>,
}
fn target(terminal_id: Option<String>, attempt_id: Option<String>) -> Result<Target, RpcError> {
    Target::from_ids(terminal_id, attempt_id)
        .map_err(|error| RpcError::invalid_params(error.to_string()))
}
fn parse<T: serde::de::DeserializeOwned>(args: Value) -> Result<T, RpcError> {
    serde_json::from_value(args).map_err(|error| RpcError::invalid_params(error.to_string()))
}
fn failure(error: impl std::fmt::Display) -> RpcError {
    RpcError::custom(-32403, error.to_string())
}
/// A write refused because the card is a codex task Worker carries a machine-readable `data.refusal`.
fn write_failure(error: anyhow::Error) -> RpcError {
    let mut rpc = failure(&error);
    if error
        .downcast_ref::<CodexTaskWorkerInputRefused>()
        .is_some()
    {
        rpc.data = Some(json!({"refusal":"codex_task_worker_input"}));
    }
    rpc
}
fn observation_result(metadata: Value) -> ToolResult {
    let summary = observation_summary(&metadata);
    ToolResult::structured_with_summary(metadata, summary)
}
/// One line without screen text; the complete state is structuredContent.
fn observation_summary(state: &Value) -> String {
    let text = |field: &str| match &state[field] {
        Value::String(value) => value.clone(),
        Value::Null => "null".into(),
        other => other.to_string(),
    };
    format!(
        "terminal {} observation {} revision {} {} {}x{} cursor {},{} wait {}; \
         full state in structuredContent",
        text("terminal_id"),
        text("observation_id"),
        text("observation_revision"),
        text("role"),
        state["cols"],
        state["rows"],
        state["cursor"]["row"],
        state["cursor"]["column"],
        state["wait"]["outcome"].as_str().unwrap_or("none")
    )
}
/// A detach receipt has no readback; its one line names the closed client.
fn detach_summary(receipt: &Value) -> String {
    format!(
        "terminal {} detached had_client {}; details in structuredContent",
        receipt["terminal_id"].as_str().unwrap_or("null"),
        receipt["had_client"]
    )
}
fn receipt_result(action: &str, mut receipt: Value) -> ToolResult {
    if receipt["detached"] == true {
        let summary = detach_summary(&receipt);
        return ToolResult::structured_with_summary(receipt, summary);
    }
    let summary = receipt_summary(action, &receipt);
    let line = summary_line(receipt["terminal_id"].as_str().unwrap_or("null"), &summary);
    receipt["summary"] = summary;
    ToolResult::structured_with_summary(receipt, line)
}
/// No terminal id exists yet, so the summary names the operation.
fn open_failure_summary(receipt: &Value) -> String {
    format!(
        "terminal open {} operation {}; details in structuredContent",
        receipt["outcome"].as_str().unwrap_or("null"),
        receipt["operation_id"].as_str().unwrap_or("null")
    )
}
fn open_failure_result(receipt: Value) -> ToolResult {
    let summary = open_failure_summary(&receipt);
    ToolResult::structured_with_summary(receipt, summary)
}
/// The waiting arguments every wait carrier accepts, in declaration order.
struct WaitArgs {
    wait_for: Option<WaitFor>,
    wait_ms: Option<u64>,
    settle_ms: Option<u64>,
    signal_events: Option<Vec<String>>,
    repaint_ms: Option<u64>,
    wait_text: Option<Vec<String>>,
    wait_text_absent: Option<Vec<String>>,
}
impl WaitArgs {
    fn any(&self) -> bool {
        self.wait_ms.is_some()
            || self.wait_for.is_some()
            || self.settle_ms.is_some()
            || self.signal_events.is_some()
            || self.repaint_ms.is_some()
            || self.wait_text.is_some()
            || self.wait_text_absent.is_some()
    }
    /// A wait that tests live viewport rows needs `scroll_offset` 0 (the service refuses it again).
    fn tests_text(&self) -> bool {
        match self.wait_for {
            Some(WaitFor::Text) => true,
            Some(WaitFor::Signal) => self.wait_text.is_some() || self.wait_text_absent.is_some(),
            _ => false,
        }
    }
    fn plan(self) -> Result<WaitPlan, RpcError> {
        WaitPlan::new(
            self.wait_for,
            self.wait_ms,
            self.settle_ms,
            self.signal_events,
            self.repaint_ms,
            self.wait_text,
            self.wait_text_absent,
        )
        .map_err(|error| RpcError::invalid_params(error.to_string()))
    }
}
fn action_observation(
    observe: bool,
    wait: WaitArgs,
    detach: bool,
) -> Result<Option<WaitPlan>, RpcError> {
    if (wait.any() && !observe) || (detach && observe) {
        return Err(RpcError::invalid_params(
            "wait_ms/wait_for/settle_ms/signal_events/repaint_ms/wait_text/wait_text_absent need observe=true; detach cannot observe",
        ));
    }
    if !observe {
        return Ok(None);
    }
    wait.plan().map(Some)
}
/// The idempotency hash view of an open. The generated hook env never enters it, so a replayed request_id hashes identically.
fn open_payload_hash(
    identity: &ToolCallIdentity,
    request: &TerminalCreateRequestPayload,
) -> Result<String, RpcError> {
    let mut view = serde_json::to_value(request).map_err(failure)?;
    if let Some(env) = view.get_mut("env").and_then(Value::as_object_mut) {
        for key in crate::terminal_hooks::TERMINAL_HOOK_ENV_KEYS {
            env.remove(key);
        }
    }
    let view = json!({"actor":identity.to_actor_id(),"request":view,"planner_hooks":true});
    stable_payload_hash(&view).map_err(failure)
}
async fn call(
    name: &str,
    ctx: Arc<AppContext>,
    identity: ToolCallIdentity,
    args: Value,
) -> Result<ToolResult, RpcError> {
    require_role(&identity, CardRole::Planner)?;
    let service = ctx
        .terminal_interaction
        .get()
        .ok_or_else(|| RpcError::internal("terminal interaction unavailable"))?;
    match name {
        "neige.terminal.resolve" => {
            let args: Resolve = parse(args)?;
            let resolved = service
                .resolve(&identity, &target(args.terminal_id, args.attempt_id)?)
                .await
                .map_err(failure)?;
            let summary = format!(
                "terminal {} resolved available {} controllable {}; details in structuredContent",
                resolved["terminal_id"].as_str().unwrap_or("null"),
                resolved["available"],
                resolved["controllable"]
            );
            Ok(ToolResult::structured_with_summary(resolved, summary))
        }
        "neige.terminal.open" => {
            let args: Open = parse(args)?;
            if args.request_id.is_empty()
                || args.request_id.len() > 128
                || args.title.as_ref().is_some_and(|s| s.len() > 200)
                || args
                    .program
                    .as_ref()
                    .is_some_and(|s| s.is_empty() || s.len() > 4096 || s.contains('\0'))
            {
                return Err(RpcError::invalid_params(
                    "invalid terminal request_id or title",
                ));
            }
            // The wait arguments never enter the idempotency hash.
            let wait = WaitArgs {
                wait_for: args.wait_for,
                wait_ms: args.wait_ms,
                settle_ms: args.settle_ms,
                signal_events: args.signal_events,
                repaint_ms: args.repaint_ms,
                wait_text: args.wait_text,
                wait_text_absent: args.wait_text_absent,
            };
            let waited = wait.any();
            let wait = wait.plan()?;
            let track_id = TerminalInteraction::authorize(ctx.repo.as_ref(), &identity)
                .await
                .map_err(failure)?;
            let idempotency_key = format!(
                "planner-terminal:{}:{}",
                identity.session_id, args.request_id
            );
            let runtime = ctx
                .operation_runtime
                .get()
                .ok_or_else(|| RpcError::internal("operation runtime unavailable"))?;
            let request = normalize_terminal_create_request(TerminalCreateRequestPayload {
                track_id: track_id.clone(),
                title: args.title,
                sort: None,
                program: args.program.unwrap_or_default(),
                cwd: String::new(),
                env: json!({}),
                theme: crate::routes::theme::RequestTheme::default_dark(),
            });
            let key = OperationKey {
                operation_key: new_id(),
                idempotency_key: Some(idempotency_key),
                payload_hash: open_payload_hash(&identity, &request)?,
            };
            let payload = serde_json::to_value(TerminalCreateOperationPayload {
                actor: identity.to_actor_id(),
                worker_session_id: Some(new_id()),
                planner_hooks: true,
                request,
            })
            .map_err(failure)?;
            let operation = runtime
                .submit("terminal-create", key, payload)
                .await
                .map_err(failure)?;
            let outcome = runtime.wait(&operation).await.map_err(failure)?;
            let card: Card = match outcome.outcome {
                OperationOutcome::Succeeded { result }
                | OperationOutcome::SucceededViaCollision { result, .. } => {
                    serde_json::from_value(result).map_err(failure)?
                }
                other => {
                    return Ok(open_failure_result(
                        json!({"operation_id":operation,"outcome":"unavailable","detail":format!("{other:?}")}),
                    ));
                }
            };
            let terminal = ctx
                .repo
                .terminal_get_by_card(card.id.as_str())
                .await
                .map_err(failure)?
                .ok_or_else(|| RpcError::internal("created card has no terminal"))?;
            let target = Target::Terminal(terminal.id.clone());
            // Establish the observation client before the Planner enters a TUI. With a wait this immediate read is the baseline of the final observation.
            let mut metadata = service
                .observe(&identity, &target, 0, WaitPlan::default())
                .await
                .map_err(failure)?;
            let mut claim = None;
            if args.claim {
                // Claim-if-unowned; the open already succeeded whatever the claim does. The wait runs after the claim, never inside its serial guard.
                match service
                    .claim_after_open(&identity, &target, WaitPlan::default())
                    .await
                {
                    Ok(receipt) if receipt["observation"]["status"] == "available" => {
                        claim =
                            Some(json!({"status":"claimed","control_id":receipt["control_id"]}));
                        if !waited {
                            metadata = receipt["observation"]["state"].clone();
                        }
                    }
                    Ok(receipt) => {
                        claim = Some(
                            json!({"status":"unavailable","control_id":receipt["control_id"],
                            "reason":format!("claim readback unavailable: {}", receipt["observation"]["reason"].as_str().unwrap_or("unknown"))}),
                        );
                    }
                    Err(error) => {
                        claim = Some(json!({"status":"unavailable","reason":error.to_string()}));
                    }
                }
            }
            if waited {
                // A failed claim still returns the waited state.
                metadata = service
                    .observe(&identity, &target, 0, wait)
                    .await
                    .map_err(failure)?;
            }
            if let Some(claim) = claim {
                metadata["claim"] = claim;
            }
            metadata["card_id"] = json!(card.id);
            metadata["operation_id"] = json!(operation);
            Ok(observation_result(metadata))
        }
        "neige.terminal.observe" => {
            let args: Observe = parse(args)?;
            if args.scroll_offset > 2000 {
                return Err(RpcError::invalid_params(
                    "scroll_offset exceeds history limit",
                ));
            }
            let wait = WaitArgs {
                wait_for: args.wait_for,
                wait_ms: args.wait_ms,
                settle_ms: args.settle_ms,
                signal_events: args.signal_events,
                repaint_ms: args.repaint_ms,
                wait_text: args.wait_text,
                wait_text_absent: args.wait_text_absent,
            };
            if wait.tests_text() && args.scroll_offset > 0 {
                return Err(RpcError::invalid_params(
                    "wait_for=text or text conditions observe the live viewport; scroll_offset must be 0",
                ));
            }
            let wait = wait.plan()?;
            let metadata = service
                .observe(
                    &identity,
                    &target(args.terminal_id, args.attempt_id)?,
                    args.scroll_offset,
                    wait,
                )
                .await
                .map_err(failure)?;
            Ok(observation_result(metadata))
        }
        "neige.terminal.control" => {
            let args: Control = parse(args)?;
            let readback = action_observation(
                args.observe,
                WaitArgs {
                    wait_for: args.wait_for,
                    wait_ms: args.wait_ms,
                    settle_ms: args.settle_ms,
                    signal_events: args.signal_events,
                    repaint_ms: args.repaint_ms,
                    wait_text: args.wait_text,
                    wait_text_absent: args.wait_text_absent,
                },
                args.action == "detach",
            )?;
            service
                .control(
                    &identity,
                    &target(args.terminal_id, args.attempt_id)?,
                    &args.action,
                    readback,
                )
                .await
                .map(|receipt| receipt_result(&args.action, receipt))
                .map_err(write_failure)
        }
        "neige.terminal.input" => {
            let args: Input = parse(args)?;
            let readback = action_observation(
                args.observe,
                WaitArgs {
                    wait_for: args.wait_for,
                    wait_ms: args.wait_ms,
                    settle_ms: args.settle_ms,
                    signal_events: args.signal_events,
                    repaint_ms: args.repaint_ms,
                    wait_text: args.wait_text,
                    wait_text_absent: args.wait_text_absent,
                },
                false,
            )?;
            service
                .input(
                    &identity,
                    &target(args.terminal_id, args.attempt_id)?,
                    args.observation_id,
                    &args.request_id,
                    args.action,
                    InputOptions {
                        allow_output_since_observation: args.allow_output_since_observation,
                        claim: args.claim,
                        release: args.release,
                    },
                    readback,
                )
                .await
                .map(|receipt| receipt_result("input", receipt))
                .map_err(write_failure)
        }
        _ => Err(RpcError::invalid_params("unknown terminal tool")),
    }
}

#[cfg(test)]
mod schema_tests;
#[cfg(test)]
mod summary_tests;
