//! #1164 §2.2 — the `mcp-http` connector's client.
//!
//! The target class of server (probed 2026-08-31 against
//! `https://mcp.wisburg.com/mcp`, protocol `2025-06-18`) is *stateless
//! streamable-HTTP*: it never returns `Mcp-Session-Id`, and a `tools/list`
//! answer is a single `event: message` frame with exactly one `data:` line
//! before the stream ends. So the transport here is deliberately tiny:
//!
//! > POST one JSON body → read the whole response body → strip a leading
//! > `data: ` if present → parse as a JSON-RPC response.
//!
//! No session handling, no server→client GET stream, no resumption. When a
//! real long-lived stream becomes necessary this module is the seam to
//! replace; nothing else in the kernel knows the shape.
//!
//! **Deliberate omissions in the minimal `initialize`** (§1.4 + D10):
//!
//! * no `_meta["dev.neige/auth"]` — external servers were never issued a
//!   kernel plugin token and must not be handed one;
//! * no `experimental.dev.neige/kernel-callbacks` — the kernel does not build
//!   an inbound router for connectors, so advertising the capability would be
//!   a lie the server could act on;
//! * no protocol-version comparison — "the set of versions the kernel knows"
//!   does not exist in this codebase (only `KERNEL_PROTOCOL_VERSION`), so v0
//!   records the server's version in a log line and moves on.
//!
//! **Timeouts are a correctness requirement, not a nicety.** `tools/list`
//! runs inside `spawn_admitted`, which `AppState::new` awaits inline via
//! `autospawn_enabled`. An upstream that accepts the connection and then
//! never answers would otherwise hang the entire server boot. Both the
//! connect and the read deadline are therefore set explicitly.

use std::sync::Arc;
use std::time::Duration;

use serde_json::{Value, json};

use super::manifest::{ApiKeyIn, McpHttpBlock};
use super::mcp::RpcError;

/// SSE data-line prefix. The probed server frames its single JSON-RPC
/// response as `event: message\ndata: {...}\n\n`.
const SSE_DATA_PREFIX: &str = "data:";

/// Cap on the response body we will buffer. A `tools/list` for 13 tools is a
/// few tens of KiB; a megabyte is generous and still bounded.
const MAX_BODY_BYTES: usize = 4 * 1024 * 1024;

/// One remote streamable-HTTP MCP server.
///
/// Cheap to construct, `Send + Sync`, and held behind an `Arc` inside
/// [`super::ConnectorClient`] so the process-table lock can clone it out
/// without being held across an await.
#[derive(Debug)]
pub struct HttpMcpClient {
    plugin_id: String,
    /// Endpoint with the API key already appended when `api_key_in` is
    /// `query:<name>`. Never logged — see [`Self::log_target`].
    url: String,
    /// `(name, value)` when `api_key_in` is `header:<name>`.
    header_auth: Option<(String, String)>,
    /// Host only, for the per-call audit line required by risk R2.
    log_target: String,
    timeout: Duration,
    next_id: std::sync::atomic::AtomicU64,
}

impl HttpMcpClient {
    /// Build a client from a validated `mcp_http` block plus the connector's
    /// resolved secret value (`None` when the block declares no key).
    ///
    /// The key is folded into the URL / header here, once, so no call site can
    /// forget it and no code path logs the assembled URL.
    pub fn new(plugin_id: &str, block: &McpHttpBlock, api_key: Option<&str>) -> Self {
        let base = block.url.trim().to_string();
        let mut url = base.clone();
        let mut header_auth = None;

        if let Some(key) = api_key {
            match block.api_key_in_parsed() {
                Some(ApiKeyIn::Query(name)) => {
                    let sep = if base.contains('?') { '&' } else { '?' };
                    url = format!(
                        "{base}{sep}{}={}",
                        percent_encode(&name),
                        percent_encode(key)
                    );
                }
                Some(ApiKeyIn::Header(name)) => {
                    header_auth = Some((name, key.to_string()));
                }
                // `Manifest::validate` rejects this combination; treat a
                // future regression as "send nothing" rather than leaking the
                // key into an unexpected slot.
                None => {}
            }
        }

        Self {
            plugin_id: plugin_id.to_string(),
            log_target: log_target(&base),
            url,
            header_auth,
            timeout: Duration::from_millis(block.timeout_ms()),
            next_id: std::sync::atomic::AtomicU64::new(1),
        }
    }

    /// Host (and port) of the endpoint — the only part of the URL that is
    /// safe to log, since the API key may ride in the query string.
    pub fn log_target(&self) -> &str {
        &self.log_target
    }

