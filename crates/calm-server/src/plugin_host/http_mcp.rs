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
//! connect and the read deadline are therefore set explicitly — but they are
//! PER-REQUEST, not a total bound: `initialize` and `tools/list` are two round
//! trips, and `spawn_blocking` queue delay plus DNS sit outside ureq's own
//! clock. The total wall-clock bound is the single `tokio::time::timeout`
//! around the whole connector bring-up in `PluginHost::spawn_mcp_http`.
//!
//! **The API key must never reach a string a human or an agent can read.** It
//! rides in the URL's query string, and `ureq::Error`'s `Display` prints that
//! URL first — so a `{e}` anywhere in this module puts the credential into the
//! `tracing` line, the persisted+broadcast `Event::PluginState.last_error`, the
//! `POST /enable` 503 body, and the wave transcript. Two rules, both enforced
//! by tests: format a `ureq::Error` only via `kind()`, and run every outgoing
//! error string through [`HttpMcpClient::scrub`].

use std::io::Read as _;
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
///
/// **No `#[derive(Debug)]`.** A derived `Debug` prints `url` (which carries the
/// API key when `api_key_in` is `query:<name>`) and `header_auth` (name AND
/// value), which would defeat the hand-written redacting `Debug` on
/// [`super::ConnectorClient`] the moment anything formatted the inner client.
pub struct HttpMcpClient {
    plugin_id: String,
    /// Endpoint with the API key already appended when `api_key_in` is
    /// `query:<name>`. Never logged — see [`Self::log_target`].
    url: String,
    /// `(name, value)` when `api_key_in` is `header:<name>`.
    header_auth: Option<(String, String)>,
    /// Host only, for the per-call audit line required by risk R2.
    log_target: String,
    /// Every literal form the secret takes on the wire (raw + percent-encoded).
    /// [`Self::scrub`] strips these from any string that could reach a log
    /// line, an `Event::PluginState.last_error`, an HTTP body, or a wave
    /// transcript. This is the belt to the "never format a `ureq::Error`"
    /// braces: an *upstream* 4xx body may quote the query string back at us,
    /// and that path is not ours to control.
    secret_forms: Vec<String>,
    /// Built once (§ review finding 9): a fresh `ureq::Agent` re-parses the
    /// ~150-certificate webpki root store and gives up all connection/TLS
    /// reuse, per `tools/call`.
    agent: ureq::Agent,
    next_id: std::sync::atomic::AtomicU64,
}

