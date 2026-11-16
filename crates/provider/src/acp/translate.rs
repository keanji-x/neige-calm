//! Pure ACP updates → PlannerEvent translation. Native tool names remain display metadata.
use super::{
    Error,
    content::{self, Footprint},
    protocol::StopReason,
};
use crate::events::{ItemPhase, OutputAppendOp, PlannerEvent, PlannerEventKind};
use calm_types::output_content::{OutputBlock, OutputChannel, OutputContentV1, OutputSegment};
use serde_json::{Value, json};
use std::collections::BTreeMap;

pub struct TurnContext {
    pub thread_id: String,
    pub turn_id: String,
    pub native_session_id: String,
    pub usage: super::usage::UsageTracker,
    /// The agent publishes terminal updates for background tools across turns in a session.
    pub background_tool_completions: bool,
}
struct OpenOutput {
    id: String,
    segment: OutputSegment,
    content: OutputContentV1,
}
impl OpenOutput {
    fn can_merge(&self, next: &OutputBlock) -> bool {
        matches!((self.content.blocks.last(),next), (Some(OutputBlock::Text {fields:a,chunk:ac,..}), OutputBlock::Text {fields:b,chunk:bc,..}) if a == b && ac == bc)
    }
    fn append(&mut self, block: OutputBlock, merge: bool) {
        if merge {
            if let (Some(OutputBlock::Text { text: old, .. }), OutputBlock::Text { text, .. }) =
                (self.content.blocks.last_mut(), block)
            {
                old.push_str(&text);
            }
        } else {
            self.content.blocks.push(block);
        }
    }
    fn materialize(&self) -> Value {
        self.content
            .materialize_item(json!({"id":self.id}), &self.segment)
    }
}
pub struct TurnTranslator {
    context: TurnContext,
    compactions: super::compaction::Compactions,
    tool_costs: BTreeMap<String, Footprint>,
    tools: BTreeMap<String, Value>,
    sequence: u64,
    current: Option<OpenOutput>,
    retained: Footprint,
    ended: bool,
    context_received: bool,
    // Failure barriers distinguish a later recovery tool from an already-running sibling.
    unanswered_tool_failures: BTreeMap<String, u64>,
    tool_starts: BTreeMap<String, u64>,
    tool_sequence: u64,
}
impl TurnTranslator {
    pub fn new(context: TurnContext) -> Self {
        Self {
            context,
            compactions: Default::default(),
            tool_costs: BTreeMap::new(),
            tools: BTreeMap::new(),
            sequence: 0,
            current: None,
            retained: Footprint::default(),
            ended: false,
            context_received: false,
            unanswered_tool_failures: BTreeMap::new(),
            tool_starts: BTreeMap::new(),
            tool_sequence: 0,
        }
    }
    pub fn started(&self) -> PlannerEvent {
        self.event(PlannerEventKind::TurnStarted {
            turn_id: self.context.turn_id.clone(),
        })
    }
    pub fn context_received(&self) -> bool {
        self.context_received
    }
    /// Compaction state keyed by the native ID, including settled replay upserts.
    /// The caller must establish operation ownership separately; there is no "latest" ID.
    pub fn compactions(&self) -> &BTreeMap<String, super::compaction::Compaction> {
        &self.compactions.entries
    }
    pub fn update(&mut self, params: &Value, at_ms: i64) -> Result<Vec<PlannerEvent>, Error> {
        if self.ended {
            return Err(Error::Protocol("output after turn end"));
        }
        if params["sessionId"].as_str() != Some(&self.context.native_session_id) {
            return Err(Error::Protocol("update session identity mismatch"));
        }
        let update = &params["update"];
        let kind = update["sessionUpdate"]
            .as_str()
            .ok_or(Error::Protocol("missing session update kind"))?;
        if let Some(compaction) = super::compaction::CompactionUpdate::decode(update)? {
            self.compactions
                .apply(compaction, update.to_string().len())?;
            return Ok(Vec::new());
        }
        let mut events = Vec::new();
        if let Some((used, size)) = self.context.usage.reading(params) {
            self.context_received = true;
            events.push(self.event(PlannerEventKind::TokenUsage {
                params: json!({"turnId": self.context.turn_id, "tokenUsage": {
                    "last": {"totalTokens": used}, "modelContextWindow": size,
                }}),
            }));
        }
        match kind {
            "agent_message_chunk" | "agent_thought_chunk" => {
                let block =
                    content::decode(&update["content"], content::ContentContext::OutputRetained)?;
                let channel = if kind == "agent_message_chunk" {
                    OutputChannel::Message
                } else {
                    OutputChannel::Thought
                };
                let mut fields = serde_json::Map::new();
                if let Some(id) = update.get("messageId") {
                    if !id.is_null() && !id.is_string() {
                        return Err(Error::Protocol("invalid message affiliation"));
                    }
                    fields.insert("messageId".into(), id.clone());
                }
                if let Some(meta) = update.get("_meta") {
                    if !meta.is_null() && !meta.is_object() {
                        return Err(Error::Protocol("invalid chunk metadata"));
                    }
                    fields.insert("chunkMeta".into(), meta.clone());
                }
                content::bounded_metadata(&Value::Object(fields.clone()))?;
                // Chunk metadata belongs to each block; segment metadata records its first chunk.
                let mut block = block;
                if let Some(meta) = fields.get("chunkMeta") {
                    block.set_chunk_meta(meta.clone());
                }
                content::validate_output_metadata(&block)?;
                let same = self.current.as_ref().is_some_and(|open| {
                    open.segment.channel == channel
                        && open
                            .segment
                            .fields
                            .get("messageId")
                            .filter(|v| !v.is_null())
                            == fields.get("messageId").filter(|v| !v.is_null())
                });
                let merge = same
                    && self
                        .current
                        .as_ref()
                        .is_some_and(|open| open.can_merge(&block));
                let next = if !same {
                    Some(
                        self.sequence
                            .checked_add(1)
                            .ok_or(Error::Protocol("output ordinal overflow"))?,
                    )
                } else {
                    None
                };
                let segment = next.map(|ordinal| OutputSegment {
                    channel: channel.clone(),
                    ordinal,
                    fields: fields.clone(),
                });
                let mut cost = content::footprint(&block);
                if merge {
                    cost.material = 0;
                } else if same {
                    cost.material += 1;
                } // comma in a nonempty retained block array
                if let Some(segment) = &segment {
                    // The first chunk metadata is mirrored on the immutable segment. Charge
                    // its one logical copy on the block, as with compatibility/native mirrors.
                    let mut segment = segment.clone();
                    segment.fields.remove("chunkMeta");
                    cost.material += content::json_size(
                        &json!({"outputContent":OutputContentV1::new(Vec::new()),"outputSegment":segment}),
                    );
                }
                let retained = self.retained.combine(cost).check()?;
                // No mutation, seal or observation precedes candidate validation.
                if let Some(ordinal) = next {
                    self.complete_current(&mut events, at_ms);
                    self.sequence = ordinal;
                    let open = OpenOutput {
                        id: format!("acp-output:{}", json!([self.context.turn_id, ordinal])),
                        segment: segment.expect("new segment"),
                        content: OutputContentV1::new(Vec::new()),
                    };
                    events.push(self.item(open.materialize(), ItemPhase::Started, at_ms));
                    self.current = Some(open);
                }
                let delta = block.text().to_owned();
                let open = self.current.as_mut().expect("accepted segment");
                let op = if merge {
                    OutputAppendOp::AppendText {
                        text: delta.clone(),
                    }
                } else {
                    OutputAppendOp::AppendBlock {
                        block: block.clone(),
                    }
                };
                open.append(block, merge);
                let id = open.id.clone();
                self.retained = retained;
                if channel == OutputChannel::Message && !delta.trim().is_empty() {
                    self.unanswered_tool_failures.clear();
                }
                events.push(self.event(PlannerEventKind::OutputAppend {
                    turn_id: self.context.turn_id.clone(),
                    item_id: id,
                    op,
                }));
            }
            "tool_call" | "tool_call_update" => {
                let id = update["toolCallId"]
                    .as_str()
                    .ok_or(Error::Protocol("missing tool call identity"))?;
                if kind == "tool_call_update"
                    && self.context.background_tool_completions
                    && matches!(update["status"].as_str(), Some("completed" | "failed"))
                    && !id.is_empty()
                    && !self.tools.contains_key(id)
                {
                    // The current turn cannot own a tool it never saw start.
                    return Ok(events);
                }
                if kind == "tool_call" && self.tools.contains_key(id) {
                    return Err(Error::Protocol("duplicate tool call"));
                }
                let mut tool = if kind == "tool_call" {
                    json!({"toolCallId":id,"status":"pending"})
                } else {
                    self.tools
                        .get(id)
                        .cloned()
                        .ok_or(Error::Protocol("tool update without tool call"))?
                };
                if kind == "tool_call" && !update["title"].is_string() {
                    return Err(Error::Protocol("missing tool title"));
                }
                // ACP v1 optional patch values use omitted/null for no change.
                let patch_value = |field| update.get(field).filter(|value| !value.is_null());
                if let Some(name) = patch_value("name").and_then(Value::as_str) {
                    tool["name"] = json!(name);
                }
                for field in [
                    "title",
                    "kind",
                    "status",
                    "rawInput",
                    "rawOutput",
                    "content",
                    "locations",
                ] {
                    let value = if kind == "tool_call_update"
                        || matches!(field, "rawInput" | "rawOutput")
                    {
                        patch_value(field)
                    } else {
                        update.get(field)
                    };
                    if let Some(value) = value {
                        tool[field] = value.clone();
                    }
                }
                // ACP does not define _meta as a ToolCallUpdateFields patch.
                // Retain the existing opaque metadata behavior separately.
                if let Some(meta) = update.get("_meta") {
                    tool["_meta"] = meta.clone();
                }
                let status = match tool["status"].as_str() {
                    Some("pending" | "in_progress") => "inProgress",
                    Some("completed") => "completed",
                    Some("failed") => "failed",
                    _ => return Err(Error::Protocol("invalid tool call status")),
                };
                let blocks = content::tool_blocks(&tool)?;
                let output = OutputContentV1::new(blocks);
                let mut overhead = tool.clone();
                overhead.as_object_mut().unwrap().remove("content");
                content::bounded_metadata(&overhead)?;
                let cost = output.blocks.iter().enumerate().fold(
                    Footprint { text:0, material:content::json_size(&json!({"native":overhead,"outputContent":OutputContentV1::new(Vec::new())})) },
                    |cost,(index,block)| cost.combine(content::footprint(block)).combine(Footprint {text:0,material:usize::from(index>0)}),
                );
                let retained = self
                    .retained
                    .replace(self.tool_costs.get(id).copied().unwrap_or_default(), cost)
                    .check()?;
                if self.tools.len() >= 4096 && !self.tools.contains_key(id) {
                    return Err(Error::Protocol("turn tool count exceeds limit"));
                }
                let content = output
                    .blocks
                    .iter()
                    .map(content::tool_compatibility)
                    .collect::<Vec<_>>();
                self.complete_current(&mut events, at_ms);
                if kind == "tool_call" {
                    self.tool_sequence += 1;
                    self.tool_starts.insert(id.to_owned(), self.tool_sequence);
                }
                self.retained = retained;
                self.tool_costs.insert(id.to_owned(), cost);
                let item = json!({"id":format!("acp-{}-tool-{id}",self.context.turn_id),"type":"dynamicToolCall","tool":tool["title"],"arguments":tool.get("rawInput").cloned().unwrap_or(Value::Null),"status":status,"result":{"content":content,"structuredContent":tool.get("rawOutput").cloned().unwrap_or(Value::Null)},"native":tool,"outputContent":output});
                let phase = if matches!(status, "completed" | "failed") {
                    ItemPhase::Completed
                } else {
                    ItemPhase::Started
                };
                events.push(self.item(item, phase, at_ms));
                if status == "failed"
                    && self
                        .tools
                        .get(id)
                        .is_none_or(|previous| previous["status"] != "failed")
                {
                    self.unanswered_tool_failures
                        .insert(id.to_owned(), self.tool_sequence);
                }
                if status == "completed"
                    && self
                        .tools
                        .get(id)
                        .is_none_or(|old| old["status"] != "completed")
                {
                    let started = self.tool_starts[id];
                    self.unanswered_tool_failures
                        .retain(|_, failed_at| *failed_at >= started);
                }
                self.tools.insert(id.into(), tool);
            }
            "plan" => {
                let entries = update["entries"]
                    .as_array()
                    .ok_or(Error::Protocol("missing plan entries"))?;
                let mut plan = Vec::new();
                for entry in entries {
                    let step = entry["content"]
                        .as_str()
                        .ok_or(Error::Protocol("invalid plan step"))?;
                    let status = entry["status"]
                        .as_str()
                        .filter(|s| matches!(*s, "pending" | "in_progress" | "completed"))
                        .ok_or(Error::Protocol("invalid plan status"))?;
                    plan.push(json!({"step":step,"status":status}));
                }
                events.push(self.event(PlannerEventKind::PlanUpdated { params: json!({"threadId":self.context.thread_id,"turnId":self.context.turn_id,"plan":plan}) }));
            }
            // User echoes do not replace the kernel's authoritative input projection.
            _ => {}
        }
        Ok(events)
    }
    /// Keep an explicit interruption authoritative; explain a silent failure only when its tool
    /// identity matches a permission this turn declined.
    pub fn finish_after_permission(
        &mut self,
        reason: StopReason,
        declined: Option<&super::approvals::DeclinedPermission>,
        at_ms: i64,
    ) -> Vec<PlannerEvent> {
        if matches!(
            reason,
            StopReason::EndTurn | StopReason::MaxTokens | StopReason::MaxTurnRequests
        ) && let Some(declined) = declined
            && self
                .unanswered_tool_failures
                .contains_key(&declined.tool_call_id)
        {
            let (code, message) = declined.diagnostic.diagnostic();
            return self.terminal("failed", json!({"code":code, "message":message}), at_ms);
        }
        self.finish(reason, at_ms)
    }