    /// Minimal `initialize`. Best-effort: a server that does not implement it
    /// (the probed one answers `tools/list` without a handshake) must not
    /// block the connector from coming up, so the caller treats an error here
    /// as informational.
    pub async fn initialize(&self) -> Result<Value, RpcError> {
        let params = json!({
            "protocolVersion": super::mcp::KERNEL_PROTOCOL_VERSION,
            "capabilities": {},
            "clientInfo": { "name": "neige-kernel", "version": env!("CARGO_PKG_VERSION") },
        });
        let result = self.request("initialize", params).await?;
        let server_version = result
            .get("protocolVersion")
            .and_then(|v| v.as_str())
            .unwrap_or("<unset>");
        let server_name = result
            .pointer("/serverInfo/name")
            .and_then(|v| v.as_str())
            .unwrap_or("<unset>");
        // v0 records, never compares (§1.4): there is no kernel-side set of
        // known-good external protocol versions to compare against.
        tracing::info!(
            plugin_id = %self.plugin_id,
            target = %self.log_target,
            server_protocol_version = %server_version,
            server_name = %server_name,
            "mcp-http connector initialized"
        );
        Ok(result)
    }

    /// `tools/list`, returning the raw `tools` array entries.
    pub async fn tools_list(&self) -> Result<Vec<Value>, RpcError> {
        let result = self.request("tools/list", json!({})).await?;
        let tools = result
            .get("tools")
            .and_then(|t| t.as_array())
            .cloned()
            .unwrap_or_default();
        Ok(tools)
    }

    /// `tools/call`, parsed into the same envelope stdio plugins return so the
    /// dispatch arm in `mcp_server::transport` is variant-agnostic.
    pub async fn tools_call(
        &self,
        name: &str,
        arguments: Value,
    ) -> Result<super::mcp::CallToolResult, RpcError> {
        // Risk R2 — every outbound tool call records its target host. The URL
        // itself is never logged because the API key may be in the query.
        tracing::info!(
            plugin_id = %self.plugin_id,
            target = %self.log_target,
            tool = %name,
            "mcp-http connector tools/call"
        );
        let raw = self
            .request(
                "tools/call",
                json!({ "name": name, "arguments": arguments }),
            )
            .await?;
        serde_json::from_value(raw).map_err(|e| {
            RpcError::internal(format!(
                "mcp-http tools/call: response did not parse as CallToolResult: {e}"
            ))
        })
    }

    /// One JSON-RPC round trip.
    ///
    /// The blocking HTTP call is moved onto the blocking pool so the async
    /// runtime keeps turning; the deadline is enforced by the client itself,
    /// so the blocking task cannot outlive `timeout` by more than scheduling
    /// jitter.
    pub async fn request(&self, method: &str, params: Value) -> Result<Value, RpcError> {
        let id = self
            .next_id
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let body = json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        })
        .to_string();

        let url = self.url.clone();
        let header_auth = self.header_auth.clone();
        let timeout = self.timeout;
        let method_owned = method.to_string();
        let target = self.log_target.clone();

        let text = tokio::task::spawn_blocking(move || {
            let agent = ureq::AgentBuilder::new()
                // Both halves matter: `timeout_connect` bounds a black-holed
                // SYN, `timeout` bounds a server that accepts and then stalls.
                .timeout_connect(timeout)
                .timeout(timeout)
                .build();
            let mut req = agent
                .post(&url)
                .set("content-type", "application/json")
                // The probed server answers `text/event-stream`; accepting
                // both means a plain-JSON server works unchanged.
                .set("accept", "application/json, text/event-stream");
            if let Some((name, value)) = header_auth.as_ref() {
                req = req.set(name, value);
            }
            match req.send_string(&body) {
                Ok(resp) => resp
                    .into_string()
                    .map_err(|e| format!("reading response body failed: {e}")),
                Err(ureq::Error::Status(code, resp)) => {
                    let detail = resp.into_string().unwrap_or_default();
                    let detail: String = detail.chars().take(512).collect();
                    Err(format!("HTTP {code} from {target}: {detail}"))
                }
                Err(e) => Err(format!("request to {target} failed: {e}")),
            }
        })
        .await
        .map_err(|e| RpcError::internal(format!("mcp-http request task failed: {e}")))?
        .map_err(|e| RpcError::custom(-32002, format!("mcp-http {method_owned}: {e}")))?;

        if text.len() > MAX_BODY_BYTES {
            return Err(RpcError::internal(format!(
                "mcp-http {method_owned}: response body exceeds {MAX_BODY_BYTES} bytes"
            )));
        }

        let payload = strip_sse_envelope(&text).ok_or_else(|| {
            RpcError::internal(format!(
                "mcp-http {method_owned}: response carried no JSON payload"
            ))
        })?;
        let parsed: Value = serde_json::from_str(payload).map_err(|e| {
            RpcError::internal(format!("mcp-http {method_owned}: malformed JSON: {e}"))
        })?;

        if let Some(err) = parsed.get("error")
            && !err.is_null()
        {
            let code = err.get("code").and_then(|c| c.as_i64()).unwrap_or(-32603);
            let message = err
                .get("message")
                .and_then(|m| m.as_str())
                .unwrap_or("upstream error")
                .to_string();
            return Err(RpcError::custom(code, message));
        }
        parsed.get("result").cloned().ok_or_else(|| {
            RpcError::internal(format!(
                "mcp-http {method_owned}: response had neither `result` nor `error`"
            ))
        })
    }
}