impl std::fmt::Debug for HttpMcpClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HttpMcpClient")
            .field("plugin_id", &self.plugin_id)
            .field("target", &self.log_target)
            .field(
                "auth",
                &match (&self.header_auth, self.secret_forms.is_empty()) {
                    (Some((name, _)), _) => format!("header:{name}=<redacted>"),
                    (None, false) => "query:<redacted>".to_string(),
                    (None, true) => "none".to_string(),
                },
            )
            .finish_non_exhaustive()
    }
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
        let mut secret_forms = Vec::new();

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
            // Both literal forms, longest first, so scrubbing the encoded form
            // is not pre-empted by a shorter raw substring match.
            let encoded = percent_encode(key);
            secret_forms.push(key.to_string());
            if encoded != key {
                secret_forms.push(encoded);
            }
            secret_forms.sort_by_key(|s| std::cmp::Reverse(s.len()));
        }

        let timeout = Duration::from_millis(block.timeout_ms());
        Self {
            plugin_id: plugin_id.to_string(),
            log_target: log_target(&base),
            url,
            header_auth,
            secret_forms,
            agent: ureq::AgentBuilder::new()
                // Both halves matter: `timeout_connect` bounds a black-holed
                // SYN, `timeout` bounds a server that accepts and then stalls.
                // Neither is a TOTAL bound — the caller wraps the whole
                // connector spawn in one outer `tokio::time::timeout` (§2.2).
                .timeout_connect(timeout)
                .timeout(timeout)
                .build(),
            next_id: std::sync::atomic::AtomicU64::new(1),
        }
    }

    /// Host (and port) of the endpoint — the only part of the URL that is
    /// safe to log, since the API key may ride in the query string.
    pub fn log_target(&self) -> &str {
        &self.log_target
    }

    /// Replace every literal occurrence of the API key with `<redacted>`.
    ///
    /// Applied to EVERY string this module can hand back, because those
    /// strings all converge on operator- and agent-visible sinks:
    /// `tracing::warn!(reason)`, the persisted+broadcast
    /// `Event::PluginState.last_error`, the `POST /enable` 503 body, and (via
    /// `tools_call`) the wave transcript.
    fn scrub(&self, s: String) -> String {
        let mut out = s;
        for form in &self.secret_forms {
            if out.contains(form.as_str()) {
                out = out.replace(form.as_str(), "<redacted>");
            }
        }
        out
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
            RpcError::internal(self.scrub(format!(
                "mcp-http tools/call: response did not parse as CallToolResult: {e}"
            )))
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
        let agent = self.agent.clone();
        let method_owned = method.to_string();
        let target = self.log_target.clone();

        let text = tokio::task::spawn_blocking(move || {
            let mut req = agent
                .post(&url)
                .set("content-type", "application/json")
                // The probed server answers `text/event-stream`; accepting
                // both means a plain-JSON server works unchanged.
                .set("accept", "application/json, text/event-stream");
            if let Some((name, value)) = header_auth.as_ref() {
                req = req.set(name, value);
            }
            // NOTE: a `ureq::Error` must NEVER be formatted with `{e}`. Its
            // `Display` prints the full URL first, and this client folds the
            // API key into that URL's query string. Only `kind()` — a closed
            // set of English descriptions — is safe.
            match req.send_string(&body) {
                Ok(resp) => read_capped(resp),
                Err(ureq::Error::Status(code, resp)) => {
                    let detail = read_capped(resp).unwrap_or_default();
                    let detail: String = detail.chars().take(512).collect();
                    Err(format!("HTTP {code} from {target}: {detail}"))
                }
                Err(e) => Err(format!("request to {target} failed: {}", e.kind())),
            }
        })
        .await
        .map_err(|e| RpcError::internal(format!("mcp-http request task failed: {e}")))?
        .map_err(|e| {
            RpcError::custom(-32002, self.scrub(format!("mcp-http {method_owned}: {e}")))
        })?;

        let payload = strip_sse_envelope(&text).ok_or_else(|| {
            RpcError::internal(format!(
                "mcp-http {method_owned}: response carried no JSON payload"
            ))
        })?;
        let parsed: Value = serde_json::from_str(payload).map_err(|e| {
            RpcError::internal(self.scrub(format!("mcp-http {method_owned}: malformed JSON: {e}")))
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
            // The upstream authored this string; it may quote our own query
            // string back at us (risk R6's residual, narrowed to the one form
            // we can actually recognize).
            return Err(RpcError::custom(code, self.scrub(message)));
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

/// Read a response body with [`MAX_BODY_BYTES`] enforced **before** buffering.
///
/// `Response::into_string()` buffers first and only then could a caller measure
/// the length, so the operative bound was ureq's own 10 MiB cap, not ours.
/// `take(MAX + 1)` makes the check exact: one byte over the cap is observable
/// without ever allocating the whole body.
fn read_capped(resp: ureq::Response) -> Result<String, String> {
    let mut buf = Vec::with_capacity(8 * 1024);
    resp.into_reader()
        .take(MAX_BODY_BYTES as u64 + 1)
        .read_to_end(&mut buf)
        .map_err(|e| format!("reading response body failed: {}", e.kind()))?;
    if buf.len() > MAX_BODY_BYTES {
        return Err(format!("response body exceeds {MAX_BODY_BYTES} bytes"));
    }
    String::from_utf8(buf).map_err(|_| "response body is not valid UTF-8".to_string())
}

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

    const LEAKY: &str = "sk-super-secret-8213";

    fn client_with(api_key_in: &str) -> HttpMcpClient {
        let block: McpHttpBlock = serde_json::from_value(serde_json::json!({
            "url": "https://mcp.example.com/mcp",
            "api_key_secret": "K",
            "api_key_in": api_key_in,
        }))
        .unwrap();
        HttpMcpClient::new("c", &block, Some(LEAKY))
    }

    /// A derived `Debug` would print `url` (key in the query) and
    /// `header_auth` (name AND value), defeating `ConnectorClient`'s own
    /// redacting `Debug`.
    #[test]
    fn debug_never_prints_the_key_in_either_placement() {
        for spec in ["query:api_key", "header:x-api-key"] {
            let rendered = format!("{:?}", client_with(spec));
            assert!(
                !rendered.contains(LEAKY),
                "{spec}: Debug leaked the key: {rendered}"
            );
            assert!(rendered.contains("redacted"), "{spec}: {rendered}");
            assert!(rendered.contains("mcp.example.com"), "{spec}: {rendered}");
        }
    }

    /// The scrubber must catch the percent-encoded form too — that is the
    /// literal an upstream echoing our query string back would use.
    #[test]
    fn scrub_removes_raw_and_percent_encoded_forms() {
        let key = "sk a/b+c";
        let block: McpHttpBlock = serde_json::from_value(serde_json::json!({
            "url": "https://mcp.example.com/mcp",
            "api_key_secret": "K",
            "api_key_in": "query:api_key",
        }))
        .unwrap();
        let client = HttpMcpClient::new("c", &block, Some(key));
        let encoded = percent_encode(key);
        assert_ne!(encoded, key);

        let raw_msg = client.scrub(format!("boom: {key}"));
        assert!(!raw_msg.contains(key), "{raw_msg}");
        let enc_msg = client.scrub(format!("https://h/mcp?api_key={encoded} failed"));
        assert!(!enc_msg.contains(&encoded), "{enc_msg}");
        assert!(enc_msg.contains("<redacted>"), "{enc_msg}");
    }

    /// No key configured ⇒ nothing to scrub, and no accidental blanket
    /// replacement of the empty string.
    #[test]
    fn scrub_is_identity_without_a_key() {
        let block: McpHttpBlock = serde_json::from_value(serde_json::json!({
            "url": "https://mcp.example.com/mcp",
        }))
        .unwrap();
        let client = HttpMcpClient::new("c", &block, None);
        assert_eq!(client.scrub("plain".to_string()), "plain");
        assert!(format!("{client:?}").contains("none"));
    }
}
