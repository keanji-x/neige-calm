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
    BELOW_CURSOR_EDITS_ONLY, InputOptions, ObservationFormat, Target, TerminalInteraction, WaitFor,
    WaitPlan, edits_the_draft,
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
            json!({"request_id":{"type":"string","minLength":1,"maxLength":128},"title":{"type":"string","maxLength":200},"program":{"type":"string","minLength":1,"maxLength":4096},"format":{"type":"string","enum":["text","image"],"default":"text"},"claim":{"type":"boolean","default":false}}),
            vec!["request_id"],
        ),
        (
            "calm.terminal.observe",
            include_str!("../../../prompts/tools/calm.terminal.observe.md").trim_end(),
            json!({"terminal_id":{"type":"string"},"task_id":{"type":"string"},"scroll_offset":{"type":"integer","minimum":0,"maximum":2000},"wait_ms":{"type":"integer","minimum":0,"maximum":20000},"wait_for":{"type":"string","enum":["elapsed","change","signal","text"],"default":"elapsed"},"signal_events":{"type":"array","minItems":1,"items":{"type":"string"}},"wait_text":{"type":"array","minItems":1,"maxItems":8,"items":{"type":"string","minLength":1,"maxLength":200}},"settle_ms":{"type":"integer","minimum":0,"maximum":2000,"default":150},"repaint_ms":{"type":"integer","minimum":0,"maximum":5000},"format":{"type":"string","enum":["text","image"],"default":"text"}}),
            vec![],
        ),
        (
            "calm.terminal.control",
            include_str!("../../../prompts/tools/calm.terminal.control.md").trim_end(),
            json!({"terminal_id":{"type":"string"},"task_id":{"type":"string"},"action":{"type":"string","enum":["claim","release","detach"]},"observe":{"type":"boolean","default":false},"wait_ms":{"type":"integer","minimum":0,"maximum":20000},"wait_for":{"type":"string","enum":["elapsed","change","signal","text"],"default":"elapsed"},"signal_events":{"type":"array","minItems":1,"items":{"type":"string"}},"wait_text":{"type":"array","minItems":1,"maxItems":8,"items":{"type":"string","minLength":1,"maxLength":200}},"settle_ms":{"type":"integer","minimum":0,"maximum":2000,"default":150},"repaint_ms":{"type":"integer","minimum":0,"maximum":5000}}),
            vec!["action"],
        ),
        (
            "calm.terminal.input",
            include_str!("../../../prompts/tools/calm.terminal.input.md").trim_end(),
            json!({"terminal_id":{"type":"string"},"task_id":{"type":"string"},"observation_id":{"type":"string","format":"uuid"},"request_id":{"type":"string","minLength":1,"maxLength":128},"observe":{"type":"boolean","default":false},"wait_ms":{"type":"integer","minimum":0,"maximum":20000},"wait_for":{"type":"string","enum":["elapsed","change","signal","text"],"default":"elapsed"},"signal_events":{"type":"array","minItems":1,"items":{"type":"string"}},"wait_text":{"type":"array","minItems":1,"maxItems":8,"items":{"type":"string","minLength":1,"maxLength":200}},"settle_ms":{"type":"integer","minimum":0,"maximum":2000,"default":150},"repaint_ms":{"type":"integer","minimum":0,"maximum":5000},"allow_output_since_observation":{"type":"boolean","default":false},"allow_output_below_cursor":{"type":"boolean","default":false},"claim":{"type":"boolean","default":false},"release":{"type":"boolean","default":false},
            "action":{"anyOf":[
                {"type":"object","required":["type","text"],"additionalProperties":false,"properties":{"type":{"enum":["text","submit"]},"text":{"type":"string","minLength":1,"maxLength":16384}}},
                {"type":"object","required":["type","key"],"additionalProperties":false,"properties":{"type":{"const":"key"},"key":{"type":"string"},"repeat":{"type":"integer","minimum":1,"maximum":32,"default":1}}},
                {"type":"object","required":["type","column","row"],"additionalProperties":false,"properties":{"type":{"const":"click"},"column":{"type":"integer","minimum":0},"row":{"type":"integer","minimum":0}}},
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
        // #1666 — no `terminal_id`/`task_id` selector arms: duplicating every
        // root property into two closed arms tripled the input schema and
        // left no room under the 4000-byte compaction threshold. Exactly-one
        // targeting stays enforced server-side (`Target::from_ids`) and is the
        // first sentence of every description; the action arms stay closed.
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
    #[serde(default)]
    format: ObservationFormat,
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
    format!(
        "terminal {} observation {} revision {} {} {}x{} cursor {},{} wait {}; full state in structuredContent",
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
fn receipt_summary(action: &str, receipt: &Value) -> String {
    let terminal = receipt["terminal_id"].as_str().unwrap_or("null");
    let readback = match receipt["observation"]["status"].as_str() {
        Some(status) => format!(" readback {status}"),
        None => String::new(),
    };
    let facts = if receipt["detached"] == true {
        format!("detached had_client {}", receipt["had_client"])
    } else if let Some(outcome) = receipt["outcome"].as_str() {
        format!("input {outcome}")
    } else {
        format!(
            "{action} control_id {}",
            if receipt["control_id"].is_string() {
                "present"
            } else {
                "null"
            }
        )
    };
    format!("terminal {terminal} {facts}{readback}; details in structuredContent")
}
fn receipt_result(action: &str, receipt: Value) -> ToolResult {
    let summary = receipt_summary(action, &receipt);
    ToolResult::structured_with_summary(receipt, summary)
}
/// An open whose card operation did not succeed: no terminal id exists yet,
/// so the summary names the operation; the outcome detail stays in
/// structuredContent like every other terminal result.
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
}
impl WaitArgs {
    fn any(&self) -> bool {
        self.wait_ms.is_some()
            || self.wait_for.is_some()
            || self.settle_ms.is_some()
            || self.signal_events.is_some()
            || self.repaint_ms.is_some()
            || self.wait_text.is_some()
    }
    fn plan(self) -> Result<WaitPlan, RpcError> {
        WaitPlan::new(
            self.wait_for,
            self.wait_ms,
            self.settle_ms,
            self.signal_events,
            self.repaint_ms,
            self.wait_text,
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
            "wait_ms/wait_for/settle_ms/signal_events/repaint_ms/wait_text require observe=true; detach cannot observe",
        ));
    }
    if !observe {
        return Ok(None);
    }
    wait.plan().map(Some)
}
/// The observation an open returns. With `format=image` a failed render
/// falls back to the text observation plus `image: {status: unavailable,
/// reason}`; only a failed text observation is an error.
async fn observe_for_open(
    service: &TerminalInteraction,
    identity: &ToolCallIdentity,
    terminal_id: &str,
    format: ObservationFormat,
) -> anyhow::Result<(Value, Option<Vec<u8>>)> {
    let target = Target::Terminal(terminal_id.to_owned());
    let attempt = service
        .observe(identity, &target, 0, WaitPlan::default(), format)
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
                )
                .await?;
            apply_image_outcome(&mut metadata, &mut png, Err(error));
            Ok((metadata, png))
        }
        Err(error) => Err(error),
    }
}
/// Merge the outcome of an explicit image observation into an open result
/// that already carries a text observation (#1620 F6): success replaces the
/// state and PNG; failure keeps the text state and creation/claim facts and
/// reports `image: {status: unavailable, reason}` with no PNG.
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
/// #1620 — the idempotency hash view of an open request. The generated hook
/// env (`TERMINAL_HOOK_ENV_KEYS`) is derived by the adapter from the card id
/// it allocates and never enters this view, so a replayed request_id hashes
/// identically; the stored request and terminal row keep the complete env.
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
    stable_payload_hash(
        &json!({"actor":identity.to_actor_id(),"request":view,"planner_hooks":true}),
    )
    .map_err(failure)
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
            let track_id = TerminalInteraction::authorize(ctx.repo.as_ref(), &identity)
                .await
                .map_err(failure)?;
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
                idempotency_key: Some(format!(
                    "planner-terminal:{}:{}",
                    identity.session_id, args.request_id
                )),
                payload_hash: open_payload_hash(&identity, &request)?,
            };
            let payload = serde_json::to_value(TerminalCreateOperationPayload {
                actor: identity.to_actor_id(),
                worker_session_id: Some(new_id()),
                planner_hooks: true,
                request,
            })
            .map_err(failure)?;
            let runtime = ctx
                .operation_runtime
                .get()
                .ok_or_else(|| RpcError::internal("operation runtime unavailable"))?;
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
            // Establish the observation client before the Planner enters a TUI.
            // The created ids survive an image failure: the text observation
            // is returned with `image: unavailable` instead of an error.
            let (mut metadata, mut png) =
                observe_for_open(service, &identity, &terminal.id, args.format)
                    .await
                    .map_err(failure)?;
            if args.claim {
                // Same claim path as calm.terminal.control (claim-if-unowned);
                // the open already succeeded whatever the claim does.
                match service
                    .claim_after_open(
                        &identity,
                        &Target::Terminal(terminal.id.clone()),
                        WaitPlan::default(),
                    )
                    .await
                {
                    Ok(receipt) if receipt["observation"]["status"] == "available" => {
                        let claim = json!({"status":"claimed","control_id":receipt["control_id"]});
                        metadata = receipt["observation"]["state"].clone();
                        png = None;
                        if args.format == ObservationFormat::Image {
                            let image = service
                                .observe(
                                    &identity,
                                    &Target::Terminal(terminal.id.clone()),
                                    0,
                                    WaitPlan::default(),
                                    args.format,
                                )
                                .await;
                            apply_image_outcome(&mut metadata, &mut png, image);
                        }
                        metadata["claim"] = claim;
                    }
                    Ok(receipt) => {
                        metadata["claim"] = json!({"status":"unavailable","control_id":receipt["control_id"],
                            "reason":format!("claim readback unavailable: {}", receipt["observation"]["reason"].as_str().unwrap_or("unknown"))});
                    }
                    Err(error) => {
                        metadata["claim"] =
                            json!({"status":"unavailable","reason":error.to_string()});
                    }
                }
            }
            metadata["card_id"] = json!(card.id);
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
            if args.wait_for == Some(WaitFor::Text) && args.scroll_offset > 0 {
                return Err(RpcError::invalid_params(
                    "wait_for=text observes the live viewport; scroll_offset must be 0",
                ));
            }
            let wait = WaitArgs {
                wait_for: args.wait_for,
                wait_ms: args.wait_ms,
                settle_ms: args.settle_ms,
                signal_events: args.signal_events,
                repaint_ms: args.repaint_ms,
                wait_text: args.wait_text,
            }
            .plan()?;
            let (metadata, png) = service
                .observe(
                    &identity,
                    &target(args.terminal_id, args.task_id)?,
                    args.scroll_offset,
                    wait,
                    args.format,
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
                .map_err(failure)
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
                },
                false,
            )?;
            // #1666 r1 — the below-cursor tolerance admits draft edits only
            // (see `terminal_interaction::edits_the_draft`); the service
            // refuses it again, this is the invalid-params shape.
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
                .map_err(failure)
        }
        _ => Err(RpcError::invalid_params("unknown terminal tool")),
    }
}