/// Extract the JSON payload from either a bare JSON body or a single-frame
/// SSE body (`event: message` + one `data:` line).
///
/// Returns `None` when nothing payload-shaped is present. Multi-line `data:`
/// frames are out of scope for v0 — the probed server emits exactly one.
pub fn strip_sse_envelope(body: &str) -> Option<&str> {
    let trimmed = body.trim();
    if trimmed.is_empty() {
        return None;
    }
    if trimmed.starts_with('{') || trimmed.starts_with('[') {
        return Some(trimmed);
    }
    for line in trimmed.lines() {
        let line = line.trim_start();
        if let Some(rest) = line.strip_prefix(SSE_DATA_PREFIX) {
            let payload = rest.trim();
            if !payload.is_empty() {
                return Some(payload);
            }
        }
    }
    None
}

/// `scheme://host[:port]`, dropping path/query so a query-string API key can
/// never reach a log line.
fn log_target(url: &str) -> String {
    let (scheme, rest) = match url.split_once("://") {
        Some((s, r)) => (s, r),
        None => return "<unparsed>".to_string(),
    };
    let authority = rest
        .split(['/', '?', '#'])
        .next()
        .unwrap_or_default()
        .to_string();
    format!("{scheme}://{authority}")
}

/// Minimal percent-encoding for query-string values. We only need the
/// characters that would break a query parameter; `url` is not a dependency of
/// this crate and pulling one for eight bytes of logic is not worth it.
fn percent_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for byte in s.as_bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(*byte as char)
            }
            other => out.push_str(&format!("%{other:02X}")),
        }
    }
    out
}

/// Convenience alias so callers can hold the client without importing `Arc`
/// plumbing at every site.
pub type SharedHttpMcpClient = Arc<HttpMcpClient>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sse_envelope_stripped() {
        let body = "event: message\ndata: {\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{}}\n\n";
        assert_eq!(
            strip_sse_envelope(body),
            Some("{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{}}")
        );
    }

    #[test]
    fn bare_json_passes_through() {
        assert_eq!(strip_sse_envelope("  {\"a\":1}  "), Some("{\"a\":1}"));
    }

    #[test]
    fn empty_or_payloadless_body_is_none() {
        assert_eq!(strip_sse_envelope(""), None);
        assert_eq!(strip_sse_envelope("event: message\n\n"), None);
        assert_eq!(strip_sse_envelope("data:\n"), None);
    }

    #[test]
    fn log_target_drops_path_and_query() {
        assert_eq!(
            log_target("https://mcp.example.com/mcp?api_key=sk-secret"),
            "https://mcp.example.com"
        );
        assert_eq!(
            log_target("http://127.0.0.1:8931/x"),
            "http://127.0.0.1:8931"
        );
    }

    #[test]
    fn query_key_is_appended_and_encoded_and_never_in_log_target() {
        let block: McpHttpBlock = serde_json::from_value(serde_json::json!({
            "url": "https://mcp.example.com/mcp",
            "api_key_secret": "K",
            "api_key_in": "query:api_key",
        }))
        .unwrap();
        let client = HttpMcpClient::new("c", &block, Some("sk a/b"));
        assert_eq!(client.url, "https://mcp.example.com/mcp?api_key=sk%20a%2Fb");
        assert!(!client.log_target().contains("sk"));
        assert!(client.header_auth.is_none());
    }

    #[test]
    fn existing_query_string_gets_ampersand() {
        let block: McpHttpBlock = serde_json::from_value(serde_json::json!({
            "url": "https://mcp.example.com/mcp?v=1",
            "api_key_secret": "K",
            "api_key_in": "query:api_key",
        }))
        .unwrap();
        let client = HttpMcpClient::new("c", &block, Some("abc"));
        assert_eq!(client.url, "https://mcp.example.com/mcp?v=1&api_key=abc");
    }

    #[test]
    fn header_key_does_not_touch_the_url() {
        let block: McpHttpBlock = serde_json::from_value(serde_json::json!({
            "url": "https://mcp.example.com/mcp",
            "api_key_secret": "K",
            "api_key_in": "header:x-api-key",
        }))
        .unwrap();
        let client = HttpMcpClient::new("c", &block, Some("abc"));
        assert_eq!(client.url, "https://mcp.example.com/mcp");
        assert_eq!(
            client.header_auth,
            Some(("x-api-key".to_string(), "abc".to_string()))
        );
    }
}