    pub fn finish(&mut self, reason: StopReason, at_ms: i64) -> Vec<PlannerEvent> {
        let (status, error) = match reason {
            StopReason::Cancelled => ("interrupted", Value::Null),
            StopReason::Refusal => ("failed", json!({"message":"ACP agent refused the turn"})),
            StopReason::EndTurn | StopReason::MaxTokens | StopReason::MaxTurnRequests => {
                // The stop reason closes the prompt. The tool results and reply tell us whether
                // the agent left the user with a result or an explanation.
                if !self.unanswered_tool_failures.is_empty() {
                    let (code, message) =
                        super::diagnostics::AcpDiagnostic::SilentToolFailure.diagnostic();
                    return self.terminal("failed", json!({"code":code,"message":message}), at_ms);
                }
                ("completed", Value::Null)
            }
        };
        self.terminal(status, error, at_ms)
    }
    /// The agent answered the prompt with a JSON-RPC error: the turn ended, and failed, with the
    /// agent's own text. Only a lost or malformed answer leaves the outcome unknown.
    pub fn fail(&mut self, message: &str, at_ms: i64) -> Vec<PlannerEvent> {
        self.terminal("failed", json!({"message":message}), at_ms)
    }
    fn terminal(&mut self, status: &str, error: Value, at_ms: i64) -> Vec<PlannerEvent> {
        let mut events = Vec::new();
        if self.ended {
            return events;
        }
        self.complete_current_partial(&mut events, at_ms, status != "completed");
        self.ended = true;
        events.push(self.event(PlannerEventKind::TurnCompleted {
            turn: json!({"id":self.context.turn_id,"status":status,"error":error}),
        }));
        events
    }
    fn complete_current(&mut self, events: &mut Vec<PlannerEvent>, at_ms: i64) {
        self.complete_current_partial(events, at_ms, false);
    }
    fn complete_current_partial(
        &mut self,
        events: &mut Vec<PlannerEvent>,
        at_ms: i64,
        partial: bool,
    ) {
        if let Some(open) = self.current.take() {
            let mut event = self.item(open.materialize(), ItemPhase::Completed, at_ms);
            if partial && let PlannerEventKind::Item { params, .. } = &mut event.kind {
                params["_partial"] = json!(true);
            }
            events.push(event);
        }
    }
    /// Best-effort content observation for a caller handling disconnect/protocol failure.
    /// Does not produce TurnCompleted or decide receipt/END authority.
    pub fn materialize_partial(&mut self, at_ms: i64) -> Vec<PlannerEvent> {
        let mut events = Vec::new();
        if !self.ended {
            self.complete_current_partial(&mut events, at_ms, true);
            self.ended = true;
        }
        events
    }
    /// Flushing never seals or retransmits a complete body.
    pub fn flush(&self) -> Vec<PlannerEvent> {
        Vec::new()
    }
    fn item(&self, item: Value, phase: ItemPhase, at_ms: i64) -> PlannerEvent {
        let mut params =
            json!({"threadId":self.context.thread_id,"turnId":self.context.turn_id,"item":item});
        params[if phase == ItemPhase::Started {
            "startedAtMs"
        } else {
            "completedAtMs"
        }] = json!(at_ms);
        self.event(PlannerEventKind::Item {
            phase,
            params,
            questions: Vec::new(),
        })
    }
    fn event(&self, kind: PlannerEventKind) -> PlannerEvent {
        PlannerEvent {
            thread_id: Some(self.context.thread_id.clone()),
            kind,
        }
    }
}
