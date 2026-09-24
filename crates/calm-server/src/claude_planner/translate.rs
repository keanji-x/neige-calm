//! Pure translation of one Claude turn's [`Record`]s into the Codex-shaped [`Notification`]s the
//! Planner harness already consumes (design #1791 §4.3, §6.1): `TurnStarted`, `TurnCompleted`,
//! `Item{item/started|item/completed}` and `Other{thread/tokenUsage/updated}`, nothing else.
//!
//! Item envelope `{threadId, turnId, item, startedAtMs | completedAtMs}`. Item ids: `tool_use.id` for
//! tools, `<record uuid>:<block index>` for text and thinking blocks, the replay's uuid for the user
//! message. The stored user message is built from the issued input, never from the CLI's replay,
//! which echoes every image as base64.

use std::collections::{BTreeMap, HashMap};

use serde_json::{Value, json};
use uuid::Uuid;

use super::protocol::{
    AssistantBlock, ModelUsage, ProtocolError, Record, ToolResult, ToolResultBlock,
    ToolResultContent, Usage, client_line_uuid,
};
use crate::codex_appserver::{InputItem, Notification};

/// The server name neige registers the calm MCP shim under.
const CALM_MCP_PREFIX: &str = "mcp__calm__";

/// The calm tools visible to the card, by the name Claude gives them.
#[derive(Debug, Clone)]
pub struct CalmToolNames {
    by_claude_name: HashMap<String, Vec<String>>,
}

impl CalmToolNames {
    /// `visible` holds the dotted registry names the card's `tools/list` answers with.
    pub fn new(visible: impl IntoIterator<Item = String>) -> Self {
        let mut by_claude_name: HashMap<String, Vec<String>> = HashMap::new();
        for dotted in visible {
            by_claude_name
                .entry(claude_sanitized(&dotted))
                .or_default()
                .push(dotted);
        }
        Self { by_claude_name }
    }

    /// The dotted name for Claude's `<sanitized>` spelling, only when exactly one visible tool has it.
    fn restore(&self, sanitized: &str) -> Option<&str> {
        match self.by_claude_name.get(sanitized).map(Vec::as_slice) {
            Some([dotted]) => Some(dotted.as_str()),
            _ => None,
        }
    }
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

/// Translates one turn; one per `claude -p` process.
#[derive(Debug)]
pub struct TurnTranslator {
    ctx: TurnContext,
    line_uuid: Uuid,
    tools: CalmToolNames,
    /// `system/init`'s model, the key of `modelUsage` that carries the context window.
    init_model: Option<String>,
    open: HashMap<String, OpenTool>,
    total_tokens: i64,
}

impl TurnTranslator {
    pub fn new(ctx: TurnContext, tools: CalmToolNames) -> Result<Self, ProtocolError> {
        let line_uuid = client_line_uuid(&ctx.client_id)?;
        let total_tokens = ctx.prior_total_tokens;
        Ok(Self {
            ctx,
            line_uuid,
            tools,
            init_model: None,
            open: HashMap::new(),
            total_tokens,
        })
    }

    pub fn turn_started(&self) -> Notification {
        Notification::TurnStarted {
            thread_id: self.ctx.thread_id.clone(),
            turn: json!({ "id": self.ctx.turn_id, "status": "inProgress", "error": null }),
        }
    }

    pub fn turn_completed(&self, outcome: &TurnOutcome) -> Notification {
        let (status, error) = match outcome {
            TurnOutcome::Completed => ("completed", Value::Null),
            TurnOutcome::Interrupted => ("interrupted", Value::Null),
            TurnOutcome::Failed { message } => ("failed", json!({ "message": message })),
        };
        Notification::TurnCompleted {
            thread_id: self.ctx.thread_id.clone(),
            turn: json!({ "id": self.ctx.turn_id, "status": status, "error": error }),
        }
    }

