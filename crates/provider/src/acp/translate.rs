//! Pure ACP updates → PlannerEvent translation. Native tool names remain display metadata.
use super::{Error, protocol::StopReason};
use crate::events::{ItemPhase, PlannerEvent, PlannerEventKind};
use serde_json::{Value, json};
use std::collections::BTreeMap;

pub struct TurnContext {
    pub thread_id: String,
    pub turn_id: String,
    pub native_session_id: String,
}
pub struct TurnTranslator {
    context: TurnContext,
    text: BTreeMap<String, (String, String)>,
    tools: BTreeMap<String, Value>,
    sequence: u64,
    current: Option<(String, String)>,
    text_bytes: usize,
}
impl TurnTranslator {
    pub fn new(context: TurnContext) -> Self {
        Self {
            context,
            text: BTreeMap::new(),
            tools: BTreeMap::new(),
            sequence: 0,
            current: None,
            text_bytes: 0,
        }
    }
    pub fn started(&self) -> PlannerEvent {
        self.event(PlannerEventKind::TurnStarted {
            turn_id: self.context.turn_id.clone(),
        })
    }
    pub fn update(&mut self, params: &Value, at_ms: i64) -> Result<Vec<PlannerEvent>, Error> {
        if params["sessionId"].as_str() != Some(&self.context.native_session_id) {
            return Err(Error::Protocol("update session identity mismatch"));
        }
        let update = &params["update"];
        let kind = update["sessionUpdate"]
            .as_str()
            .ok_or(Error::Protocol("missing session update kind"))?;
        let mut events = Vec::new();
        match kind {
            "agent_message_chunk" | "agent_thought_chunk" => {
                let content = &update["content"];
                let content_type = content["type"]
                    .as_str()
                    .ok_or(Error::Protocol("missing content type"))?;
                if content_type != "text" {
                    return Ok(events);
                }
                let delta = content["text"]
                    .as_str()
                    .ok_or(Error::Protocol("missing text chunk"))?;
                let native = update.get("messageId").and_then(Value::as_str);
                let id = if let Some(id) = native {
                    format!("acp-{}-{kind}-{id}", self.context.turn_id)
                } else if let Some((channel, id)) = &self.current {
                    if channel == kind {
                        id.clone()
                    } else {
                        self.sequence += 1;
                        format!("acp-{}-{}", self.context.turn_id, self.sequence)
                    }
                } else {
                    self.sequence += 1;
                    format!("acp-{}-{}", self.context.turn_id, self.sequence)
                };
                self.current = Some((kind.into(), id.clone()));
                self.text_bytes = self.text_bytes.saturating_add(delta.len());
                if self.text_bytes > 16 * 1024 * 1024 {
                    return Err(Error::Protocol("turn text exceeds byte limit"));
                }
                let first = !self.text.contains_key(&id);
                let entry = self
                    .text
                    .entry(id.clone())
                    .or_insert_with(|| (kind.into(), String::new()));
                entry.1.push_str(delta);
                let item = if kind == "agent_message_chunk" {
                    json!({"id":id,"type":"agentMessage","text":""})
                } else {
                    json!({"id":id,"type":"reasoning","summary":[entry.1],"content":[]})
                };
                if first || kind == "agent_thought_chunk" {
                    events.push(self.item(item, ItemPhase::Started, at_ms));
                }
                if kind == "agent_message_chunk" {
                    events.push(self.event(PlannerEventKind::ReplyDelta {
                        turn_id: self.context.turn_id.clone(),
                        item_id: id,
                        delta: delta.into(),
                    }));
                }
            }
            "tool_call" | "tool_call_update" => {
                self.current = None;
                let id = update["toolCallId"]
                    .as_str()
                    .ok_or(Error::Protocol("missing tool call identity"))?;
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
                for field in [
                    "title",
                    "kind",
                    "status",
                    "rawInput",
                    "rawOutput",
                    "content",
                    "locations",
                    "_meta",
                ] {
                    if let Some(value) = update.get(field) {
                        tool[field] = value.clone();
                    }
                }
                let status = match tool["status"].as_str() {
                    Some("pending" | "in_progress") => "inProgress",
                    Some("completed") => "completed",
                    Some("failed") => "failed",
                    _ => return Err(Error::Protocol("invalid tool call status")),
                };
                let content = tool_content(&tool)?;
                let item = json!({"id":format!("acp-{}-{id}",self.context.turn_id),"type":"dynamicToolCall","tool":tool["title"],"arguments":tool.get("rawInput").cloned().unwrap_or(Value::Null),"status":status,"result":{"content":content,"structuredContent":tool.get("rawOutput").cloned().unwrap_or(Value::Null)},"native":tool});
                let phase = if matches!(status, "completed" | "failed") {
                    ItemPhase::Completed
                } else {
                    ItemPhase::Started
                };
                events.push(self.item(item, phase, at_ms));
                if self.tools.len() >= 4096 && !self.tools.contains_key(id) {
                    return Err(Error::Protocol("turn tool count exceeds limit"));
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
    pub fn finish(&self, reason: StopReason, at_ms: i64) -> Vec<PlannerEvent> {
        let mut events = Vec::new();
        for (id, (kind, text)) in &self.text {
            let item = if kind == "agent_message_chunk" {
                json!({"id":id,"type":"agentMessage","text":text})
            } else {
                json!({"id":id,"type":"reasoning","summary":[text],"content":[]})
            };
            events.push(self.item(item, ItemPhase::Completed, at_ms));
        }
        let (status, error) = match reason {
            StopReason::Cancelled => ("interrupted", Value::Null),
            StopReason::Refusal => ("failed", json!({"message":"ACP agent refused the turn"})),
            StopReason::EndTurn | StopReason::MaxTokens | StopReason::MaxTurnRequests => {
                ("completed", Value::Null)
            }
        };
        events.push(self.event(PlannerEventKind::TurnCompleted {
            turn: json!({"id":self.context.turn_id,"status":status,"error":error}),
        }));
        events
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

fn tool_content(tool: &Value) -> Result<Vec<Value>, Error> {
    let Some(content) = tool.get("content") else {
        return Ok(Vec::new());
    };
    let content = content
        .as_array()
        .ok_or(Error::Protocol("invalid tool content"))?;
    let mut output = Vec::new();
    for part in content {
        match part["type"].as_str() {
            Some("content") => {
                if !part["content"].is_object() || !part["content"]["type"].is_string() {return Err(Error::Protocol("invalid tool content block"));}
                output.push(part["content"].clone());
            },
            Some("diff") => {
                let path=part["path"].as_str().ok_or(Error::Protocol("diff has no path"))?;
                let new=part["newText"].as_str().ok_or(Error::Protocol("diff has no new text"))?;
                let old=part["oldText"].as_str().unwrap_or("");
                output.push(json!({"type":"text","text":format!("{path}\n---\n{old}\n+++\n{new}")}));
            },
            Some("terminal") => {
                let id=part["terminalId"].as_str().ok_or(Error::Protocol("terminal content has no identity"))?;
                output.push(json!({"type":"text","text":format!("Agent terminal: {id}")}));
            },
            Some(_) => output.push(json!({"type":"text","text":"The agent returned an ACP content extension; its native record is retained."})),
            None => return Err(Error::Protocol("tool content has no kind")),
        }
    }
    Ok(output)
}
