//! Pure translation of one Claude turn's [`Record`]s into the [`PlannerEvent`]s the Planner harness
//! consumes (design #1791 §4.3, §6.1, #1981 S4): `TurnStarted`, `TurnCompleted`, `Item`,
//! `TokenUsage` and `ReplyDelta` (#1923), nothing else.
//!
//! Item envelope `{threadId, turnId, item, startedAtMs | completedAtMs}`. Item ids: `tool_use.id` for
//! tools, `<message.id>:<stream block index>` for text and thinking blocks (#1923), the replay's uuid
//! for the user message. The stored user message is built from the issued input, never from the
//! CLI's replay, which echoes every image as base64.
//!
//! A text block streams (`--include-partial-messages`): its `content_block_start` emits the
//! `agentMessage` start, each `text_delta` one [`PlannerEventKind::ReplyDelta`], and the `assistant`
//! record that carries the block, which the CLI writes before the block's `content_block_stop`,
//! completes it under the same id.

use std::collections::{BTreeMap, HashMap};

use serde_json::{Value, json};
use uuid::Uuid;

use super::protocol::{
    AssistantBlock, BlockKind, ModelUsage, ProtocolError, Record, StreamEvent, ToolResult,
    ToolResultBlock, ToolResultContent, Usage, chain_entry_uuid, client_line_uuid,
};
use crate::InputItem;
use crate::events::{ItemPhase, PlannerEvent, PlannerEventKind};

/// The kernel tools visible to the card, by the name Claude gives them.
#[derive(Debug, Clone)]
pub struct ToolNames {
    server_key: String,
    by_claude_name: HashMap<String, Vec<String>>,
}

impl ToolNames {
    /// `visible` holds the dotted registry names the card's `tools/list` answers with.
    pub fn new(server_key: impl Into<String>, visible: impl IntoIterator<Item = String>) -> Self {
        let mut by_claude_name: HashMap<String, Vec<String>> = HashMap::new();
        for dotted in visible {
            by_claude_name
                .entry(claude_sanitized(&dotted))
                .or_default()
                .push(dotted);
        }
        Self {
            server_key: server_key.into(),
            by_claude_name,
        }
    }

    /// The dotted name for Claude's `<sanitized>` spelling, only when exactly one visible tool has it.
    fn restore(&self, sanitized: &str) -> Option<&str> {
        match self.by_claude_name.get(sanitized).map(Vec::as_slice) {
            Some([dotted]) => Some(dotted.as_str()),
            _ => None,
        }
    }
}

/// The `<sanitized>` part of Claude's `mcp__<server key>__<sanitized>` kernel tool spelling.
fn kernel_tool_suffix<'a>(name: &'a str, server_key: &str) -> Option<&'a str> {
    name.strip_prefix("mcp__")?
        .strip_prefix(server_key)?
        .strip_prefix("__")
}

/// Claude's MCP tool spelling: every char outside `[A-Za-z0-9_-]` becomes `_`.
fn claude_sanitized(name: &str) -> String {
    name.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

/// What the turn was issued with.
#[derive(Debug, Clone)]
pub struct TurnContext {
    pub thread_id: String,
    pub turn_id: String,
    /// The harness client id; the stdin line's `uuid` is [`client_line_uuid`] of it.
    pub client_id: String,
    /// The issued input: text plus `localImage` placeholders, stored as the user message's content.
    pub input: Vec<InputItem>,
    /// The workspace the CLI runs in, reported as every command's `cwd`.
    pub cwd: String,
    /// The thread's lifetime token total before this turn, from the harness snapshot.
    pub prior_total_tokens: i64,
}

/// How a turn ended, as the stored outcome row spells it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TurnOutcome {
    Completed,
    Interrupted,
    Failed { message: String },
}

