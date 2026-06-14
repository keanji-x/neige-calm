use std::collections::HashMap;

use calm_types::worker::{WorkerProviderKind, WorkerSessionId};
use calm_types::worker_flow::{
    CommandAction, ExecSource, ExecStatus, FileChangeKind, FileEdit, FlowEnvelope, McpStatus,
    MessageBlock, PatchStatus, RawRef, ToolCallId, ToolError, ToolResultBlock, WorkerFlowItem,
};
use serde_json::Value;

#[derive(Default)]
pub struct ClaudeNormalizerState {
    pending_commands: HashMap<String, PendingCommand>,
    pending_file_changes: HashMap<String, PendingFileChange>,
    pending_web_searches: HashMap<String, PendingWebSearch>,
    pending_mcp_calls: HashMap<String, PendingMcpCall>,
}

impl ClaudeNormalizerState {
    pub fn pending_commands_len(&self) -> usize {
        self.pending_commands.len()
    }

    pub fn pending_mcp_calls_len(&self) -> usize {
        self.pending_mcp_calls.len()
    }
}

#[derive(Clone)]
struct PendingCommand {
    command: String,
    cwd: Option<String>,
    parsed_actions: Vec<CommandAction>,
}

#[derive(Clone)]
struct PendingFileChange {
    changes: Vec<FileEdit>,
}

#[derive(Clone)]
struct PendingWebSearch {
    query: Option<String>,
}

#[derive(Clone)]
struct PendingMcpCall {
    server: Option<String>,
    tool: String,
    arguments: Value,
}

pub fn normalize_record(
    record: &Value,
    seq: u64,
    turn: u32,
    session_id: &WorkerSessionId,
    raw_ref: RawRef,
) -> Option<WorkerFlowItem> {
    normalize_record_items(record, seq, turn, session_id, raw_ref)
        .into_iter()
        .next()
}

pub fn normalize_record_items(
    record: &Value,
    seq: u64,
    turn: u32,
    session_id: &WorkerSessionId,
    raw_ref: RawRef,
) -> Vec<WorkerFlowItem> {
    let mut state = ClaudeNormalizerState::default();
    normalize_record_with_state(record, seq, turn, session_id, raw_ref, &mut state)
}

pub fn normalize_record_with_state(
    record: &Value,
    seq: u64,
    turn: u32,
    session_id: &WorkerSessionId,
    raw_ref: RawRef,
    state: &mut ClaudeNormalizerState,
) -> Vec<WorkerFlowItem> {
    let record_type = record
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    let mut out = Vec::new();
    let mut next_seq = seq;
    match record_type {
        "assistant" => {
            let Some(content) = record
                .get("message")
                .and_then(|message| message.get("content"))
                .and_then(Value::as_array)
            else {
                out.push(unknown_item(record, next_seq, turn, session_id, raw_ref));
                return out;
            };
            for block in content {
                let env = env_for(record, next_seq, turn, session_id, raw_ref.clone());
                let Some(item) = assistant_block_item(block, env, state) else {
                    continue;
                };
                out.push(item);
                next_seq = next_seq.saturating_add(1);
            }
        }
        "user" => {
            let Some(content) = record
                .get("message")
                .and_then(|message| message.get("content"))
            else {
                out.push(unknown_item(record, next_seq, turn, session_id, raw_ref));
                return out;
            };
            match content {
                Value::String(text) => {
                    out.push(WorkerFlowItem::UserMessage {
                        env: env_for(record, next_seq, turn, session_id, raw_ref),
                        content: vec![MessageBlock::Text { text: text.clone() }],
                    });
                }
                Value::Array(blocks) => {
                    let mut message_blocks = Vec::new();
                    for block in blocks {
                        if content_type(block) == Some("tool_result") {
                            let call_id = string_field(block, &["tool_use_id", "id"]);
                            let has_pending_mcp = call_id
                                .as_ref()
                                .map(|call_id| state.pending_mcp_calls.contains_key(call_id))
                                .unwrap_or(false);
                            if has_pending_mcp
                                && let Some(item) = tool_completion_item(
                                    block,
                                    record,
                                    next_seq,
                                    turn,
                                    session_id,
                                    raw_ref.clone(),
                                    state,
                                )
                            {
                                out.push(item);
                                next_seq = next_seq.saturating_add(1);
                                continue;
                            }
                            let env = env_for(record, next_seq, turn, session_id, raw_ref.clone());
                            out.push(tool_result_item(block, env));
                            next_seq = next_seq.saturating_add(1);
                            if let Some(item) = tool_completion_item(
                                block,
                                record,
                                next_seq,
                                turn,
                                session_id,
                                raw_ref.clone(),
                                state,
                            ) {
                                out.push(item);
                                next_seq = next_seq.saturating_add(1);
                            }
                        } else {
                            message_blocks.push(message_block_from_content(block));
                        }
                    }
                    if !message_blocks.is_empty() {
                        out.push(WorkerFlowItem::UserMessage {
                            env: env_for(record, next_seq, turn, session_id, raw_ref),
                            content: message_blocks,
                        });
                    }
                }
                _ => {
                    out.push(WorkerFlowItem::UserMessage {
                        env: env_for(record, next_seq, turn, session_id, raw_ref),
                        content: vec![unsupported_content_block("unknown")],
                    });
                }
            }
        }
        _ => {
            out.push(unknown_item(record, next_seq, turn, session_id, raw_ref));
        }
    }
    out
}