    /// The notifications one record produces, in order. Terminal records yield at most the usage
    /// frame; the outcome is the caller's (design §6.2).
    pub fn translate(&mut self, record: &Record, now_ms: i64) -> Vec<Notification> {
        match record {
            Record::SystemInit(init) => {
                self.init_model = Some(init.model.clone());
                Vec::new()
            }
            Record::UserReplay { uuid, .. } if *uuid == self.line_uuid => {
                vec![self.item("item/completed", self.user_message(uuid), now_ms)]
            }
            Record::Assistant { uuid, blocks } => blocks
                .iter()
                .enumerate()
                .flat_map(|(index, block)| self.assistant_block(uuid, index, block, now_ms))
                .collect(),
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
    pub fn close_open(&mut self, now_ms: i64) -> Vec<Notification> {
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
                self.item("item/completed", item, now_ms)
            })
            .collect()
    }

    fn item(&self, method: &str, item: Value, now_ms: i64) -> Notification {
        let at_key = if method == "item/started" {
            "startedAtMs"
        } else {
            "completedAtMs"
        };
        let mut params = json!({
            "threadId": self.ctx.thread_id,
            "turnId": self.ctx.turn_id,
            "item": item,
        });
        params[at_key] = json!(now_ms);
        Notification::Item {
            method: method.to_string(),
            params,
        }
    }

    fn user_message(&self, uuid: &Uuid) -> Value {
        json!({
            "id": uuid.to_string(),
            "type": "userMessage",
            "clientId": self.ctx.client_id,
            "content": self.ctx.input,
        })
    }

    fn assistant_block(
        &mut self,
        uuid: &Uuid,
        index: usize,
        block: &AssistantBlock,
        now_ms: i64,
    ) -> Vec<Notification> {
        let id = format!("{uuid}:{index}");
        match block {
            AssistantBlock::Thinking { .. } => {
                let item = json!({ "id": id, "type": "reasoning", "content": [], "summary": [] });
                vec![
                    self.item("item/started", item.clone(), now_ms),
                    self.item("item/completed", item, now_ms),
                ]
            }
            AssistantBlock::Text { text } => vec![
                self.item(
                    "item/started",
                    json!({ "id": id, "type": "agentMessage", "text": "" }),
                    now_ms,
                ),
                self.item(
                    "item/completed",
                    json!({ "id": id, "type": "agentMessage", "text": text }),
                    now_ms,
                ),
            ],
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
    ) -> Option<Notification> {
        if name == "ToolSearch" {
            self.open.insert(id.to_string(), OpenTool::Suppressed);
            return None;
        }
        let (kind, item) = if let Some(sanitized) = name.strip_prefix(CALM_MCP_PREFIX) {
            let tool = match self.tools.restore(sanitized) {
                Some(dotted) => dotted.to_string(),
                None => {
                    tracing::warn!(
                        tool = name,
                        "claude planner: no single visible calm tool has this name; kept as is"
                    );
                    name.to_string()
                }
            };
            let item = json!({
                "id": id, "type": "mcpToolCall", "server": "calm", "tool": tool,
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
        let started = self.item("item/started", item.clone(), now_ms);
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
    ) -> Option<Notification> {
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
        Some(self.item("item/completed", item, now_ms))
    }

    /// `thread/tokenUsage/updated` from a result with at least one iteration, success or error;
    /// nothing otherwise, so the harness keeps its previous reading.
    fn usage_frame(
        &mut self,
        usage: &Usage,
        model_usage: &BTreeMap<String, ModelUsage>,
    ) -> Option<Notification> {
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
        Some(Notification::Other {
            method: "thread/tokenUsage/updated".to_string(),
            params: json!({
                "threadId": self.ctx.thread_id,
                "turnId": self.ctx.turn_id,
                "tokenUsage": {
                    "last": { "totalTokens": last_total },
                    "total": { "totalTokens": self.total_tokens },
                    "modelContextWindow": window,
                },
            }),
        })
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