#[derive(Debug, Clone)]
enum OpenTool {
    /// `ToolSearch` loads tool schemas; it is plumbing, not an action, and renders nothing.
    Suppressed,
    Shown {
        kind: ShownTool,
        started_at_ms: i64,
        item: Value,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ShownTool {
    Mcp,
    Command,
    FileChange,
    Dynamic,
}

/// The message the stream is in, and its last started block, which the message's next record
/// completes.
#[derive(Debug)]
struct StreamedMessage {
    message_id: String,
    block: Option<StreamedBlock>,
}

#[derive(Debug, Clone, Copy)]
struct StreamedBlock {
    index: u64,
    kind: BlockKind,
}

/// Translates one turn; one per `claude -p` process.
#[derive(Debug)]
pub struct TurnTranslator {
    ctx: TurnContext,
    line_uuid: Uuid,
    tools: ToolNames,
    /// `system/init`'s model, the key of `modelUsage` that carries the context window.
    init_model: Option<String>,
    open: HashMap<String, OpenTool>,
    total_tokens: i64,
    streamed: Option<StreamedMessage>,
    /// The last record's `message.id` and how many of its blocks records have carried: a block
    /// without a stream block takes that count as its index.
    record_blocks: Option<(String, u64)>,
    /// The uuid of the last chain record this turn produced, whether or not it made an item: the
    /// point a later rewind keeps the conversation up to (`--resume-session-at`).
    last_record_uuid: Option<Uuid>,
    /// The clock at [`Self::turn_started`]. While it is `None` (the start never ran), the outcome
    /// carries no `durationMs`.
    started_at_ms: Option<i64>,
}

impl TurnTranslator {
    pub fn new(ctx: TurnContext, tools: ToolNames) -> Result<Self, ProtocolError> {
        let line_uuid = client_line_uuid(&ctx.client_id)?;
        let total_tokens = ctx.prior_total_tokens;
        Ok(Self {
            ctx,
            line_uuid,
            tools,
            init_model: None,
            open: HashMap::new(),
            total_tokens,
            streamed: None,
            record_blocks: None,
            last_record_uuid: None,
            started_at_ms: None,
        })
    }

    pub fn turn_started(&mut self, now_ms: i64) -> PlannerEvent {
        self.started_at_ms = Some(now_ms);
        self.event(PlannerEventKind::TurnStarted {
            turn_id: self.ctx.turn_id.clone(),
        })
    }

    /// The outcome row's turn JSON. `durationMs` is the clock from [`Self::turn_started`] to `now_ms`,
    /// on the same injected clock the item durations use.
    pub fn turn_completed(&self, outcome: &TurnOutcome, now_ms: i64) -> PlannerEvent {
        let (status, error) = match outcome {
            TurnOutcome::Completed => ("completed", Value::Null),
            TurnOutcome::Interrupted => ("interrupted", Value::Null),
            TurnOutcome::Failed { message } => ("failed", json!({ "message": message })),
        };
        let mut turn = json!({ "id": self.ctx.turn_id, "status": status, "error": error });
        if let Some(uuid) = self.last_record_uuid {
            turn["lastRecordUuid"] = json!(uuid.to_string());
        }
        if let Some(started_at_ms) = self.started_at_ms {
            turn["durationMs"] = json!(now_ms.saturating_sub(started_at_ms));
        }
        self.event(PlannerEventKind::TurnCompleted { turn })
    }

    fn event(&self, kind: PlannerEventKind) -> PlannerEvent {
        PlannerEvent {
            thread_id: Some(self.ctx.thread_id.clone()),
            kind,
        }
    }

    /// [`Self::translate`] for one stdout line and its record: a chain entry, whether or not it makes
    /// an item, becomes the turn's last record (the `lastRecordUuid` a later rewind keeps up to).
    pub fn translate_line(
        &mut self,
        line: &str,
        record: &Record,
        now_ms: i64,
    ) -> Vec<PlannerEvent> {
        if let Some(uuid) = chain_entry_uuid(line) {
            self.last_record_uuid = Some(uuid);
        }
        self.translate(record, now_ms)
    }

    /// The events one record produces, in order. Terminal records yield at most the usage
    /// frame; the outcome is the caller's (design §6.2).
    pub fn translate(&mut self, record: &Record, now_ms: i64) -> Vec<PlannerEvent> {
        match record {
            Record::SystemInit(init) => {
                self.init_model = Some(init.model.clone());
                Vec::new()
            }
            Record::UserReplay { uuid, .. } if *uuid == self.line_uuid => {
                vec![self.item(ItemPhase::Completed, self.user_message(uuid), now_ms)]
            }
            Record::Assistant { message_id, blocks } => blocks
                .iter()
                .flat_map(|block| self.assistant_block(message_id, block, now_ms))
                .collect(),
            Record::Stream(event) => self.stream_event(event, now_ms).into_iter().collect(),
            Record::UserToolResults {
                results,
                tool_use_result,
                ..
            } => {
                // The structured view is per record; it describes a result only when it is alone.
                let structured = match results.as_slice() {
                    [_] => tool_use_result,
                    _ => {
                        tracing::warn!(
                            results = results.len(),
                            "claude planner: one line carries several tool results; \
                             their structured payloads are dropped"
                        );
                        &Value::Null
                    }
                };
                results
                    .iter()
                    .filter_map(|result| self.tool_result(result, structured, now_ms))
                    .collect()
            }
            Record::ResultSuccess(success) => self
                .usage_frame(&success.usage, &success.model_usage)
                .into_iter()
                .collect(),
            Record::ResultError(error) => self
                .usage_frame(&error.usage, &error.model_usage)
                .into_iter()
                .collect(),
            Record::UserReplay { .. }
            | Record::UserText { .. }
            | Record::ControlResponseIn { .. }
            | Record::ControlRequestIn { .. }
            | Record::Ignored { .. } => Vec::new(),
        }
    }

    /// Completes every tool item still open as `failed`, oldest first, and forgets them: a turn
    /// that settles without a tool's result (killed, crashed) must not leave it running.
    pub fn close_open(&mut self, now_ms: i64) -> Vec<PlannerEvent> {
        let mut open: Vec<(i64, String, Value)> = self
            .open
            .drain()
            .filter_map(|(id, tool)| match tool {
                OpenTool::Shown {
                    started_at_ms,
                    item,
                    ..
                } => Some((started_at_ms, id, item)),
                OpenTool::Suppressed => None,
            })
            .collect();
        open.sort_by(|a, b| (a.0, &a.1).cmp(&(b.0, &b.1)));
        open.into_iter()
            .map(|(started_at_ms, _, mut item)| {
                item["status"] = json!("failed");
                item["durationMs"] = json!(now_ms.saturating_sub(started_at_ms));
                self.item(ItemPhase::Completed, item, now_ms)
            })
            .collect()
    }

    fn item(&self, phase: ItemPhase, item: Value, now_ms: i64) -> PlannerEvent {
        let at_key = match phase {
            ItemPhase::Started => "startedAtMs",
            ItemPhase::Completed => "completedAtMs",
        };
        let mut params = json!({
            "threadId": self.ctx.thread_id,
            "turnId": self.ctx.turn_id,
            "item": item,
        });
        params[at_key] = json!(now_ms);
        self.event(PlannerEventKind::Item { phase, params })
    }

    fn user_message(&self, uuid: &Uuid) -> Value {
        json!({
            "id": uuid.to_string(),
            "type": "userMessage",
            "clientId": self.ctx.client_id,
            "content": self.ctx.input,
        })
    }

    /// A text block's start opens its reply and each of its deltas extends it; nothing else in the
    /// stream is an event.
    fn stream_event(&mut self, event: &StreamEvent, now_ms: i64) -> Option<PlannerEvent> {
        match event {
            StreamEvent::MessageStart { message_id } => {
                self.streamed = Some(StreamedMessage {
                    message_id: message_id.clone(),
                    block: None,
                });
                None
            }
            StreamEvent::BlockStart { index, kind } => {
                let Some(message) = self.streamed.as_mut() else {
                    tracing::warn!(index, "claude planner: a block started outside a message");
                    return None;
                };
                message.block = Some(StreamedBlock {
                    index: *index,
                    kind: *kind,
                });
                let id = format!("{}:{index}", message.message_id);
                (*kind == BlockKind::Text).then(|| {
                    let item = json!({ "id": id, "type": "agentMessage", "text": "" });
                    self.item(ItemPhase::Started, item, now_ms)
                })
            }
            StreamEvent::TextDelta { index, text } => {
                let message = self.streamed.as_ref()?;
                let block = message.block?;
                if block.index != *index || block.kind != BlockKind::Text {
                    tracing::debug!(index, "claude planner: a text delta for no open text block");
                    return None;
                }
                Some(self.event(PlannerEventKind::ReplyDelta {
                    turn_id: self.ctx.turn_id.clone(),
                    item_id: format!("{}:{index}", message.message_id),
                    delta: text.clone(),
                }))
            }
        }
    }

    /// Where the block a record of `message_id` carries sits in its message, and whether the stream
    /// started it. A streamed block is the open stream block, if it is that message's and of the same
    /// kind; the record closes it. Any other block takes its place among the message's records,
    /// which is its stream index while the CLI writes one record per block.
    fn block_index(&mut self, message_id: &str, kind: BlockKind) -> (u64, bool) {
        let count = match self.record_blocks.as_mut() {
            Some((id, count)) if id == message_id => count,
            _ => &mut self.record_blocks.insert((message_id.to_owned(), 0)).1,
        };
        let place = *count;
        *count += 1;
        let streamed = self
            .streamed
            .as_mut()
            .filter(|message| message.message_id == message_id)
            .and_then(|message| message.block.take_if(|block| block.kind == kind));
        match streamed {
            Some(block) => (block.index, true),
            None => (place, false),
        }
    }

    fn assistant_block(
        &mut self,
        message_id: &str,
        block: &AssistantBlock,
        now_ms: i64,
    ) -> Vec<PlannerEvent> {
        let (index, streamed) = self.block_index(message_id, block.kind());
        let id = format!("{message_id}:{index}");
        let unstreamed = |kind: &str| {
            tracing::warn!(
                item_id = %id,
                kind,
                "claude planner: a record's block has no open stream block; it is stored without \
                 live text"
            );
        };
        match block {
            AssistantBlock::Thinking { .. } => {
                if !streamed {
                    unstreamed("thinking");
                }
                let item = json!({ "id": id, "type": "reasoning", "content": [], "summary": [] });
                vec![
                    self.item(ItemPhase::Started, item.clone(), now_ms),
                    self.item(ItemPhase::Completed, item, now_ms),
                ]
            }
            AssistantBlock::Text { text } => {
                let completed = json!({ "id": id, "type": "agentMessage", "text": text });
                if streamed {
                    // Started at its `content_block_start`.
                    return vec![self.item(ItemPhase::Completed, completed, now_ms)];
                }
                unstreamed("text");
                vec![
                    self.item(
                        ItemPhase::Started,
                        json!({ "id": id, "type": "agentMessage", "text": "" }),
                        now_ms,
                    ),
                    self.item(ItemPhase::Completed, completed, now_ms),
                ]
            }
            AssistantBlock::ToolUse { id, name, input } => {
                self.tool_use(id, name, input, now_ms).into_iter().collect()
            }
            AssistantBlock::Other => Vec::new(),
        }
    }

    fn tool_use(
        &mut self,
        id: &str,
        name: &str,
        input: &Value,
        now_ms: i64,
    ) -> Option<PlannerEvent> {
        if name == "ToolSearch" {
            self.open.insert(id.to_string(), OpenTool::Suppressed);
            return None;
        }
        let (kind, item) = if let Some(sanitized) = kernel_tool_suffix(name, &self.tools.server_key)
        {
            let tool = match self.tools.restore(sanitized) {
                Some(dotted) => dotted.to_string(),
                None => {
                    tracing::warn!(
                        tool = name,
                        "claude planner: no single visible kernel tool has this name; kept as is"
                    );
                    name.to_string()
                }
            };
            let item = json!({
                "id": id, "type": "mcpToolCall", "server": self.tools.server_key, "tool": tool,
                "arguments": input, "status": "inProgress",
            });
            (ShownTool::Mcp, item)
        } else if name == "Bash" {
            let item = json!({
                "id": id, "type": "commandExecution", "command": input.get("command"),
                "cwd": self.ctx.cwd, "status": "inProgress",
                "aggregatedOutput": null, "exitCode": null,
            });
            (ShownTool::Command, item)
        } else if name == "Edit" || name == "Write" {
            let kind = if name == "Write" { "add" } else { "update" };
            let item = json!({
                "id": id, "type": "fileChange", "status": "inProgress",
                "changes": [{ "path": input.get("file_path"), "kind": { "type": kind }, "diff": "" }],
            });
            (ShownTool::FileChange, item)
        } else {
            let item = json!({
                "id": id, "type": "dynamicToolCall", "tool": name,
                "arguments": input, "status": "inProgress",
            });
            (ShownTool::Dynamic, item)
        };
        let started = self.item(ItemPhase::Started, item.clone(), now_ms);
        self.open.insert(
            id.to_string(),
            OpenTool::Shown {
                kind,
                started_at_ms: now_ms,
                item,
            },
        );
        Some(started)
    }

    fn tool_result(
        &mut self,
        result: &ToolResult,
        structured: &Value,
        now_ms: i64,
    ) -> Option<PlannerEvent> {
        let Some(open) = self.open.remove(&result.tool_use_id) else {
            tracing::debug!(
                tool_use_id = %result.tool_use_id,
                "claude planner: tool result for a tool use this turn never saw"
            );
            return None;
        };
        let OpenTool::Shown {
            kind,
            started_at_ms,
            mut item,
        } = open
        else {
            return None;
        };
        let text = result_text(&result.content);
        let status = if result.is_error {
            "failed"
        } else {
            "completed"
        };
        item["status"] = json!(status);
        item["durationMs"] = json!(now_ms.saturating_sub(started_at_ms));
        match kind {
            ShownTool::Mcp if result.is_error => item["error"] = json!({ "message": text }),
            ShownTool::Mcp => item["result"] = json!({ "content": mcp_content(&result.content) }),
            ShownTool::Command => {
                item["exitCode"] = json!(exit_code(result.is_error, &text));
                item["aggregatedOutput"] = json!(text);
            }
            ShownTool::FileChange if !result.is_error => {
                let kind = if structured.get("type").and_then(Value::as_str) == Some("create") {
                    "add"
                } else {
                    "update"
                };
                item["changes"][0]["kind"] = json!({ "type": kind });
                item["changes"][0]["diff"] = json!(file_diff(structured));
            }
            ShownTool::FileChange | ShownTool::Dynamic => {}
        }
        Some(self.item(ItemPhase::Completed, item, now_ms))
    }

    /// A usage reading from a result with at least one iteration, success or error; nothing
    /// otherwise, so the harness keeps its previous reading.
    fn usage_frame(
        &mut self,
        usage: &Usage,
        model_usage: &BTreeMap<String, ModelUsage>,
    ) -> Option<PlannerEvent> {
        let last = usage.iterations.last()?;
        let last_total = last.input_tokens
            + last.cache_read_input_tokens
            + last.cache_creation_input_tokens
            + last.output_tokens;
        let turn_total = usage.input_tokens
            + usage.cache_read_input_tokens
            + usage.cache_creation_input_tokens
            + usage.output_tokens;
        self.total_tokens = self.total_tokens.saturating_add(turn_total);
        let window = self
            .init_model
            .as_ref()
            .and_then(|model| model_usage.get(model))
            .map(|usage| usage.context_window);
        if window.is_none() {
            tracing::debug!(
                init_model = ?self.init_model,
                "claude planner: no modelUsage entry for the init model; context window unknown"
            );
        }
        Some(self.event(PlannerEventKind::TokenUsage {
            params: json!({
                "tokenUsage": {
                    "last": { "totalTokens": last_total },
                    "total": { "totalTokens": self.total_tokens },
                    "modelContextWindow": window,
                },
            }),
        }))
    }
}

fn result_text(content: &ToolResultContent) -> String {
    match content {
        ToolResultContent::Text(text) => text.clone(),
        ToolResultContent::Blocks(blocks) => blocks
            .iter()
            .filter_map(|block| match block {
                ToolResultBlock::Text { text } => Some(text.as_str()),
                ToolResultBlock::Image {} | ToolResultBlock::Other => None,
            })
            .collect::<Vec<_>>()
            .join("\n"),
    }
}

/// An MCP result's content with every non-text block reduced to its kind, so no payload is stored.
fn mcp_content(content: &ToolResultContent) -> Value {
    match content {
        ToolResultContent::Text(text) => json!([{ "type": "text", "text": text }]),
        ToolResultContent::Blocks(blocks) => blocks
            .iter()
            .map(|block| match block {
                ToolResultBlock::Text { text } => json!({ "type": "text", "text": text }),
                ToolResultBlock::Image {} => json!({ "type": "image" }),
                ToolResultBlock::Other => json!({ "type": "other" }),
            })
            .collect(),
    }
}

/// 0 for a success; N from the CLI's leading `Exit code N` line; otherwise unknown.
fn exit_code(is_error: bool, text: &str) -> Option<i64> {
    if !is_error {
        return Some(0);
    }
    let rest = text.strip_prefix("Exit code ")?;
    rest.split('\n').next()?.trim().parse().ok()
}

/// A unified-diff body from the CLI's `structuredPatch`, or the whole file as added lines for a
/// newly created file.
fn file_diff(structured: &Value) -> String {
    let hunks = structured
        .get("structuredPatch")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default();
    if hunks.is_empty() {
        return match structured.get("content").and_then(Value::as_str) {
            Some(content) if structured.get("type").and_then(Value::as_str) == Some("create") => {
                content.lines().map(|line| format!("+{line}\n")).collect()
            }
            _ => String::new(),
        };
    }
    let mut diff = String::new();
    for hunk in hunks {
        let n = |key: &str| hunk.get(key).and_then(Value::as_i64).unwrap_or(0);
        diff.push_str(&format!(
            "@@ -{},{} +{},{} @@\n",
            n("oldStart"),
            n("oldLines"),
            n("newStart"),
            n("newLines")
        ));
        for line in hunk
            .get("lines")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            if let Some(line) = line.as_str() {
                diff.push_str(line);
                diff.push('\n');
            }
        }
    }
    diff
}