pub fn record_type(record: &Value) -> String {
    raw_type_for_record(record)
}

pub fn source_uuid(record: &Value) -> Option<String> {
    record
        .get("uuid")
        .and_then(Value::as_str)
        .map(str::to_string)
}

pub fn record_cwd(record: &Value) -> Option<&str> {
    record
        .get("cwd")
        .and_then(Value::as_str)
        .or_else(|| {
            record
                .get("message")
                .and_then(|message| message.get("cwd"))
                .and_then(Value::as_str)
        })
        .or_else(|| {
            record
                .get("message")
                .and_then(|message| message.get("metadata"))
                .and_then(|metadata| metadata.get("cwd"))
                .and_then(Value::as_str)
        })
        .or_else(|| {
            record
                .get("hook")
                .and_then(|hook| hook.get("cwd"))
                .and_then(Value::as_str)
        })
        .or_else(|| {
            record
                .get("payload")
                .and_then(|payload| payload.get("cwd"))
                .and_then(Value::as_str)
        })
}

pub fn record_starts_turn(record: &Value) -> bool {
    if record.get("type").and_then(Value::as_str) != Some("user") {
        return false;
    }
    let Some(content) = record
        .get("message")
        .and_then(|message| message.get("content"))
    else {
        return false;
    };
    match content {
        Value::String(text) => !text.trim().is_empty(),
        Value::Array(blocks) => blocks
            .iter()
            .any(|block| content_type(block) != Some("tool_result")),
        _ => false,
    }
}

fn assistant_block_item(
    block: &Value,
    env: FlowEnvelope,
    state: &mut ClaudeNormalizerState,
) -> Option<WorkerFlowItem> {
    match content_type(block).unwrap_or("unknown") {
        "thinking" => {
            let text = string_field(block, &["thinking", "text", "content"])
                .unwrap_or_else(|| "[unsupported content block: thinking]".to_string());
            Some(WorkerFlowItem::Reasoning {
                env,
                summary: vec![summary(&text)],
                content: vec![text],
                redacted: block.get("signature").is_some()
                    || block.get("encrypted_content").is_some()
                    || block.get("encryptedContent").is_some(),
            })
        }
        "text" => Some(WorkerFlowItem::AgentMessage {
            env,
            text: string_field(block, &["text"])
                .unwrap_or_else(|| "[unsupported content block: text]".to_string()),
            is_final: false,
            phase: None,
        }),
        "tool_use" => tool_use_item(block, env, state),
        _ => Some(WorkerFlowItem::Unknown {
            env,
            raw_type: format!("assistant/{}", content_type(block).unwrap_or("unknown")),
        }),
    }
}

