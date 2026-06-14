use calm_types::worker::{WorkerProviderKind, WorkerSessionId};
use calm_types::worker_flow::{
    CommandAction, ExecSource, ExecStatus, FileChangeKind, FileEdit, FlowEnvelope, MessageBlock,
    PatchStatus, RawRef, ToolCallId, ToolResultBlock, WorkerFlowItem,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RolloutLine {
    pub timestamp: String,
    #[serde(flatten)]
    pub item: RolloutItem,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", content = "payload", rename_all = "snake_case")]
pub enum RolloutItem {
    SessionMeta(SessionMetaLine),
    ResponseItem(ResponseItem),
    Compacted(CompactedItem),
    TurnContext(TurnContextItem),
    EventMsg(EventMsg),
    #[serde(other)]
    Other,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SessionMetaLine {
    pub id: String,
    #[serde(flatten)]
    pub extra: Value,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TurnContextItem {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn_id: Option<String>,
    #[serde(flatten)]
    pub extra: Value,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CompactedItem {
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub replacement_history: Option<Vec<ResponseItem>>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum EventMsg {
    ExecCommandBegin {},
    ExecCommandEnd(ExecCommandEndEvent),
    #[serde(other)]
    Other,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ExecCommandEndEvent {
    pub call_id: String,
    pub command: Vec<String>,
    pub cwd: std::path::PathBuf,
    #[serde(default)]
    pub parsed_cmd: Vec<ParsedCommand>,
    pub aggregated_output: String,
    pub exit_code: i32,
    #[serde(default)]
    pub duration: Option<Value>,
    pub status: ExecCommandStatus,
    #[serde(default)]
    pub source: ExecCommandSource,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ParsedCommand(Value);

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecCommandStatus {
    Completed,
    Failed,
    Declined,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecCommandSource {
    Agent,
    UserShell,
    UnifiedExecStartup,
    UnifiedExecInteraction,
    #[default]
    #[serde(other)]
    Unknown,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ResponseItem {
    Message {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        id: Option<String>,
        role: String,
        content: Vec<Value>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        phase: Option<String>,
    },
    AgentMessage {
        author: String,
        recipient: String,
        content: Vec<Value>,
    },
    Reasoning {
        #[serde(default)]
        id: String,
        summary: Vec<ReasoningSummary>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        content: Option<Vec<ReasoningContent>>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        encrypted_content: Option<String>,
    },
    LocalShellCall {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        id: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        call_id: Option<String>,
        status: LocalShellStatus,
        action: LocalShellAction,
    },
    FunctionCall {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        id: Option<String>,
        name: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        namespace: Option<String>,
        arguments: String,
        call_id: String,
    },
    FunctionCallOutput {
        call_id: String,
        output: FunctionCallOutputPayload,
    },
    CustomToolCall {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        id: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        status: Option<String>,
        call_id: String,
        name: String,
        input: String,
    },
    CustomToolCallOutput {
        call_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        name: Option<String>,
        output: FunctionCallOutputPayload,
    },
    WebSearchCall {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        id: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        status: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        action: Option<WebSearchAction>,
    },
    #[serde(other)]
    Other,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ReasoningSummary {
    SummaryText { text: String },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ReasoningContent {
    ReasoningText { text: String },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LocalShellStatus {
    Completed,
    InProgress,
    Incomplete,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum LocalShellAction {
    Exec(LocalShellExecAction),
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LocalShellExecAction {
    pub command: Vec<String>,
    pub timeout_ms: Option<u64>,
    pub working_directory: Option<String>,
    pub env: Option<std::collections::HashMap<String, String>>,
    pub user: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum WebSearchAction {
    Search {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        query: Option<String>,
        #[serde(flatten)]
        extra: Value,
    },
    #[serde(other)]
    Other,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum FunctionCallOutputPayload {
    Text(String),
    Items(Vec<FunctionCallOutputContentItem>),
    Json(Value),
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum FunctionCallOutputContentItem {
    InputText { text: String },
    InputImage { image_url: String },
    EncryptedContent { encrypted_content: String },
}

pub fn normalize_rollout_line(
    line: &RolloutLine,
    env_seq: u64,
    env_turn: u32,
    session_id: &WorkerSessionId,
    raw_ref: RawRef,
) -> Option<WorkerFlowItem> {
    let env = FlowEnvelope {
        seq: env_seq,
        turn: env_turn,
        session_id: session_id.clone(),
        provider: WorkerProviderKind::Codex,
        timestamp: parse_timestamp_ms(&line.timestamp),
        source_uuid: rollout_line_source_uuid(line),
        provider_extra: None,
        raw_ref: Some(raw_ref),
    };

    match &line.item {
        RolloutItem::SessionMeta(_) | RolloutItem::TurnContext(_) => None,
        RolloutItem::EventMsg(EventMsg::ExecCommandBegin { .. })
        | RolloutItem::EventMsg(EventMsg::Other) => None,
        RolloutItem::EventMsg(EventMsg::ExecCommandEnd(event)) => {
            let command = event.command.join(" ");
            Some(WorkerFlowItem::CommandExecution {
                env,
                call_id: Some(ToolCallId::from(event.call_id.clone())),
                command: command.clone(),
                cwd: Some(event.cwd.to_string_lossy().to_string()),
                parsed_actions: parsed_actions_from_parsed_cmd(&event.parsed_cmd, &command),
                aggregated_output: Some(event.aggregated_output.clone()),
                exit_code: Some(event.exit_code),
                duration_ms: event.duration.as_ref().and_then(duration_ms_from_value),
                status: exec_status_from_event(&event.status),
                source: exec_source_from_event(&event.source),
            })
        }
        RolloutItem::Compacted(item) => Some(WorkerFlowItem::Compaction {
            env,
            reason: Some("codex_compacted".to_string()),
            summary: Some(item.message.clone()),
        }),
        RolloutItem::ResponseItem(item) => normalize_response_item(item, env),
        RolloutItem::Other => Some(WorkerFlowItem::Unknown {
            env,
            raw_type: "rollout_item".to_string(),
        }),
    }
}

pub fn rollout_record_type(line: &RolloutLine) -> &'static str {
    match &line.item {
        RolloutItem::SessionMeta(_) => "session_meta",
        RolloutItem::ResponseItem(item) => response_item_type(item),
        RolloutItem::Compacted(_) => "compacted",
        RolloutItem::TurnContext(_) => "turn_context",
        RolloutItem::EventMsg(_) => "event_msg",
        RolloutItem::Other => "unknown",
    }
}

pub fn rollout_line_source_uuid(line: &RolloutLine) -> Option<String> {
    match &line.item {
        RolloutItem::ResponseItem(ResponseItem::Message { id, .. }) => id.clone(),
        RolloutItem::ResponseItem(ResponseItem::Reasoning { id, .. }) if !id.is_empty() => {
            Some(id.clone())
        }
        RolloutItem::ResponseItem(ResponseItem::LocalShellCall { call_id, id, .. }) => {
            call_id.clone().or_else(|| id.clone())
        }
        RolloutItem::ResponseItem(ResponseItem::FunctionCall { id, call_id, .. }) => {
            id.clone().or_else(|| Some(call_id.clone()))
        }
        RolloutItem::ResponseItem(ResponseItem::FunctionCallOutput { call_id, .. })
        | RolloutItem::ResponseItem(ResponseItem::CustomToolCallOutput { call_id, .. }) => {
            Some(call_id.clone())
        }
        RolloutItem::ResponseItem(ResponseItem::CustomToolCall { id, call_id, .. }) => {
            id.clone().or_else(|| Some(call_id.clone()))
        }
        RolloutItem::ResponseItem(ResponseItem::WebSearchCall { id, .. }) => id.clone(),
        RolloutItem::EventMsg(EventMsg::ExecCommandEnd(event)) => Some(event.call_id.clone()),
        _ => None,
    }
}

pub fn session_meta_id(line: &RolloutLine) -> Option<&str> {
    match &line.item {
        RolloutItem::SessionMeta(meta) => Some(meta.id.as_str()),
        _ => None,
    }
}

pub fn is_turn_context(line: &RolloutLine) -> bool {
    matches!(line.item, RolloutItem::TurnContext(_))
}

fn normalize_response_item(item: &ResponseItem, env: FlowEnvelope) -> Option<WorkerFlowItem> {
    match item {
        ResponseItem::Message {
            role,
            content,
            phase,
            ..
        } if role == "user" => Some(WorkerFlowItem::UserMessage {
            env,
            content: message_blocks_from_content(content),
        }),
        ResponseItem::Message {
            role,
            content,
            phase,
            ..
        } if role == "assistant" => Some(WorkerFlowItem::AgentMessage {
            env,
            text: text_from_content(content),
            is_final: is_final_message_phase(phase.as_deref()),
            phase: phase.clone(),
        }),
        ResponseItem::Message { role, .. } => Some(WorkerFlowItem::Unknown {
            env,
            raw_type: format!("message.{role}"),
        }),
        ResponseItem::AgentMessage { author, .. } => Some(WorkerFlowItem::Unknown {
            env,
            raw_type: format!("agent_message.{author}"),
        }),
        ResponseItem::Reasoning {
            summary,
            content,
            encrypted_content,
            ..
        } => Some(WorkerFlowItem::Reasoning {
            env,
            summary: summary
                .iter()
                .map(|ReasoningSummary::SummaryText { text }| text.clone())
                .collect(),
            content: content
                .as_deref()
                .unwrap_or_default()
                .iter()
                .map(|ReasoningContent::ReasoningText { text }| text.clone())
                .collect(),
            redacted: encrypted_content.is_some(),
        }),
        ResponseItem::LocalShellCall {
            call_id,
            status,
            action,
            ..
        } => {
            let LocalShellAction::Exec(exec) = action;
            let command = exec.command.join(" ");
            Some(WorkerFlowItem::CommandExecution {
                env,
                call_id: call_id.clone().map(ToolCallId::from),
                command: command.clone(),
                cwd: exec.working_directory.clone(),
                parsed_actions: parse_command_actions(&command),
                aggregated_output: None,
                exit_code: None,
                duration_ms: None,
                status: exec_status_from_local_shell(status),
                source: ExecSource::Agent,
            })
        }
        ResponseItem::FunctionCall {
            name,
            namespace,
            arguments,
            call_id,
            ..
        } if is_shell_function(name, namespace.as_deref()) => {
            let command = shell_command_from_arguments(arguments);
            Some(WorkerFlowItem::CommandExecution {
                env,
                call_id: Some(ToolCallId::from(call_id.clone())),
                command: command.clone(),
                cwd: cwd_from_arguments(arguments),
                parsed_actions: parse_command_actions(&command),
                aggregated_output: None,
                exit_code: None,
                duration_ms: None,
                status: ExecStatus::InProgress,
                source: ExecSource::Agent,
            })
        }
        ResponseItem::FunctionCall {
            name,
            arguments,
            call_id,
            ..
        } => Some(WorkerFlowItem::ToolCall {
            env,
            call_id: ToolCallId::from(call_id.clone()),
            name: name.clone(),
            input: parse_json_or_string(arguments),
            input_summary: Some(summary(arguments)),
        }),
        ResponseItem::FunctionCallOutput { call_id, output }
        | ResponseItem::CustomToolCallOutput {
            call_id, output, ..
        } => Some(WorkerFlowItem::ToolResult {
            env,
            call_id: ToolCallId::from(call_id.clone()),
            ok: true,
            output: tool_result_blocks(output),
            output_summary: Some(summary(&output_text(output))),
            error: None,
        }),
        ResponseItem::CustomToolCall {
            call_id,
            name,
            input,
            status,
            ..
        } if is_file_change(name, input) => Some(WorkerFlowItem::FileChange {
            env,
            call_id: Some(ToolCallId::from(call_id.clone())),
            changes: parse_apply_patch_or_fallback(input),
            status: patch_status(status.as_deref()),
        }),
        ResponseItem::CustomToolCall {
            call_id,
            name,
            input,
            ..
        } => Some(WorkerFlowItem::ToolCall {
            env,
            call_id: ToolCallId::from(call_id.clone()),
            name: name.clone(),
            input: Value::String(input.clone()),
            input_summary: Some(summary(input)),
        }),
        ResponseItem::WebSearchCall { id, action, .. } => Some(WorkerFlowItem::WebSearch {
            env,
            call_id: id.clone().map(ToolCallId::from),
            query: action.as_ref().and_then(|a| match a {
                WebSearchAction::Search { query, .. } => query.clone(),
                WebSearchAction::Other => None,
            }),
            results_summary: None,
        }),
        ResponseItem::Other => Some(WorkerFlowItem::Unknown {
            env,
            raw_type: "response_item".to_string(),
        }),
    }
}

fn response_item_type(item: &ResponseItem) -> &'static str {
    match item {
        ResponseItem::Message { .. } => "message",
        ResponseItem::AgentMessage { .. } => "agent_message",
        ResponseItem::Reasoning { .. } => "reasoning",
        ResponseItem::LocalShellCall { .. } => "local_shell_call",
        ResponseItem::FunctionCall { .. } => "function_call",
        ResponseItem::FunctionCallOutput { .. } => "function_call_output",
        ResponseItem::CustomToolCall { .. } => "custom_tool_call",
        ResponseItem::CustomToolCallOutput { .. } => "custom_tool_call_output",
        ResponseItem::WebSearchCall { .. } => "web_search_call",
        ResponseItem::Other => "response_item",
    }
}

fn parse_timestamp_ms(timestamp: &str) -> Option<i64> {
    chrono::DateTime::parse_from_rfc3339(timestamp)
        .ok()
        .map(|dt| dt.timestamp_millis())
}

const UNRECOGNIZED_MESSAGE_CONTENT: &str = "[message with unrecognized content]";

fn message_blocks_from_content(content: &[Value]) -> Vec<MessageBlock> {
    let mut blocks = content
        .iter()
        .map(message_block_from_content)
        .collect::<Vec<_>>();
    if blocks.is_empty() {
        blocks.push(MessageBlock::Text {
            text: UNRECOGNIZED_MESSAGE_CONTENT.to_string(),
        });
    }
    blocks
}

fn message_block_from_content(item: &Value) -> MessageBlock {
    let raw_type = content_type(item).unwrap_or("unknown");
    match raw_type {
        "input_text" | "output_text" | "text" => field_as_string(item, &["text"])
            .map(|text| MessageBlock::Text { text })
            .unwrap_or_else(|| unsupported_content_block(raw_type)),
        "input_image" | "image" | "image_url" => {
            let url = field_as_string(item, &["image_url", "url"]);
            let path = field_as_string(item, &["path"]);
            if url.is_some() || path.is_some() {
                MessageBlock::Image { url, path }
            } else {
                unsupported_content_block(raw_type)
            }
        }
        "file_ref" => field_as_string(item, &["path", "file_path"])
            .map(|path| MessageBlock::FileRef { path })
            .unwrap_or_else(|| unsupported_content_block(raw_type)),
        "mention" => field_as_string(item, &["name", "text"])
            .or_else(|| field_as_string(item, &["path"]))
            .map(|name| MessageBlock::Mention {
                name,
                path: field_as_string(item, &["path"]),
            })
            .unwrap_or_else(|| unsupported_content_block(raw_type)),
        _ => unsupported_content_block(raw_type),
    }
}

fn unsupported_content_block(raw_type: &str) -> MessageBlock {
    MessageBlock::Text {
        text: format!("[unsupported content block: {raw_type}]"),
    }
}

fn content_type(item: &Value) -> Option<&str> {
    item.get("type").and_then(Value::as_str)
}

fn field_as_string(item: &Value, keys: &[&str]) -> Option<String> {
    keys.iter()
        .find_map(|key| item.get(*key).and_then(Value::as_str).map(str::to_string))
}

fn text_from_content(content: &[Value]) -> String {
    let text = message_blocks_from_content(content)
        .into_iter()
        .filter_map(|block| match block {
            MessageBlock::Text { text } => Some(text),
            MessageBlock::Image { .. }
            | MessageBlock::FileRef { .. }
            | MessageBlock::Mention { .. } => None,
        })
        .collect::<Vec<_>>()
        .join("\n");
    if text.is_empty() {
        UNRECOGNIZED_MESSAGE_CONTENT.to_string()
    } else {
        text
    }
}

fn is_final_message_phase(phase: Option<&str>) -> bool {
    matches!(
        phase,
        Some("FinalAnswer") | Some("final_answer") | Some("final")
    )
}

fn exec_status_from_local_shell(status: &LocalShellStatus) -> ExecStatus {
    match status {
        LocalShellStatus::Completed => ExecStatus::Completed,
        LocalShellStatus::InProgress => ExecStatus::InProgress,
        LocalShellStatus::Incomplete => ExecStatus::Failed,
    }
}

fn exec_status_from_event(status: &ExecCommandStatus) -> ExecStatus {
    match status {
        ExecCommandStatus::Completed => ExecStatus::Completed,
        ExecCommandStatus::Failed => ExecStatus::Failed,
        ExecCommandStatus::Declined => ExecStatus::Declined,
    }
}

fn exec_source_from_event(source: &ExecCommandSource) -> ExecSource {
    match source {
        ExecCommandSource::Agent => ExecSource::Agent,
        ExecCommandSource::UserShell => ExecSource::UserShell,
        ExecCommandSource::UnifiedExecStartup
        | ExecCommandSource::UnifiedExecInteraction
        | ExecCommandSource::Unknown => ExecSource::Unknown,
    }
}

fn is_shell_function(name: &str, namespace: Option<&str>) -> bool {
    let lower = name.to_ascii_lowercase();
    let namespace = namespace.unwrap_or_default().to_ascii_lowercase();
    matches!(
        lower.as_str(),
        "shell" | "bash" | "exec" | "exec_command" | "local_shell" | "container.exec"
    ) || namespace == "shell"
}

fn parse_json_or_string(s: &str) -> Value {
    serde_json::from_str(s).unwrap_or_else(|_| Value::String(s.to_string()))
}

fn shell_command_from_arguments(arguments: &str) -> String {
    let value = parse_json_or_string(arguments);
    if let Some(cmd) = value.get("cmd").and_then(Value::as_array) {
        return cmd
            .iter()
            .filter_map(Value::as_str)
            .collect::<Vec<_>>()
            .join(" ");
    }
    if let Some(cmd) = value.get("cmd").and_then(Value::as_str) {
        return cmd.to_string();
    }
    if let Some(cmd) = value.get("argv").and_then(Value::as_array) {
        return cmd
            .iter()
            .filter_map(Value::as_str)
            .collect::<Vec<_>>()
            .join(" ");
    }
    value
        .get("command")
        .and_then(Value::as_str)
        .unwrap_or(arguments)
        .to_string()
}

fn cwd_from_arguments(arguments: &str) -> Option<String> {
    let value = parse_json_or_string(arguments);
    value
        .get("cwd")
        .or_else(|| value.get("workdir"))
        .and_then(Value::as_str)
        .map(str::to_string)
}

fn parse_command_actions(command: &str) -> Vec<CommandAction> {
    let trimmed = command.trim();
    let mut parts = trimmed.split_whitespace();
    let head = parts.next().unwrap_or_default();
    match head {
        "ls" | "find" => vec![CommandAction::ListFiles {
            command: trimmed.to_string(),
            path: parts.next().map(str::to_string),
        }],
        "rg" | "grep" => vec![CommandAction::Search {
            command: trimmed.to_string(),
            query: parts.next().map(str::to_string),
            path: parts.next().map(str::to_string),
        }],
        "cat" | "sed" | "nl" | "head" | "tail" => vec![CommandAction::Read {
            command: trimmed.to_string(),
            name: head.to_string(),
            path: parts.last().unwrap_or_default().to_string(),
        }],
        _ if trimmed.is_empty() => vec![],
        _ => vec![CommandAction::Unknown {
            command: trimmed.to_string(),
        }],
    }
}

fn parsed_actions_from_parsed_cmd(
    parsed_cmd: &[ParsedCommand],
    fallback_command: &str,
) -> Vec<CommandAction> {
    for parsed in parsed_cmd {
        if let Some(command) = parsed
            .0
            .get("command")
            .or_else(|| parsed.0.get("cmd"))
            .and_then(Value::as_str)
        {
            return parse_command_actions(command);
        }
    }

    // TODO(#704): map Codex ParsedCommand variants directly once that schema is
    // imported; the raw command parser is the durable fallback for now.
    parse_command_actions(fallback_command)
}

fn duration_ms_from_value(value: &Value) -> Option<i64> {
    if let Some(ms) = value.as_i64() {
        return Some(ms);
    }
    if let Some(ms) = value.as_f64() {
        return Some(ms.round() as i64);
    }
    if let Some(text) = value.as_str() {
        return text.parse::<i64>().ok();
    }

    let object = value.as_object()?;
    if let Some(ms) = object
        .get("millis")
        .or_else(|| object.get("milliseconds"))
        .and_then(Value::as_i64)
    {
        return Some(ms);
    }

    let secs = object.get("secs").and_then(Value::as_i64).unwrap_or(0);
    let nanos = object.get("nanos").and_then(Value::as_i64).unwrap_or(0);
    Some(secs.saturating_mul(1_000) + nanos / 1_000_000)
}

fn tool_result_blocks(output: &FunctionCallOutputPayload) -> Vec<ToolResultBlock> {
    match output {
        FunctionCallOutputPayload::Text(text) => vec![ToolResultBlock::Text { text: text.clone() }],
        FunctionCallOutputPayload::Items(items) => items
            .iter()
            .map(|item| match item {
                FunctionCallOutputContentItem::InputText { text } => {
                    ToolResultBlock::Text { text: text.clone() }
                }
                FunctionCallOutputContentItem::InputImage { image_url } => ToolResultBlock::Image {
                    url: Some(image_url.clone()),
                },
                FunctionCallOutputContentItem::EncryptedContent { .. } => ToolResultBlock::Text {
                    text: "[encrypted content]".to_string(),
                },
            })
            .collect(),
        FunctionCallOutputPayload::Json(value) => vec![ToolResultBlock::Text {
            text: value.to_string(),
        }],
    }
}

fn output_text(output: &FunctionCallOutputPayload) -> String {
    tool_result_blocks(output)
        .into_iter()
        .filter_map(|block| match block {
            ToolResultBlock::Text { text } => Some(text),
            ToolResultBlock::Image { url } => url,
            ToolResultBlock::FileRef { path } => Some(path),
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn summary(text: &str) -> String {
    const LIMIT: usize = 240;
    let mut out = text.chars().take(LIMIT).collect::<String>();
    if text.chars().count() > LIMIT {
        out.push_str("...");
    }
    out
}

fn is_file_change(name: &str, input: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    lower.contains("patch")
        || lower.contains("edit")
        || input.contains("*** Begin Patch")
        || input.contains("*** Update File:")
        || input.contains("*** Add File:")
        || input.contains("*** Delete File:")
}

fn parse_apply_patch_or_fallback(input: &str) -> Vec<FileEdit> {
    match parse_apply_patch(input) {
        Ok(changes) => changes,
        Err(error) => {
            tracing::warn!(
                error,
                "failed to parse codex apply_patch input; falling back to single file edit"
            );
            vec![FileEdit {
                path: "<patch>".to_string(),
                kind: FileChangeKind::Update { move_path: None },
                diff: Some(input.to_string()),
            }]
        }
    }
}

fn parse_apply_patch(input: &str) -> Result<Vec<FileEdit>, &'static str> {
    let mut saw_begin = false;
    let mut saw_end = false;
    let mut current: Option<ApplyPatchSection> = None;
    let mut changes = Vec::new();

    for line in input.lines() {
        if !saw_begin {
            if line == "*** Begin Patch" {
                saw_begin = true;
            } else if !line.trim().is_empty() {
                return Err("missing Begin Patch header");
            }
            continue;
        }

        if line == "*** End Patch" {
            finish_apply_patch_section(&mut changes, current.take());
            saw_end = true;
            break;
        }

        if line == "*** End of File" {
            let Some(section) = current.as_mut() else {
                return Err("End of File marker without active section");
            };
            section.diff_lines.push(line.to_string());
            continue;
        }

        if let Some(path) = line.strip_prefix("*** Add File: ") {
            finish_apply_patch_section(&mut changes, current.take());
            current = Some(ApplyPatchSection::new(
                path,
                ApplyPatchSectionKind::Add,
                line,
            )?);
            continue;
        }

        if let Some(path) = line.strip_prefix("*** Delete File: ") {
            finish_apply_patch_section(&mut changes, current.take());
            current = Some(ApplyPatchSection::new(
                path,
                ApplyPatchSectionKind::Delete,
                line,
            )?);
            continue;
        }

        if let Some(path) = line.strip_prefix("*** Update File: ") {
            finish_apply_patch_section(&mut changes, current.take());
            current = Some(ApplyPatchSection::new(
                path,
                ApplyPatchSectionKind::Update,
                line,
            )?);
            continue;
        }

        if let Some(move_path) = line.strip_prefix("*** Move to: ") {
            let Some(section) = current.as_mut() else {
                return Err("Move to header without active section");
            };
            if section.kind != ApplyPatchSectionKind::Update {
                return Err("Move to header outside update section");
            }
            let move_path = move_path.trim();
            if move_path.is_empty() {
                return Err("empty Move to path");
            }
            section.move_path = Some(move_path.to_string());
            section.diff_lines.push(line.to_string());
            continue;
        }

        if line.starts_with("*** ") {
            return Err("malformed patch header");
        }

        let Some(section) = current.as_mut() else {
            if line.trim().is_empty() {
                continue;
            }
            return Err("patch content before file section");
        };
        section.diff_lines.push(line.to_string());
    }

    if !saw_begin {
        return Err("missing Begin Patch header");
    }
    if !saw_end {
        return Err("missing End Patch header");
    }
    if changes.is_empty() {
        return Err("patch contained no file sections");
    }
    Ok(changes)
}

#[derive(Debug, PartialEq, Eq)]
enum ApplyPatchSectionKind {
    Add,
    Delete,
    Update,
}

struct ApplyPatchSection {
    path: String,
    kind: ApplyPatchSectionKind,
    move_path: Option<String>,
    diff_lines: Vec<String>,
}

impl ApplyPatchSection {
    fn new(path: &str, kind: ApplyPatchSectionKind, header: &str) -> Result<Self, &'static str> {
        let path = path.trim();
        if path.is_empty() {
            return Err("empty file path");
        }
        Ok(Self {
            path: path.to_string(),
            kind,
            move_path: None,
            diff_lines: vec![header.to_string()],
        })
    }
}

fn finish_apply_patch_section(changes: &mut Vec<FileEdit>, section: Option<ApplyPatchSection>) {
    let Some(section) = section else {
        return;
    };
    let (kind, diff) = match section.kind {
        ApplyPatchSectionKind::Add => (FileChangeKind::Add, Some(section.diff_lines.join("\n"))),
        ApplyPatchSectionKind::Delete => (FileChangeKind::Delete, None),
        ApplyPatchSectionKind::Update => (
            FileChangeKind::Update {
                move_path: section.move_path,
            },
            Some(section.diff_lines.join("\n")),
        ),
    };
    changes.push(FileEdit {
        path: section.path,
        kind,
        diff,
    });
}

fn patch_status(status: Option<&str>) -> PatchStatus {
    match status {
        Some("completed" | "success" | "succeeded") => PatchStatus::Completed,
        Some("failed" | "error") => PatchStatus::Failed,
        Some("declined" | "rejected") => PatchStatus::Declined,
        _ => PatchStatus::InProgress,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn normalize_assistant_message(phase: Option<&str>) -> WorkerFlowItem {
        let mut payload = json!({
            "timestamp": "2026-06-13T00:00:00Z",
            "type": "response_item",
            "payload": {
                "type": "message",
                "id": "msg-assistant",
                "role": "assistant",
                "content": [{ "type": "output_text", "text": "done" }]
            }
        });
        if let Some(phase) = phase {
            payload["payload"]["phase"] = json!(phase);
        }
        let line: RolloutLine = serde_json::from_value(payload).unwrap();
        normalize_rollout_line(
            &line,
            0,
            0,
            &WorkerSessionId::from("sess"),
            RawRef {
                provider: WorkerProviderKind::Codex,
                source_path: Some("/tmp/rollout.jsonl".to_string()),
                line: Some(0),
                record_type: Some("message".to_string()),
            },
        )
        .unwrap()
    }

    fn assert_agent_phase(
        item: WorkerFlowItem,
        expected_final: bool,
        expected_phase: Option<&str>,
    ) {
        let WorkerFlowItem::AgentMessage {
            is_final, phase, ..
        } = item
        else {
            panic!("expected agent message");
        };
        assert_eq!(is_final, expected_final);
        assert_eq!(phase.as_deref(), expected_phase);
    }

    #[test]
    fn shell_arguments_accept_cmd_array_string_and_legacy_command() {
        let array = serde_json::to_string(&json!({
            "cmd": ["bash", "-lc", "echo hi"],
            "workdir": "/tmp"
        }))
        .unwrap();
        assert_eq!(shell_command_from_arguments(&array), "bash -lc echo hi");
        assert_eq!(cwd_from_arguments(&array), Some("/tmp".to_string()));

        let string = serde_json::to_string(&json!({
            "cmd": "pwd",
            "workdir": "/tmp"
        }))
        .unwrap();
        assert_eq!(shell_command_from_arguments(&string), "pwd");
        assert_eq!(cwd_from_arguments(&string), Some("/tmp".to_string()));

        let legacy = serde_json::to_string(&json!({
            "command": "uname -a"
        }))
        .unwrap();
        assert_eq!(shell_command_from_arguments(&legacy), "uname -a");
    }

    #[test]
    fn assistant_final_answer_phase_marks_final() {
        assert_agent_phase(
            normalize_assistant_message(Some("FinalAnswer")),
            true,
            Some("FinalAnswer"),
        );
    }

    #[test]
    fn assistant_commentary_phase_is_not_final() {
        assert_agent_phase(
            normalize_assistant_message(Some("Commentary")),
            false,
            Some("Commentary"),
        );
    }

    #[test]
    fn assistant_missing_phase_is_not_final() {
        assert_agent_phase(normalize_assistant_message(None), false, None);
    }
}
