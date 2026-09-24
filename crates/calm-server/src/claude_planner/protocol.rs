//! The `claude -p --input-format stream-json --output-format stream-json` wire.
//!
//! Inbound, every stdout line decodes to one [`Record`]: `type` is read first, then `subtype` or the
//! content kind, and only then the variant's fields. An unknown discriminant is [`Record::Ignored`],
//! never an error; a known discriminant whose fields do not match the recorded shape is a
//! [`ProtocolError`]. Outbound, the stdin lines neige writes: [`UserLine`], [`ControlRequestOut`],
//! [`ControlResponseOut`].

use std::collections::BTreeMap;

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

#[derive(Debug, thiserror::Error)]
pub enum ProtocolError {
    #[error("claude stream line is not JSON: {0}")]
    NotJson(serde_json::Error),
    #[error("claude stream line is not an object with a string `type`")]
    NoType,
    #[error("claude `{kind}` record has no string `subtype`")]
    NoSubtype { kind: String },
    #[error("claude `{kind}` record does not have its recorded shape: {source}")]
    Shape {
        kind: String,
        source: serde_json::Error,
    },
    #[error("client id `{0}` is not 32 hex digits")]
    ClientId(String),
}

/// One decoded stdout line.
#[derive(Debug, Clone, PartialEq)]
pub enum Record {
    SystemInit(SystemInit),
    /// The CLI's echo of a line we wrote (`isReplay: true`), carrying the line's `uuid`.
    UserReplay {
        uuid: Uuid,
        message: UserMessage,
    },
    /// A user-role text record the CLI writes itself, e.g. `[Request interrupted by user]`.
    UserText {
        uuid: Uuid,
        text: String,
    },
    UserToolResults {
        uuid: Uuid,
        results: Vec<ToolResult>,
        /// The CLI's structured view of the result; its shape depends on the tool.
        tool_use_result: Value,
    },
    Assistant {
        uuid: Uuid,
        blocks: Vec<AssistantBlock>,
    },
    ResultSuccess(ResultSuccess),
    ResultError(ResultError),
    ControlResponseIn {
        response: ControlResponseBody,
    },
    ControlRequestIn {
        request_id: String,
        request: Value,
    },
    Ignored {
        kind: String,
    },
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct SystemInit {
    pub session_id: Uuid,
    pub claude_code_version: String,
    pub model: String,
    pub capabilities: Vec<String>,
    pub mcp_servers: Vec<McpStatus>,
    pub skills: Vec<String>,
    pub plugins: Vec<PluginRef>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct McpStatus {
    pub name: String,
    pub status: String,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct PluginRef {
    pub name: String,
    pub source: String,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct UserMessage {
    pub content: UserContent,
}

/// A user message's content: the CLI echoes a plain-string line as a string.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(untagged)]
pub enum UserContent {
    Text(String),
    Blocks(Vec<UserBlock>),
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum UserBlock {
    Text {
        text: String,
    },
    /// Fields are not decoded: the base64 payload has no reader on our side.
    Image {},
    ToolResult(ToolResult),
    #[serde(other)]
    Other,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct ToolResult {
    pub tool_use_id: String,
    pub content: ToolResultContent,
    /// The wire omits the member on a successful result.
    #[serde(default)]
    pub is_error: bool,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(untagged)]
pub enum ToolResultContent {
    Text(String),
    Blocks(Vec<ToolResultBlock>),
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ToolResultBlock {
    Text {
        text: String,
    },
    Image {},
    /// `tool_reference`, and any block kind added later.
    #[serde(other)]
    Other,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AssistantBlock {
    Thinking {
        thinking: String,
    },
    Text {
        text: String,
    },
    ToolUse {
        id: String,
        name: String,
        input: Value,
    },
    #[serde(other)]
    Other,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct ResultSuccess {
    pub is_error: bool,
    pub result: String,
    pub usage: Usage,
    #[serde(rename = "modelUsage")]
    pub model_usage: BTreeMap<String, ModelUsage>,
    pub terminal_reason: String,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Usage {
    pub input_tokens: i64,
    pub cache_creation_input_tokens: i64,
    pub cache_read_input_tokens: i64,
    pub output_tokens: i64,
    /// One entry per model response of the turn's last request; empty when no response arrived.
    pub iterations: Vec<UsageIteration>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct UsageIteration {
    pub input_tokens: i64,
    pub cache_creation_input_tokens: i64,
    pub cache_read_input_tokens: i64,
    pub output_tokens: i64,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct ModelUsage {
    #[serde(rename = "contextWindow")]
    pub context_window: i64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ResultError {
    pub subtype: ErrorSubtype,
    pub errors: Vec<String>,
    /// Absent when the turn fails before it starts (sandbox unavailable).
    pub terminal_reason: Option<String>,
    /// Non-empty `iterations` when responses arrived before the turn failed (SIGINT, P-F1).
    pub usage: Usage,
    pub model_usage: BTreeMap<String, ModelUsage>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ErrorSubtype {
    ErrorDuringExecution,
    ErrorMaxTurns,
    ErrorMaxBudgetUsd,
    ErrorMaxStructuredOutputRetries,
    Other(String),
}

impl ErrorSubtype {
    fn from_wire(subtype: &str) -> Self {
        match subtype {
            "error_during_execution" => Self::ErrorDuringExecution,
            "error_max_turns" => Self::ErrorMaxTurns,
            "error_max_budget_usd" => Self::ErrorMaxBudgetUsd,
            "error_max_structured_output_retries" => Self::ErrorMaxStructuredOutputRetries,
            other => Self::Other(other.to_string()),
        }
    }
}

/// The body of an inbound `control_response`; `response` is absent when a request has no answer
/// payload (e.g. `set_model`).
#[derive(Debug, Clone, PartialEq)]
pub enum ControlResponseBody {
    Success {
        request_id: String,
        response: Option<Value>,
    },
    Error {
        request_id: String,
        error: String,
    },
}

/// Decode one stdout line.
pub fn decode(line: &str) -> Result<Record, ProtocolError> {
    let value: Value = serde_json::from_str(line).map_err(ProtocolError::NotJson)?;
    let kind = value
        .get("type")
        .and_then(Value::as_str)
        .ok_or(ProtocolError::NoType)?
        .to_string();
    let subtype = value
        .get("subtype")
        .and_then(Value::as_str)
        .map(str::to_string);
    match (kind.as_str(), subtype.as_deref()) {
        ("system", Some("init")) => Ok(Record::SystemInit(shape("system/init", value)?)),
        ("system", Some(other)) => Ok(Record::Ignored {
            kind: format!("system/{other}"),
        }),
        ("system" | "result", None) => Err(ProtocolError::NoSubtype { kind }),
        ("user", _) => decode_user(value),
        ("assistant", _) => {
            let wire: WireAssistant = shape("assistant", value)?;
            Ok(Record::Assistant {
                uuid: wire.uuid,
                blocks: wire.message.content,
            })
        }
        ("result", Some("success")) => Ok(Record::ResultSuccess(shape("result/success", value)?)),
        ("result", Some(other)) => {
            let wire: WireResultError = shape("result", value)?;
            Ok(Record::ResultError(ResultError {
                subtype: ErrorSubtype::from_wire(other),
                errors: wire.errors,
                terminal_reason: wire.terminal_reason,
                usage: wire.usage,
                model_usage: wire.model_usage,
            }))
        }
        ("control_response", _) => decode_control_response(value),
        ("control_request", _) => {
            let wire: WireControlRequest = shape("control_request", value)?;
            Ok(Record::ControlRequestIn {
                request_id: wire.request_id,
                request: wire.request,
            })
        }
        _ => Ok(Record::Ignored { kind }),
    }
}

fn shape<T: DeserializeOwned>(kind: &str, value: Value) -> Result<T, ProtocolError> {
    serde_json::from_value(value).map_err(|source| ProtocolError::Shape {
        kind: kind.to_string(),
        source,
    })
}

#[derive(Deserialize)]
struct WireUser {
    uuid: Uuid,
    message: UserMessage,
    #[serde(rename = "isReplay", default)]
    is_replay: bool,
    tool_use_result: Option<Value>,
}

/// A replay is ours by definition; otherwise tool results win over text, text over nothing.
fn decode_user(value: Value) -> Result<Record, ProtocolError> {
    let wire: WireUser = shape("user", value)?;
    if wire.is_replay {
        return Ok(Record::UserReplay {
            uuid: wire.uuid,
            message: wire.message,
        });
    }
    let blocks = match wire.message.content {
        UserContent::Text(text) => {
            return Ok(Record::UserText {
                uuid: wire.uuid,
                text,
            });
        }
        UserContent::Blocks(blocks) => blocks,
    };
    let mut results = Vec::new();
    let mut texts = Vec::new();
    for block in blocks {
        match block {
            UserBlock::ToolResult(result) => results.push(result),
            UserBlock::Text { text } => texts.push(text),
            UserBlock::Image {} | UserBlock::Other => {}
        }
    }
    if !results.is_empty() {
        let tool_use_result = wire.tool_use_result.ok_or_else(|| ProtocolError::Shape {
            kind: "user/tool_result".to_string(),
            source: serde::de::Error::missing_field("tool_use_result"),
        })?;
        return Ok(Record::UserToolResults {
            uuid: wire.uuid,
            results,
            tool_use_result,
        });
    }
    if !texts.is_empty() {
        return Ok(Record::UserText {
            uuid: wire.uuid,
            text: texts.join("\n"),
        });
    }
    Ok(Record::Ignored {
        kind: "user".to_string(),
    })
}

#[derive(Deserialize)]
struct WireAssistant {
    uuid: Uuid,
    message: WireAssistantMessage,
}

#[derive(Deserialize)]
struct WireAssistantMessage {
    content: Vec<AssistantBlock>,
}

#[derive(Deserialize)]
struct WireResultError {
    errors: Vec<String>,
    terminal_reason: Option<String>,
    usage: Usage,
    #[serde(rename = "modelUsage")]
    model_usage: BTreeMap<String, ModelUsage>,
}

#[derive(Deserialize)]
struct WireControlRequest {
    request_id: String,
    request: Value,
}

#[derive(Deserialize)]
struct WireControlResponse {
    response: WireControlResponseBody,
}

#[derive(Deserialize)]
#[serde(tag = "subtype", rename_all = "snake_case")]
enum WireControlResponseBody {
    Success {
        request_id: String,
        response: Option<Value>,
    },
    Error {
        request_id: String,
        error: String,
    },
}

fn decode_control_response(value: Value) -> Result<Record, ProtocolError> {
    let subtype = value
        .get("response")
        .and_then(|response| response.get("subtype"))
        .and_then(Value::as_str);
    match subtype {
        None => {
            return Err(ProtocolError::NoSubtype {
                kind: "control_response".to_string(),
            });
        }
        Some("success" | "error") => {}
        Some(other) => {
            return Ok(Record::Ignored {
                kind: format!("control_response/{other}"),
            });
        }
    }
    let wire: WireControlResponse = shape("control_response", value)?;
    let response = match wire.response {
        WireControlResponseBody::Success {
            request_id,
            response,
        } => ControlResponseBody::Success {
            request_id,
            response,
        },
        WireControlResponseBody::Error { request_id, error } => {
            ControlResponseBody::Error { request_id, error }
        }
    };
    Ok(Record::ControlResponseIn { response })
}

/// The `uuid` of the stdin line for a harness client id: the dashed UUID of the same 128 bits, so the
/// CLI's replay (`isReplay`) names the client id it echoes.
pub fn client_line_uuid(client_id: &str) -> Result<Uuid, ProtocolError> {
    if client_id.len() != 32 || !client_id.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(ProtocolError::ClientId(client_id.to_string()));
    }
    Uuid::try_parse(client_id).map_err(|_| ProtocolError::ClientId(client_id.to_string()))
}

/// `{"type":"user","message":{"role":"user","content":[…]},"parent_tool_use_id":null,"session_id","uuid"}`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct UserLine {
    #[serde(rename = "type")]
    kind: &'static str,
    message: UserLineMessage,
    /// Always `null`: neige writes only top-level lines.
    parent_tool_use_id: (),
    session_id: Uuid,
    uuid: Uuid,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
struct UserLineMessage {
    role: &'static str,
    content: Vec<UserLineContent>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum UserLineContent {
    Text { text: String },
    Image { source: Base64Image },
}

/// `{"type":"base64","media_type","data"}`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Base64Image {
    #[serde(rename = "type")]
    kind: &'static str,
    media_type: String,
    data: String,
}

impl Base64Image {
    pub fn new(media_type: impl Into<String>, data: impl Into<String>) -> Self {
        Self {
            kind: "base64",
            media_type: media_type.into(),
            data: data.into(),
        }
    }
}

impl UserLine {
    pub fn new(session_id: Uuid, uuid: Uuid, content: Vec<UserLineContent>) -> Self {
        Self {
            kind: "user",
            message: UserLineMessage {
                role: "user",
                content,
            },
            parent_tool_use_id: (),
            session_id,
            uuid,
        }
    }
}

/// `{"type":"control_request","request_id","request":{"subtype":…}}`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ControlRequestOut {
    #[serde(rename = "type")]
    kind: &'static str,
    request_id: String,
    request: ControlRequestKind,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "subtype", rename_all = "snake_case")]
pub enum ControlRequestKind {
    Interrupt,
}

impl ControlRequestOut {
    pub fn new(request_id: impl Into<String>, request: ControlRequestKind) -> Self {
        Self {
            kind: "control_request",
            request_id: request_id.into(),
            request,
        }
    }
}

/// `{"type":"control_response","response":{"subtype":"success"|"error","request_id",…}}`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ControlResponseOut {
    #[serde(rename = "type")]
    kind: &'static str,
    response: ControlResponseOutBody,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "subtype", rename_all = "snake_case")]
pub enum ControlResponseOutBody {
    Success { request_id: String, response: Value },
    Error { request_id: String, error: String },
}

impl ControlResponseOut {
    pub fn new(response: ControlResponseOutBody) -> Self {
        Self {
            kind: "control_response",
            response,
        }
    }
}