fn tool_use_item(
    block: &Value,
    env: FlowEnvelope,
    state: &mut ClaudeNormalizerState,
) -> Option<WorkerFlowItem> {
    let call_id = string_field(block, &["id", "tool_use_id"])?;
    let name = string_field(block, &["name"]).unwrap_or_else(|| "unknown".to_string());
    let input = block.get("input").cloned().unwrap_or(Value::Null);
    match name.as_str() {
        "Bash" => {
            let command = input
                .get("command")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            let cwd = input
                .get("cwd")
                .or_else(|| input.get("workdir"))
                .and_then(Value::as_str)
                .map(str::to_string);
            let parsed_actions = parse_command_actions(&command);
            state.pending_commands.insert(
                call_id.clone(),
                PendingCommand {
                    command: command.clone(),
                    cwd: cwd.clone(),
                    parsed_actions: parsed_actions.clone(),
                },
            );
            Some(WorkerFlowItem::CommandExecution {
                env,
                call_id: Some(ToolCallId::from(call_id)),
                command,
                cwd,
                parsed_actions,
                aggregated_output: None,
                exit_code: None,
                duration_ms: None,
                status: ExecStatus::InProgress,
                source: ExecSource::Agent,
            })
        }
        "Edit" | "Write" | "MultiEdit" => {
            let changes = vec![file_edit_from_tool_input(&name, &input)];
            state.pending_file_changes.insert(
                call_id.clone(),
                PendingFileChange {
                    changes: changes.clone(),
                },
            );
            Some(WorkerFlowItem::FileChange {
                env,
                call_id: Some(ToolCallId::from(call_id)),
                changes,
                status: PatchStatus::InProgress,
            })
        }
        "WebSearch" => {
            let query = input
                .get("query")
                .and_then(Value::as_str)
                .map(str::to_string);
            state.pending_web_searches.insert(
                call_id.clone(),
                PendingWebSearch {
                    query: query.clone(),
                },
            );
            Some(WorkerFlowItem::WebSearch {
                env,
                call_id: Some(ToolCallId::from(call_id)),
                query,
                results_summary: None,
            })
        }
        "Read" | "Grep" | "Glob" => Some(WorkerFlowItem::ToolCall {
            env,
            call_id: ToolCallId::from(call_id),
            name,
            input: input.clone(),
            input_summary: Some(summary(&input.to_string())),
        }),
        _ if name.starts_with("mcp__") => {
            let (server, tool) = parse_mcp_tool_name(&name);
            state.pending_mcp_calls.insert(
                call_id.clone(),
                PendingMcpCall {
                    server: server.clone(),
                    tool: tool.clone(),
                    arguments: input.clone(),
                },
            );
            Some(WorkerFlowItem::McpToolCall {
                env,
                call_id: ToolCallId::from(call_id),
                server,
                tool,
                arguments: input,
                status: McpStatus::InProgress,
                result: None,
                error: None,
                duration_ms: None,
            })
        }
        _ => Some(WorkerFlowItem::ToolCall {
            env,
            call_id: ToolCallId::from(call_id),
            name,
            input: input.clone(),
            input_summary: Some(summary(&input.to_string())),
        }),
    }
}

fn file_edit_from_tool_input(name: &str, input: &Value) -> FileEdit {
    let path = input
        .get("file_path")
        .or_else(|| input.get("path"))
        .and_then(Value::as_str)
        .unwrap_or("<unknown>")
        .to_string();
    let diff = match name {
        "Edit" => Some(format!(
            "old_string: {}\nnew_string: {}",
            input
                .get("old_string")
                .and_then(Value::as_str)
                .unwrap_or_default(),
            input
                .get("new_string")
                .and_then(Value::as_str)
                .unwrap_or_default()
        )),
        "Write" => input
            .get("content")
            .and_then(Value::as_str)
            .map(str::to_string),
        "MultiEdit" => input.get("edits").map(Value::to_string),
        _ => None,
    };
    FileEdit {
        path,
        kind: FileChangeKind::Update { move_path: None },
        diff,
    }
}

fn tool_result_item(block: &Value, env: FlowEnvelope) -> WorkerFlowItem {
    let call_id = string_field(block, &["tool_use_id", "id"]).unwrap_or_default();
    let ok = !tool_result_is_error(block);
    let output = tool_result_blocks(block.get("content").unwrap_or(&Value::Null));
    let output_text = output_text(&output);
    WorkerFlowItem::ToolResult {
        env,
        call_id: ToolCallId::from(call_id),
        ok,
        output,
        output_summary: Some(summary(&output_text)),
        error: if ok {
            None
        } else {
            Some(ToolError {
                message: summary(&output_text),
                kind: None,
            })
        },
    }
}

