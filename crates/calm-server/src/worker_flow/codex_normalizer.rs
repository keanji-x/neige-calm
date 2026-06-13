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
    EventMsg(Value),
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
pub enum ResponseItem {
    Message {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        id: Option<String>,
        role: String,
        content: Vec<ContentItem>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        phase: Option<MessagePhase>,
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

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MessagePhase {
    Commentary,
    FinalAnswer,
}

impl MessagePhase {
    fn as_str(&self) -> &'static str {
        match self {
            MessagePhase::Commentary => "commentary",
            MessagePhase::FinalAnswer => "final_answer",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContentItem {
    InputText {
        text: String,
    },
    InputImage {
        image_url: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        detail: Option<String>,
    },
    OutputText {
        text: String,
    },
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
        RolloutItem::SessionMeta(_) | RolloutItem::TurnContext(_) | RolloutItem::EventMsg(_) => {
            None
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
            content: content.iter().map(message_block_from_content).collect(),
        }),
        ResponseItem::Message {
            role,
            content,
            phase,
            ..
        } if role == "assistant" => Some(WorkerFlowItem::AgentMessage {
            env,
            text: text_from_content(content),
            is_final: true,
            phase: phase.as_ref().map(|p| p.as_str().to_string()),
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
            changes: vec![FileEdit {
                path: file_change_path(input).unwrap_or_else(|| "<patch>".to_string()),
                kind: FileChangeKind::Update { move_path: None },
                diff: Some(input.clone()),
            }],
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

fn message_block_from_content(item: &ContentItem) -> MessageBlock {
    match item {
        ContentItem::InputText { text } | ContentItem::OutputText { text } => {
            MessageBlock::Text { text: text.clone() }
        }
        ContentItem::InputImage { image_url, .. } => MessageBlock::Image {
            url: Some(image_url.clone()),
            path: None,
        },
    }
}

fn text_from_content(content: &[ContentItem]) -> String {
    content
        .iter()
        .filter_map(|item| match item {
            ContentItem::InputText { text } | ContentItem::OutputText { text } => {
                Some(text.as_str())
            }
            ContentItem::InputImage { .. } => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn exec_status_from_local_shell(status: &LocalShellStatus) -> ExecStatus {
    match status {
        LocalShellStatus::Completed => ExecStatus::Completed,
        LocalShellStatus::InProgress => ExecStatus::InProgress,
        LocalShellStatus::Incomplete => ExecStatus::Failed,
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
    parse_json_or_string(arguments)
        .get("cwd")
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

fn file_change_path(input: &str) -> Option<String> {
    for prefix in [
        "*** Update File: ",
        "*** Add File: ",
        "*** Delete File: ",
        "--- ",
        "+++ ",
    ] {
        if let Some(line) = input.lines().find(|line| line.starts_with(prefix)) {
            return Some(line.trim_start_matches(prefix).trim().to_string());
        }
    }
    None
}

fn patch_status(status: Option<&str>) -> PatchStatus {
    match status {
        Some("completed" | "success" | "succeeded") => PatchStatus::Completed,
        Some("failed" | "error") => PatchStatus::Failed,
        Some("declined" | "rejected") => PatchStatus::Declined,
        _ => PatchStatus::InProgress,
    }
}
