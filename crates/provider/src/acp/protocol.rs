//! The supported ACP v1 contract. Optional capabilities are absent unless advertised.
use super::{Client, Error, PendingResponse, WriteFailure};
use crate::send_boundary::PreSendGate;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::time::Duration;

pub const PROTOCOL_VERSION: u32 = 1;
const SETUP_TIMEOUT: Duration = Duration::from_secs(10);
/// An agent that never answers `authenticate` (Grok Build 1.0.62 signed out) fails within this.
const AUTHENTICATE_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentCapabilities {
    #[serde(default)]
    pub load_session: bool,
    #[serde(default)]
    pub prompt_capabilities: PromptCapabilities,
    #[serde(default)]
    pub session_capabilities: SessionCapabilities,
}
#[derive(Clone, Debug, Default, Deserialize)]
pub struct SessionCapabilities {
    pub resume: Option<SessionResumeCapability>,
    pub close: Option<SessionCloseCapability>,
}
#[derive(Clone, Debug, Deserialize)]
pub struct SessionCloseCapability {}
#[derive(Clone, Debug, Deserialize)]
pub struct SessionResumeCapability {}
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PromptCapabilities {
    #[serde(default)]
    pub image: bool,
    #[serde(default)]
    pub embedded_context: bool,
}
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InitializeResponse {
    pub protocol_version: u32,
    pub agent_capabilities: AgentCapabilities,
    pub agent_info: Option<AgentInfo>,
    /// The methods `authenticate` accepts; ACP's default is none.
    #[serde(default)]
    pub auth_methods: Vec<AuthMethod>,
    /// The agent's extension metadata, uninterpreted here.
    #[serde(rename = "_meta")]
    pub meta: Option<serde_json::Map<String, Value>>,
}
#[derive(Debug, Deserialize)]
pub struct AuthMethod {
    pub id: String,
}
#[derive(Debug, Deserialize)]
pub struct AgentInfo {
    pub name: String,
    pub version: String,
}
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NewSessionResponse {
    pub session_id: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct EnvVariable {
    pub name: String,
    pub value: String,
}
#[derive(Clone, Debug, Serialize)]
#[serde(untagged)]
pub enum McpServer {
    Stdio {
        name: String,
        command: String,
        args: Vec<String>,
        env: Vec<EnvVariable>,
    },
    Http {
        r#type: HttpTransport,
        name: String,
        url: String,
        headers: Vec<Header>,
    },
}
#[derive(Clone, Debug, Serialize)]
pub struct Header {
    pub name: String,
    pub value: String,
}
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum HttpTransport {
    Http,
}

#[derive(Clone, Debug, Serialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum ContentBlock {
    Text {
        text: String,
    },
    Image {
        data: String,
        #[serde(rename = "mimeType")]
        mime_type: String,
    },
}
impl ContentBlock {
    pub fn text(text: impl Into<String>) -> Self {
        Self::Text { text: text.into() }
    }
}
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PromptResponse {
    pub stop_reason: StopReason,
}
#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StopReason {
    EndTurn,
    MaxTokens,
    MaxTurnRequests,
    Refusal,
    Cancelled,
}

pub async fn initialize(
    client: &Client,
    name: &str,
    version: &str,
) -> Result<InitializeResponse, Error> {
    initialize_with_compaction(client, name, version, false).await
}
/// Declare only capabilities the owning session consumes; no vendor policy lives here.
pub async fn initialize_with_compaction(
    client: &Client,
    name: &str,
    version: &str,
    compaction: bool,
) -> Result<InitializeResponse, Error> {
    let capabilities = if compaction {
        json!({"session":{"compaction":{}}})
    } else {
        json!({})
    };
    let value = client.request("initialize", json!({"protocolVersion":PROTOCOL_VERSION,"clientCapabilities":capabilities,"clientInfo":{"name":name,"version":version}}), SETUP_TIMEOUT).await?;
    let response: InitializeResponse = decode(value)?;
    if response.protocol_version != PROTOCOL_VERSION {
        client.close();
        return Err(Error::Protocol("unsupported negotiated protocol version"));
    }
    Ok(response)
}
/// Authenticate with `method`, one of the `authMethods` the agent reported. The caller chooses it.
pub async fn authenticate(client: &Client, method: &str) -> Result<(), Error> {
    client
        .request(
            "authenticate",
            json!({"methodId":method}),
            AUTHENTICATE_TIMEOUT,
        )
        .await
        .map(drop)
}
/// Encode native setup only. The host owns validation, authority and response pumping.
pub fn encode_new_session(cwd: &std::path::Path, mcp: &[McpServer]) -> Value {
    json!({"cwd":cwd,"mcpServers":mcp})
}
pub fn encode_load_session(session: &str, cwd: &std::path::Path, mcp: &[McpServer]) -> Value {
    json!({"sessionId":session,"cwd":cwd,"mcpServers":mcp})
}
/// Resume attaches without replay; the owning session policy selects this advertised method.
pub fn encode_resume_session(session: &str, cwd: &std::path::Path, mcp: &[McpServer]) -> Value {
    encode_load_session(session, cwd, mcp)
}
pub fn encode_set_config_option(session: &str, config_id: &str, value: &str) -> Value {
    json!({"sessionId":session,"configId":config_id,"value":value})
}
pub fn encode_close_session(session: &str) -> Value {
    json!({"sessionId":session})
}
pub async fn prompt(
    client: &Client,
    capabilities: &AgentCapabilities,
    session: &str,
    blocks: &[ContentBlock],
    gate: &PreSendGate<'_>,
) -> Result<PendingResponse, WriteFailure> {
    if blocks.is_empty() {
        return Err(WriteFailure::BeforeWrite(Error::Protocol("empty prompt")));
    }
    if !capabilities.prompt_capabilities.image
        && blocks
            .iter()
            .any(|block| matches!(block, ContentBlock::Image { .. }))
    {
        return Err(WriteFailure::BeforeWrite(Error::Protocol(
            "agent does not support image prompts",
        )));
    }
    client
        .submit_gated(
            "session/prompt",
            json!({"sessionId":session,"prompt":blocks}),
            gate,
        )
        .await
}
pub async fn cancel(
    client: &Client,
    session: &str,
    gate: &PreSendGate<'_>,
) -> Result<(), WriteFailure> {
    client
        .notify_gated("session/cancel", json!({"sessionId":session}), gate)
        .await
}
pub fn decode<T: serde::de::DeserializeOwned>(value: Value) -> Result<T, Error> {
    serde_json::from_value(value).map_err(|_| Error::Protocol("malformed method result"))
}