fn tool_completion_item(
    block: &Value,
    record: &Value,
    seq: u64,
    turn: u32,
    session_id: &WorkerSessionId,
    raw_ref: RawRef,
    state: &mut ClaudeNormalizerState,
) -> Option<WorkerFlowItem> {
    let call_id = string_field(block, &["tool_use_id", "id"])?;
    if state.pending_commands.contains_key(&call_id) {
        return command_completion_item(block, record, seq, turn, session_id, raw_ref, state);
    }
    if state.pending_file_changes.contains_key(&call_id) {
        return file_change_completion_item(block, record, seq, turn, session_id, raw_ref, state);
    }
    if state.pending_web_searches.contains_key(&call_id) {
        return web_search_completion_item(block, record, seq, turn, session_id, raw_ref, state);
    }
    if state.pending_mcp_calls.contains_key(&call_id) {
        return mcp_tool_completion_item(block, record, seq, turn, session_id, raw_ref, state);
    }
    None
}

fn command_completion_item(
    block: &Value,
    record: &Value,
    seq: u64,
    turn: u32,
    session_id: &WorkerSessionId,
    raw_ref: RawRef,
    state: &mut ClaudeNormalizerState,
) -> Option<WorkerFlowItem> {
    let call_id = string_field(block, &["tool_use_id", "id"])?;
    let result = build_bash_completion(record, block)?;
    let pending = state.pending_commands.remove(&call_id)?;
    let status = if tool_result_is_error(block) {
        ExecStatus::Failed
    } else {
        ExecStatus::Completed
    };
    Some(WorkerFlowItem::CommandExecution {
        env: env_for(record, seq, turn, session_id, raw_ref),
        call_id: Some(ToolCallId::from(call_id)),
        command: pending.command,
        cwd: pending.cwd,
        parsed_actions: pending.parsed_actions,
        aggregated_output: Some(result.aggregated_output),
        exit_code: Some(result.exit_code),
        duration_ms: None,
        status,
        source: ExecSource::Agent,
    })
}

fn mcp_tool_completion_item(
    block: &Value,
    record: &Value,
    seq: u64,
    turn: u32,
    session_id: &WorkerSessionId,
    raw_ref: RawRef,
    state: &mut ClaudeNormalizerState,
) -> Option<WorkerFlowItem> {
    let call_id = string_field(block, &["tool_use_id", "id"])?;
    let pending = state.pending_mcp_calls.remove(&call_id)?;
    let is_error = tool_result_is_error(block);
    let result = block.get("content").cloned().unwrap_or(Value::Null);
    let output = tool_result_blocks(&result);
    let output_text = output_text(&output);
    Some(WorkerFlowItem::McpToolCall {
        env: env_for(record, seq, turn, session_id, raw_ref),
        call_id: ToolCallId::from(call_id),
        server: pending.server,
        tool: pending.tool,
        arguments: pending.arguments,
        status: if is_error {
            McpStatus::Failed
        } else {
            McpStatus::Completed
        },
        result: Some(result),
        error: if is_error {
            Some(ToolError {
                message: summary(&output_text),
                kind: None,
            })
        } else {
            None
        },
        duration_ms: None,
    })
}

fn file_change_completion_item(
    block: &Value,
    record: &Value,
    seq: u64,
    turn: u32,
    session_id: &WorkerSessionId,
    raw_ref: RawRef,
    state: &mut ClaudeNormalizerState,
) -> Option<WorkerFlowItem> {
    let call_id = string_field(block, &["tool_use_id", "id"])?;
    let pending = state.pending_file_changes.remove(&call_id)?;
    let status = if tool_result_is_error(block) {
        PatchStatus::Failed
    } else {
        PatchStatus::Completed
    };
    Some(WorkerFlowItem::FileChange {
        env: env_for(record, seq, turn, session_id, raw_ref),
        call_id: Some(ToolCallId::from(call_id)),
        changes: pending.changes,
        status,
    })
}

