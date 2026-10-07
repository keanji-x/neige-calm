//! The supported ACP v1 contract. Optional capabilities are absent unless advertised.
use super::{Client, Error, PendingResponse};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::time::Duration;

pub const PROTOCOL_VERSION: u32 = 1;
const SETUP_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentCapabilities {
    #[serde(default)]
    pub load_session: bool,
    #[serde(default)]
    pub prompt_capabilities: PromptCapabilities,
}
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
    let value = client.request("initialize", json!({"protocolVersion":PROTOCOL_VERSION,"clientCapabilities":{},"clientInfo":{"name":name,"version":version}}), SETUP_TIMEOUT).await?;
    let response: InitializeResponse = decode(value)?;
    if response.protocol_version != PROTOCOL_VERSION {
        client.close();
        return Err(Error::Protocol("unsupported negotiated protocol version"));
    }
    Ok(response)
}
pub async fn new_session(client: &Client, cwd: &str, mcp: &[McpServer]) -> Result<String, Error> {
    validate_cwd(cwd)?;
    let response: NewSessionResponse = decode(
        client
            .request(
                "session/new",
                json!({"cwd":cwd,"mcpServers":mcp}),
                SETUP_TIMEOUT,
            )
            .await?,
    )?;
    if response.session_id.is_empty() {
        return Err(Error::Protocol("empty session identity"));
    }
    Ok(response.session_id)
}
pub async fn load_session(
    client: &Client,
    capabilities: &AgentCapabilities,
    session: &str,
    cwd: &str,
    mcp: &[McpServer],
) -> Result<Value, Error> {
    if !capabilities.load_session {
        return Err(Error::Protocol("agent does not support session/load"));
    }
    validate_cwd(cwd)?;
    client
        .request(
            "session/load",
            json!({"sessionId":session,"cwd":cwd,"mcpServers":mcp}),
            SETUP_TIMEOUT,
        )
        .await
}
pub async fn prompt(
    client: &Client,
    capabilities: &AgentCapabilities,
    session: &str,
    blocks: &[ContentBlock],
) -> Result<PendingResponse, Error> {
    if blocks.is_empty() {
        return Err(Error::Protocol("empty prompt"));
    }
    if !capabilities.prompt_capabilities.image
        && blocks
            .iter()
            .any(|block| matches!(block, ContentBlock::Image { .. }))
    {
        return Err(Error::Protocol("agent does not support image prompts"));
    }
    client
        .submit(
            "session/prompt",
            json!({"sessionId":session,"prompt":blocks}),
        )
        .await
}
pub async fn cancel(client: &Client, session: &str) -> Result<(), Error> {
    client
        .notify("session/cancel", json!({"sessionId":session}))
        .await
}
pub fn decode<T: serde::de::DeserializeOwned>(value: Value) -> Result<T, Error> {
    serde_json::from_value(value).map_err(|_| Error::Protocol("malformed method result"))
}
fn validate_cwd(cwd: &str) -> Result<(), Error> {
    if !std::path::Path::new(cwd).is_absolute() {
        return Err(Error::Protocol("cwd must be absolute"));
    }
    Ok(())
}