#[cfg(test)]
mod schema_tests;
#[cfg(test)]
mod summary_tests {
    use super::*;

    /// #1620 F6 — an image failure after creation, claim and text readback
    /// keeps the text state, the claim and the ids, drops the PNG and reports
    /// the reason; an image success replaces state and PNG.
    #[test]
    fn open_image_failure_keeps_the_text_state_and_reports_image_unavailable() {
        let text = json!({"terminal_id":"t-1","text":["READY"],"role":"owner","control_id":"c-1"});
        let mut metadata = text.clone();
        let mut png = Some(vec![1, 2, 3]);
        apply_image_outcome(
            &mut metadata,
            &mut png,
            Err(anyhow::anyhow!("terminal image unavailable: zero geometry")),
        );
        assert!(png.is_none(), "no PNG on an image failure");
        assert_eq!(
            metadata["image"],
            json!({"status":"unavailable","reason":"terminal image unavailable: zero geometry"})
        );
        for key in ["terminal_id", "text", "role", "control_id"] {
            assert_eq!(metadata[key], text[key], "{key} must survive");
        }
        metadata["claim"] = json!({"status":"claimed","control_id":"c-1"});
        metadata["card_id"] = json!("card-1");
        let wire =
            serde_json::to_value(observation_result(metadata.clone(), png).unwrap()).unwrap();
        assert_eq!(wire["structuredContent"], metadata);
        assert_eq!(wire["content"].as_array().unwrap().len(), 1, "text only");
        assert_eq!(wire["content"][0]["type"], "text");

        let mut metadata = text.clone();
        let mut png = None;
        let rendered =
            json!({"terminal_id":"t-1","text":["READY"],"image_source":"rmux_client_projection"});
        apply_image_outcome(
            &mut metadata,
            &mut png,
            Ok((rendered.clone(), Some(vec![9]))),
        );
        assert_eq!(metadata, rendered);
        assert_eq!(png, Some(vec![9]));
    }

    #[test]
    fn terminal_open_failure_is_a_one_line_summary_with_structured_detail() {
        let receipt = json!({"operation_id":"op-7","outcome":"unavailable","detail":"Failed { error: \"spawn refused\" }"});
        let wire = serde_json::to_value(open_failure_result(receipt.clone())).unwrap();
        assert_eq!(wire["structuredContent"], receipt);
        let content = wire["content"].as_array().unwrap();
        assert_eq!(content.len(), 1);
        assert_eq!(
            content[0]["text"],
            "terminal open unavailable operation op-7; details in structuredContent"
        );
        assert!(
            !content[0]["text"]
                .as_str()
                .unwrap()
                .contains("spawn refused"),
            "the detail must live only in structuredContent"
        );
    }
}