fn web_search_completion_item(
    block: &Value,
    record: &Value,
    seq: u64,
    turn: u32,
    session_id: &WorkerSessionId,
    raw_ref: RawRef,
    state: &mut ClaudeNormalizerState,
) -> Option<WorkerFlowItem> {
    let call_id = string_field(block, &["tool_use_id", "id"])?;
    let pending = state.pending_web_searches.remove(&call_id)?;
    let output = tool_result_blocks(block.get("content").unwrap_or(&Value::Null));
    let output_text = output_text(&output);
    Some(WorkerFlowItem::WebSearch {
        env: env_for(record, seq, turn, session_id, raw_ref),
        call_id: Some(ToolCallId::from(call_id)),
        query: pending.query,
        results_summary: Some(summary(&output_text)),
    })
}

fn tool_result_is_error(block: &Value) -> bool {
    block
        .get("is_error")
        .or_else(|| block.get("isError"))
        .and_then(Value::as_bool)
        .unwrap_or(false)
}

struct BashResult {
    aggregated_output: String,
    exit_code: i32,
}

fn build_bash_completion(record: &Value, block: &Value) -> Option<BashResult> {
    if let Some(content) = block.get("content")
        && let Some(result) = parse_bash_result_tagged(content)
    {
        return Some(result);
    }

    let tool_use_result = record.get("toolUseResult")?;
    let stdout = tool_use_result
        .get("stdout")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let stderr = tool_use_result
        .get("stderr")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let exit_code = tool_use_result
        .get("exit_code")
        .or_else(|| tool_use_result.get("exitCode"))
        .and_then(Value::as_i64)
        .and_then(|code| i32::try_from(code).ok())
        // Claude often omits exit_code here; -1 is the error sentinel.
        .unwrap_or_else(|| if tool_result_is_error(block) { -1 } else { 0 });
    let aggregated_output = match (stdout.is_empty(), stderr.is_empty()) {
        (false, false) => format!("{stdout}\n{stderr}"),
        (false, true) => stdout.to_string(),
        (true, false) => stderr.to_string(),
        (true, true) => String::new(),
    };
    Some(BashResult {
        aggregated_output,
        exit_code,
    })
}

fn parse_bash_result_tagged(content: &Value) -> Option<BashResult> {
    let text = match content {
        Value::String(text) => text.clone(),
        Value::Array(items) => items
            .iter()
            .filter_map(|item| string_field(item, &["text", "content"]))
            .collect::<Vec<_>>()
            .join("\n"),
        other => other.to_string(),
    };
    let exit_code = tag_value(&text, "exit_code")?.trim().parse::<i32>().ok()?;
    let stdout = tag_value(&text, "stdout").unwrap_or_default();
    let stderr = tag_value(&text, "stderr").unwrap_or_default();
    let aggregated_output = match (stdout.trim().is_empty(), stderr.trim().is_empty()) {
        (false, false) => format!("{stdout}{stderr}"),
        (false, true) => stdout,
        (true, false) => stderr,
        (true, true) => text,
    };
    Some(BashResult {
        aggregated_output,
        exit_code,
    })
}

fn tag_value(text: &str, tag: &str) -> Option<String> {
    let start_tag = format!("<{tag}>");
    let end_tag = format!("</{tag}>");
    let start = text.find(&start_tag)? + start_tag.len();
    let end = text[start..].find(&end_tag)? + start;
    Some(text[start..end].to_string())
}

fn unknown_item(
    record: &Value,
    seq: u64,
    turn: u32,
    session_id: &WorkerSessionId,
    raw_ref: RawRef,
) -> WorkerFlowItem {
    WorkerFlowItem::Unknown {
        env: env_for(record, seq, turn, session_id, raw_ref),
        raw_type: raw_type_for_record(record),
    }
}

fn raw_type_for_record(record: &Value) -> String {
    match record
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or("unknown")
    {
        "attachment" => format!(
            "attachment/{}",
            string_field(record, &["hook_event", "hookEvent"])
                .unwrap_or_else(|| "unknown".to_string())
        ),
        "system" => format!(
            "system/{}",
            string_field(record, &["subtype"]).unwrap_or_else(|| "unknown".to_string())
        ),
        "queue-operation" => format!(
            "queue_operation/{}",
            string_field(record, &["operation"]).unwrap_or_else(|| "unknown".to_string())
        ),
        other => other.to_string(),
    }
}

