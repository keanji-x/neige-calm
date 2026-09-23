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
    BELOW_CURSOR_EDITS_ONLY, CodexTaskWorkerInputRefused, InputOptions, ObservationFormat,
    Occurrence, ScrollTo, Target, TerminalInteraction, WaitFor, WaitPlan, edits_the_draft,
    receipt_summary, summary_line,
};
use crate::terminal_permissions::{
    ClaudePermissionsScope, apply_policy, parse_scope, validate_scope, wait_at_ceiling_checked_hook,
};
use crate::validation::{
    TERMINAL_CLAUDE_PERMISSIONS_PAYLOAD_KEY, TERMINAL_CLAUDE_PERMISSIONS_SOURCE_PAYLOAD_KEY,
};
use serde::Deserialize;
use serde_json::{Value, json};
use std::sync::Arc;
use uuid::Uuid;

pub fn register_into(registry: &mut ToolRegistry) {
    for (name, description, properties, required) in [
        (
            "calm.terminal.resolve",
            include_str!("../../../prompts/tools/calm.terminal.resolve.md").trim_end(),
            json!({"terminal_id":{"type":"string"},"task_id":{"type":"string"}}),
            vec![],
        ),
        (
            "calm.terminal.open",
            include_str!("../../../prompts/tools/calm.terminal.open.md").trim_end(),
            json!({"request_id":{"type":"string","minLength":1,"maxLength":128},"title":{"type":"string","maxLength":200},"program":{"type":"string","minLength":1,"maxLength":4096},"format":{"type":"string","enum":["text","image"],"default":"text"},"claim":{"type":"boolean","default":false},"wait_ms":{"type":"integer","minimum":0,"maximum":20000},"wait_for":{"type":"string","enum":["elapsed","change","signal","text"],"default":"elapsed"},"signal_events":{"type":"array","minItems":1,"items":{"type":"string"}},"wait_text":{"type":"array","minItems":1,"maxItems":8,"items":{"type":"string","minLength":1,"maxLength":200}},"wait_text_absent":{"type":"array","minItems":1,"maxItems":8,"items":{"type":"string","minLength":1,"maxLength":200}},"settle_ms":{"type":"integer","minimum":0,"maximum":2000,"default":150},"repaint_ms":{"type":"integer","minimum":0,"maximum":5000},
            // The caps mirror `terminal_permissions`.
            "claude_permissions":{"type":"object","additionalProperties":false,"properties":{"edit":{"type":"array","minItems":1,"maxItems":16,"items":{"type":"string","minLength":1,"maxLength":200}},"bash":{"type":"array","minItems":1,"maxItems":32,"items":{"type":"string","minLength":1,"maxLength":200}},"deny":{"type":"array","maxItems":32,"items":{"type":"string","minLength":1,"maxLength":200}}}}}),
            vec!["request_id"],
        ),
        (
            "calm.terminal.observe",
            include_str!("../../../prompts/tools/calm.terminal.observe.md").trim_end(),
            json!({"terminal_id":{"type":"string"},"task_id":{"type":"string"},"scroll_offset":{"type":"integer","minimum":0,"maximum":2000},"wait_ms":{"type":"integer","minimum":0,"maximum":20000},"wait_for":{"type":"string","enum":["elapsed","change","signal","text"],"default":"elapsed"},"signal_events":{"type":"array","minItems":1,"items":{"type":"string"}},"wait_text":{"type":"array","minItems":1,"maxItems":8,"items":{"type":"string","minLength":1,"maxLength":200}},"wait_text_absent":{"type":"array","minItems":1,"maxItems":8,"items":{"type":"string","minLength":1,"maxLength":200}},"settle_ms":{"type":"integer","minimum":0,"maximum":2000,"default":150},"repaint_ms":{"type":"integer","minimum":0,"maximum":5000},"format":{"type":"string","enum":["text","image"],"default":"text"},
            "scroll_to_text":{"type":"string","minLength":1,"maxLength":200},"scroll_to_occurrence":{"type":"string","enum":["latest","earliest"],"default":"latest"}}),
            vec![],
        ),
        (
            "calm.terminal.control",
            include_str!("../../../prompts/tools/calm.terminal.control.md").trim_end(),
            json!({"terminal_id":{"type":"string"},"task_id":{"type":"string"},"action":{"type":"string","enum":["claim","release","detach"]},"observe":{"type":"boolean","default":false},"wait_ms":{"type":"integer","minimum":0,"maximum":20000},"wait_for":{"type":"string","enum":["elapsed","change","signal","text"],"default":"elapsed"},"signal_events":{"type":"array","minItems":1,"items":{"type":"string"}},"wait_text":{"type":"array","minItems":1,"maxItems":8,"items":{"type":"string","minLength":1,"maxLength":200}},"wait_text_absent":{"type":"array","minItems":1,"maxItems":8,"items":{"type":"string","minLength":1,"maxLength":200}},"settle_ms":{"type":"integer","minimum":0,"maximum":2000,"default":150},"repaint_ms":{"type":"integer","minimum":0,"maximum":5000}}),
            vec!["action"],
        ),
        (
            "calm.terminal.input",
            include_str!("../../../prompts/tools/calm.terminal.input.md").trim_end(),
            json!({"terminal_id":{"type":"string"},"task_id":{"type":"string"},"observation_id":{"type":"string","format":"uuid"},"request_id":{"type":"string","minLength":1,"maxLength":128},"observe":{"type":"boolean","default":false},"wait_ms":{"type":"integer","minimum":0,"maximum":20000},"wait_for":{"type":"string","enum":["elapsed","change","signal","text"],"default":"elapsed"},"signal_events":{"type":"array","minItems":1,"items":{"type":"string"}},"wait_text":{"type":"array","minItems":1,"maxItems":8,"items":{"type":"string","minLength":1,"maxLength":200}},"wait_text_absent":{"type":"array","minItems":1,"maxItems":8,"items":{"type":"string","minLength":1,"maxLength":200}},"settle_ms":{"type":"integer","minimum":0,"maximum":2000,"default":150},"repaint_ms":{"type":"integer","minimum":0,"maximum":5000},"allow_output_since_observation":{"type":"boolean","default":false},"allow_output_below_cursor":{"type":"boolean","default":false},"claim":{"type":"boolean","default":false},"release":{"type":"boolean","default":false},
            "action":{"anyOf":[
                {"type":"object","required":["type","text"],"additionalProperties":false,"properties":{"type":{"enum":["text","submit"]},"text":{"type":"string","minLength":1,"maxLength":16384}}},
                {"type":"object","required":["type","key"],"additionalProperties":false,"properties":{"type":{"const":"key"},"key":{"type":"string"},"repeat":{"type":"integer","minimum":1,"maximum":32,"default":1}}},
                {"type":"object","required":["type","column","row"],"additionalProperties":false,"properties":{"type":{"const":"click"},"column":{"type":"integer","minimum":0},"row":{"type":"integer","minimum":0}}},
                {"type":"object","required":["type","steps"],"additionalProperties":false,"properties":{"type":{"const":"sequence"},"steps":{"type":"array","minItems":2,"maxItems":8,"items":{"type":"object"}}}},
                {"type":"object","required":["type","from","to"],"additionalProperties":false,"properties":{"type":{"const":"replace"},"from":{"type":"string","minLength":1,"maxLength":200},"to":{"type":"string","maxLength":16384}}}
            ]}}),
            vec!["request_id", "action"],
        ),
    ] {
        let tool = name.to_owned();
        let handler: ToolHandler = Arc::new(move |ctx, identity, args| -> ToolHandlerFuture {
            let name = tool.clone();
            Box::pin(async move { call(&name, ctx, identity, args).await })
        });
        // No `terminal_id`/`task_id` selector arms: duplicating every root property pushed the schema past the 4000-byte compaction threshold; exactly-one targeting is enforced server-side.
        let input_schema = json!({"type":"object","additionalProperties":false,"properties":properties,"required":required});
        registry.register(ToolDescriptor { name:name.into(),description:description.into(),
            input_schema,
            // Terminal programs may reach network/filesystem; no auto-approval annotation.
            annotations:Some(json!({"readOnlyHint":matches!(name,"calm.terminal.observe"|"calm.terminal.resolve"),"destructiveHint":!matches!(name,"calm.terminal.observe"|"calm.terminal.resolve"),"openWorldHint":true})),
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
    format: ObservationFormat,
    #[serde(default)]
    claim: bool,
    wait_ms: Option<u64>,
    wait_for: Option<WaitFor>,
    settle_ms: Option<u64>,
    signal_events: Option<Vec<String>>,
    repaint_ms: Option<u64>,
    wait_text: Option<Vec<String>>,
    wait_text_absent: Option<Vec<String>>,
    /// Shape checked by `parse_scope` (a JSON `null` or array is refused by name rather than read as absent); part of the idempotency hash.
    #[serde(default, deserialize_with = "present_value")]
    claude_permissions: Option<Value>,
}
/// Keeps a JSON `null` as `Some(Null)` (serde's default reads `null` as `None`) so it is refused as "must be an object".
fn present_value<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<Value>, D::Error> {
    Value::deserialize(deserializer).map(Some)
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Observe {
    terminal_id: Option<String>,
    task_id: Option<String>,
    #[serde(default)]
    scroll_offset: usize,
    wait_ms: Option<u64>,
    wait_for: Option<WaitFor>,
    settle_ms: Option<u64>,
    signal_events: Option<Vec<String>>,
    repaint_ms: Option<u64>,
    wait_text: Option<Vec<String>>,
    wait_text_absent: Option<Vec<String>>,
    #[serde(default)]
    format: ObservationFormat,
    scroll_to_text: Option<String>,
    scroll_to_occurrence: Option<String>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Control {
    terminal_id: Option<String>,
    task_id: Option<String>,
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
    task_id: Option<String>,
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
    allow_output_below_cursor: bool,
    #[serde(default)]
    claim: bool,
    #[serde(default)]
    release: bool,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Resolve {
    terminal_id: Option<String>,
    task_id: Option<String>,
}
fn target(terminal_id: Option<String>, task_id: Option<String>) -> Result<Target, RpcError> {
    Target::from_ids(terminal_id, task_id)
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
fn observation_result(metadata: Value, png: Option<Vec<u8>>) -> Result<ToolResult, RpcError> {
    match png {
        Some(png) => ToolResult::png(metadata, &png),
        None => {
            let summary = observation_summary(&metadata);
            Ok(ToolResult::structured_with_summary(metadata, summary))
        }
    }
}
/// One line without screen text; the complete state is structuredContent.
fn observation_summary(state: &Value) -> String {
    let text = |field: &str| match &state[field] {
        Value::String(value) => value.clone(),
        Value::Null => "null".into(),
        other => other.to_string(),
    };
    let permissions = match state[TERMINAL_CLAUDE_PERMISSIONS_PAYLOAD_KEY].as_object() {
        Some(block) => {
            let rules = |list: &str| {
                block
                    .get(list)
                    .and_then(Value::as_array)
                    .map_or(0, Vec::len)
            };
            let source = match state[TERMINAL_CLAUDE_PERMISSIONS_SOURCE_PAYLOAD_KEY].as_str() {
                Some(source) => format!(" source {source}"),
                None => String::new(),
            };
            format!(
                " permissions allow {} ask {} deny {}{source}",
                rules("allow"),
                rules("ask"),
                rules("deny")
            )
        }
        None => String::new(),
    };
    let scroll_to = match state["scroll_to"]["status"].as_str() {
        Some("found") => format!(" scroll_to found row {}", state["scroll_to"]["row"]),
        Some(status) => format!(" scroll_to {status}"),
        None => String::new(),
    };
    format!(
        "terminal {} observation {} revision {} {} {}x{} cursor {},{} wait {}\
         {permissions}{scroll_to}; full state in structuredContent",
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
/// With `format=image` a failed render falls back to a text observation plus `image: {status: unavailable, reason}`; only a failed text observation is an error.
async fn observe_for_open(
    service: &TerminalInteraction,
    identity: &ToolCallIdentity,
    terminal_id: &str,
    wait: WaitPlan,
    format: ObservationFormat,
) -> anyhow::Result<(Value, Option<Vec<u8>>)> {
    let target = Target::Terminal(terminal_id.to_owned());
    let attempt = service
        .observe(identity, &target, 0, wait, format, None)
        .await;
    match attempt {
        Ok(observed) => Ok(observed),
        Err(error) if format == ObservationFormat::Image => {
            let (mut metadata, mut png) = service
                .observe(
                    identity,
                    &target,
                    0,
                    WaitPlan::default(),
                    ObservationFormat::Text,
                    None,
                )
                .await?;
            apply_image_outcome(&mut metadata, &mut png, Err(error));
            Ok((metadata, png))
        }
        Err(error) => Err(error),
    }
}
/// Success replaces the state and PNG; failure keeps the text state and reports `image: {status: unavailable, reason}`.
fn apply_image_outcome(
    metadata: &mut Value,
    png: &mut Option<Vec<u8>>,
    image: anyhow::Result<(Value, Option<Vec<u8>>)>,
) {
    match image {
        Ok((image_metadata, image_png)) => {
            *metadata = image_metadata;
            *png = image_png;
        }
        Err(error) => {
            *png = None;
            metadata["image"] = json!({"status":"unavailable","reason":error.to_string()});
        }
    }
}
/// The idempotency hash view of an open. The generated hook env never enters it, so a replayed request_id hashes identically;
/// `claude_permissions` joins ONLY when declared, so every non-scoped open hashes as before.
fn open_payload_hash(
    identity: &ToolCallIdentity,
    request: &TerminalCreateRequestPayload,
    claude_permissions: Option<&ClaudePermissionsScope>,
) -> Result<String, RpcError> {
    let mut view = serde_json::to_value(request).map_err(failure)?;
    if let Some(env) = view.get_mut("env").and_then(Value::as_object_mut) {
        for key in crate::terminal_hooks::TERMINAL_HOOK_ENV_KEYS {
            env.remove(key);
        }
    }
    let mut view = json!({"actor":identity.to_actor_id(),"request":view,"planner_hooks":true});
    if let Some(scope) = claude_permissions {
        view[TERMINAL_CLAUDE_PERMISSIONS_PAYLOAD_KEY] =
            serde_json::to_value(scope).map_err(failure)?;
    }
    stable_payload_hash(&view).map_err(failure)
}
/// The search derives its own offset (so `scroll_offset` must be 0) and a wait that tests text returns the live viewport (so the two are exclusive).
fn scroll_to_request(
    text: Option<String>,
    occurrence: Option<String>,
    scroll_offset: usize,
    wait_tests_text: bool,
) -> Result<Option<ScrollTo>, RpcError> {
    let Some(pattern) = text else {
        if occurrence.is_some() {
            return Err(RpcError::invalid_params(
                "scroll_to_occurrence needs scroll_to_text",
            ));
        }
        return Ok(None);
    };
    let occurrence = match occurrence.as_deref() {
        None | Some("latest") => Occurrence::Latest,
        Some("earliest") => Occurrence::Earliest,
        Some(other) => {
            return Err(RpcError::invalid_params(format!(
                "scroll_to_occurrence must be latest or earliest, not {other}"
            )));
        }
    };
    let request = ScrollTo::new(pattern, occurrence)
        .map_err(|error| RpcError::invalid_params(error.to_string()))?;
    if scroll_offset > 0 {
        return Err(RpcError::invalid_params(
            "scroll_to_text needs scroll_offset 0",
        ));
    }
    if wait_tests_text {
        return Err(RpcError::invalid_params(
            "scroll_to_text and wait_for=text / text conditions are exclusive",
        ));
    }
    Ok(Some(request))
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
        "calm.terminal.resolve" => {
            let args: Resolve = parse(args)?;
            let resolved = service
                .resolve(&identity, &target(args.terminal_id, args.task_id)?)
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
        "calm.terminal.open" => {
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
            // The trimmed scope enters the hash.
            let claude_permissions = args
                .claude_permissions
                .as_ref()
                .map(|value| parse_scope(value).and_then(|scope| validate_scope(&scope)))
                .transpose()
                .map_err(RpcError::invalid_params)?;
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
            // Only a FRESH request_id is checked against the policy ceiling, so a replay with the same arguments never meets a since-narrowed policy;
            // the verdict is discarded — `prepare_tx` re-reads the ceiling inside the write transaction.
            if runtime
                .find_by_kind_and_idempotency("terminal-create", &idempotency_key)
                .await
                .map_err(failure)?
                .is_none()
            {
                let ceiling = ctx
                    .repo
                    .track_claude_permissions_ceiling(&track_id)
                    .await
                    .map_err(failure)?;
                apply_policy(ceiling.as_ref(), claude_permissions.as_ref())
                    .map_err(RpcError::invalid_params)?;
                wait_at_ceiling_checked_hook(&track_id).await;
            }
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
                payload_hash: open_payload_hash(&identity, &request, claude_permissions.as_ref())?,
            };
            let payload = serde_json::to_value(TerminalCreateOperationPayload {
                actor: identity.to_actor_id(),
                worker_session_id: Some(new_id()),
                planner_hooks: true,
                claude_permissions,
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
            // Establish the observation client before the Planner enters a TUI. With a wait this immediate read is text only and is the baseline of the final observation.
            let immediate = if waited {
                ObservationFormat::Text
            } else {
                args.format
            };
            let (mut metadata, mut png) = observe_for_open(
                service,
                &identity,
                &terminal.id,
                WaitPlan::default(),
                immediate,
            )
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
                            png = None;
                            if args.format == ObservationFormat::Image {
                                let image = service
                                    .observe(
                                        &identity,
                                        &target,
                                        0,
                                        WaitPlan::default(),
                                        args.format,
                                        None,
                                    )
                                    .await;
                                apply_image_outcome(&mut metadata, &mut png, image);
                            }
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
                (metadata, png) =
                    observe_for_open(service, &identity, &terminal.id, wait, args.format)
                        .await
                        .map_err(failure)?;
            }
            if let Some(claim) = claim {
                metadata["claim"] = claim;
            }
            metadata["card_id"] = json!(card.id);
            // Echoed from the stamped card (source of truth), so a replay echoes the same block; a source recomputed against the CURRENT policy could mislabel it.
            for key in [
                TERMINAL_CLAUDE_PERMISSIONS_PAYLOAD_KEY,
                TERMINAL_CLAUDE_PERMISSIONS_SOURCE_PAYLOAD_KEY,
            ] {
                if let Some(value) = card.payload.get(key) {
                    metadata[key] = value.clone();
                }
            }
            metadata["operation_id"] = json!(operation);
            observation_result(metadata, png)
        }
        "calm.terminal.observe" => {
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
            let scroll_to = scroll_to_request(
                args.scroll_to_text,
                args.scroll_to_occurrence,
                args.scroll_offset,
                wait.tests_text(),
            )?;
            let wait = wait.plan()?;
            let (metadata, png) = service
                .observe(
                    &identity,
                    &target(args.terminal_id, args.task_id)?,
                    args.scroll_offset,
                    wait,
                    args.format,
                    scroll_to,
                )
                .await
                .map_err(failure)?;
            observation_result(metadata, png)
        }
        "calm.terminal.control" => {
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
                    &target(args.terminal_id, args.task_id)?,
                    &args.action,
                    readback,
                )
                .await
                .map(|receipt| receipt_result(&args.action, receipt))
                .map_err(write_failure)
        }
        "calm.terminal.input" => {
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
            // The below-cursor tolerance admits draft edits only; the service refuses it again.
            if args.allow_output_below_cursor && !edits_the_draft(&args.action) {
                return Err(RpcError::invalid_params(BELOW_CURSOR_EDITS_ONLY));
            }
            service
                .input(
                    &identity,
                    &target(args.terminal_id, args.task_id)?,
                    args.observation_id,
                    &args.request_id,
                    args.action,
                    InputOptions {
                        allow_output_since_observation: args.allow_output_since_observation,
                        allow_output_below_cursor: args.allow_output_below_cursor,
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