fn env_for(
    record: &Value,
    seq: u64,
    turn: u32,
    session_id: &WorkerSessionId,
    raw_ref: RawRef,
) -> FlowEnvelope {
    FlowEnvelope {
        seq,
        turn,
        session_id: session_id.clone(),
        provider: WorkerProviderKind::Claude,
        timestamp: record
            .get("timestamp")
            .and_then(Value::as_str)
            .and_then(parse_timestamp_ms),
        source_uuid: source_uuid(record),
        provider_extra: None,
        raw_ref: Some(raw_ref),
    }
}

fn parse_timestamp_ms(timestamp: &str) -> Option<i64> {
    chrono::DateTime::parse_from_rfc3339(timestamp)
        .ok()
        .map(|dt| dt.timestamp_millis())
}

fn content_type(item: &Value) -> Option<&str> {
    item.get("type").and_then(Value::as_str)
}

fn string_field(item: &Value, keys: &[&str]) -> Option<String> {
    keys.iter()
        .find_map(|key| item.get(*key).and_then(Value::as_str).map(str::to_string))
}

fn message_block_from_content(item: &Value) -> MessageBlock {
    match content_type(item).unwrap_or("unknown") {
        "text" => string_field(item, &["text"])
            .map(|text| MessageBlock::Text { text })
            .unwrap_or_else(|| unsupported_content_block("text")),
        "image" | "image_url" => MessageBlock::Image {
            url: string_field(item, &["url", "image_url"]),
            path: string_field(item, &["path"]),
        },
        "file" | "file_ref" => string_field(item, &["path", "file_path"])
            .map(|path| MessageBlock::FileRef { path })
            .unwrap_or_else(|| unsupported_content_block("file")),
        "mention" => string_field(item, &["name", "text", "path"])
            .map(|name| MessageBlock::Mention {
                name,
                path: string_field(item, &["path"]),
            })
            .unwrap_or_else(|| unsupported_content_block("mention")),
        raw_type => unsupported_content_block(raw_type),
    }
}

fn unsupported_content_block(raw_type: &str) -> MessageBlock {
    MessageBlock::Text {
        text: format!("[unsupported content block: {raw_type}]"),
    }
}

fn tool_result_blocks(content: &Value) -> Vec<ToolResultBlock> {
    match content {
        Value::String(text) => vec![ToolResultBlock::Text { text: text.clone() }],
        Value::Array(items) => items.iter().map(tool_result_block).collect(),
        Value::Null => vec![ToolResultBlock::Text {
            text: String::new(),
        }],
        other => vec![ToolResultBlock::Text {
            text: other.to_string(),
        }],
    }
}

fn tool_result_block(item: &Value) -> ToolResultBlock {
    match content_type(item).unwrap_or("text") {
        "text" => ToolResultBlock::Text {
            text: string_field(item, &["text", "content"]).unwrap_or_else(|| item.to_string()),
        },
        "image" | "image_url" => ToolResultBlock::Image {
            url: string_field(item, &["url", "image_url"]),
        },
        "file" | "file_ref" => ToolResultBlock::FileRef {
            path: string_field(item, &["path", "file_path"]).unwrap_or_default(),
        },
        raw_type => ToolResultBlock::Text {
            text: format!("[unsupported content block: {raw_type}]"),
        },
    }
}

fn output_text(output: &[ToolResultBlock]) -> String {
    output
        .iter()
        .filter_map(|block| match block {
            ToolResultBlock::Text { text } => Some(text.clone()),
            ToolResultBlock::Image { url } => url.clone(),
            ToolResultBlock::FileRef { path } => Some(path.clone()),
        })
        .collect::<Vec<_>>()
        .join("\n")
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

fn parse_mcp_tool_name(name: &str) -> (Option<String>, String) {
    let rest = name.strip_prefix("mcp__").unwrap_or(name);
    let mut parts = rest.split("__");
    let first = parts.next().unwrap_or_default();
    let remaining = parts.collect::<Vec<_>>();
    if remaining.is_empty() {
        (None, first.to_string())
    } else {
        (Some(first.to_string()), remaining.join("__"))
    }
}

fn summary(text: &str) -> String {
    const LIMIT: usize = 240;
    let mut out = text.chars().take(LIMIT).collect::<String>();
    if text.chars().count() > LIMIT {
        out.push_str("...");
    }
    out
}
