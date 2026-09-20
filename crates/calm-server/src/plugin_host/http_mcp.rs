//! The `mcp-http` connector's client: POST one JSON body, read the whole response, strip a leading
//! `data:` if present, parse as a JSON-RPC response. Every string that leaves this module is scrubbed of the credential.

use std::collections::HashSet;
use std::io::Read as _;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use serde_json::{Value, json};

use super::manifest::{ApiKeyIn, McpHttpBlock, ResolvedMcpUrl};
use super::mcp::RpcError;

/// SSE data-line prefix.
const SSE_DATA_PREFIX: &str = "data:";

/// Cap on the response body we will buffer.
const MAX_BODY_BYTES: usize = 4 * 1024 * 1024;

/// A fast peer can otherwise return a fresh cursor forever; crossing the cap is an error, never a partial-success return.
const MAX_TOOLS_LIST_PAGES: usize = 100;

/// Cursors ride in the next request body, so bound one independently of the response-body cap.
const MAX_TOOLS_CURSOR_BYTES: usize = 4 * 1024;

/// How much of an upstream error body survives into the operator-facing message; applied strictly AFTER [`scrub_with`].
pub const MAX_UPSTREAM_DETAIL_CHARS: usize = 512;

/// Cap on upstream-authored identity strings we record in a `tracing` line.
const MAX_SERVER_IDENT_CHARS: usize = 128;

/// Shortest credential this client will carry: redaction is by literal match, so a short value
/// matches constantly inside unrelated upstream text and turns redaction into corruption.
pub const MIN_CREDENTIAL_LEN: usize = 8;
/// The TCP-connect deadline is never allowed below this. ureq resolves `timeout_connect` ahead of the
/// per-request deadline, so a bring-up-sized connect timeout would subject every cold `tools/call` to it.
pub const CONNECT_TIMEOUT_FLOOR: Duration = Duration::from_secs(10);

/// A credential that has been checked against everything the HTTP path's redaction machinery
/// requires of it; the only way to obtain one is [`HttpCredential::parse`].
#[derive(Clone)]
pub struct HttpCredential(String);

impl HttpCredential {
    /// Refuses: empty; non-printable-ASCII, space, `"` or `\`; anything JSON parses as a number (an
    /// upstream can echo it back as a number, which [`scrub_value`] never descends into); shorter than
    /// [`MIN_CREDENTIAL_LEN`]; and text overlapping the redaction marker. The error never quotes the credential.
    pub fn parse(raw: &str) -> Result<Self, String> {
        if raw.is_empty() {
            return Err("is empty; remove the key or give it a real credential".to_string());
        }
        if let Some(bad) = raw
            .chars()
            .find(|c| !c.is_ascii_graphic() || *c == '"' || *c == '\\')
        {
            let named = match bad {
                '"' => "a double quote".to_string(),
                '\\' => "a backslash".to_string(),
                ' ' => "a space".to_string(),
                c if c.is_control() => "a control character".to_string(),
                c if !c.is_ascii() => "a non-ASCII character".to_string(),
                _ => "a character outside printable ASCII".to_string(),
            };
            return Err(format!(
                "contains {named}, which cannot be redacted reliably: an HTTP \
                 credential must be printable ASCII with no spaces, quotes or \
                 backslashes"
            ));
        }
        if is_number_shaped(raw) {
            return Err(
                "parses as a JSON number: an upstream is free to echo such a value back \
                 as a JSON *number*, which carries no string to redact, so it would reach \
                 tool results and track transcripts in the clear. Add a non-numeric \
                 character to the credential."
                    .to_string(),
            );
        }
        if raw.len() < MIN_CREDENTIAL_LEN {
            return Err(format!(
                "is shorter than {MIN_CREDENTIAL_LEN} characters, which would make \
                 redaction match unrelated upstream text"
            ));
        }
        if overlaps_redaction_marker(raw) {
            // The marker is deliberately not spelled here: several credentials this rule refuses are substrings of it.
            return Err("overlaps the marker string this module rewrites \
                 credentials to — it ends where that marker begins, begins \
                 where it ends, contains it, or is a piece of it. Scrubbing is \
                 a single left-to-right pass that never rescans what it wrote, \
                 so an upstream can steer it into re-forming exactly this \
                 credential out of the marker's own text: the scrubbed string \
                 would then carry the credential verbatim into tool catalogs \
                 and track transcripts. Choose a credential that shares no text \
                 with that marker."
                .to_string());
        }
        Ok(Self(raw.to_string()))
    }

    fn as_str(&self) -> &str {
        &self.0
    }
}

/// Is `raw` a value an upstream could echo back as a bare JSON **number**? Asks serde_json's scanner
/// (`IgnoredAny`, no f64 conversion, so `1234e567` counts); the digits-only arm covers leading zeros the grammar rejects.
fn is_number_shaped(raw: &str) -> bool {
    raw.chars().all(|c| c.is_ascii_digit())
        || serde_json::from_str::<serde::de::IgnoredAny>(raw).is_ok()
}

/// Can a single [`scrub_with`] pass re-form `raw` out of its own redaction? `str::replace` never
/// rescans what it wrote, so a surviving occurrence must intersect an inserted marker — in exactly four ways.
fn overlaps_redaction_marker(raw: &str) -> bool {
    is_marker_family_substring(raw)
        || tail_is_marker_prefix(raw)
        || head_is_marker_suffix(raw)
        || raw.contains(REDACTED)
}

/// Case 4 — `raw` is a substring of `REDACTED` or of `REDACTED#<digits>`. Split at the maximal
/// trailing digit run: no digits ⇒ inside `P`; no head ⇒ all digits; else `P.ends_with(head)`.
fn is_marker_family_substring(raw: &str) -> bool {
    let family = format!("{REDACTED}#");
    let head = raw.trim_end_matches(|c: char| c.is_ascii_digit());
    let digits = &raw[head.len()..];
    if digits.is_empty() {
        family.contains(raw)
    } else if head.is_empty() {
        true
    } else {
        family.ends_with(head)
    }
}

/// Case 2 — `raw` ends with a nonempty prefix of [`REDACTED`]. `REDACTED` is ASCII, so byte slicing is safe.
fn tail_is_marker_prefix(raw: &str) -> bool {
    (1..=REDACTED.len()).any(|k| raw.ends_with(&REDACTED[..k]))
}

/// Case 1 — `raw` begins with a nonempty suffix of [`REDACTED`].
fn head_is_marker_suffix(raw: &str) -> bool {
    (1..=REDACTED.len()).any(|k| raw.starts_with(&REDACTED[REDACTED.len() - k..]))
}

/// Redacting, so a credential cannot reach a log line through a `{:?}` on a
/// struct that happens to hold one.
impl std::fmt::Debug for HttpCredential {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("HttpCredential(<redacted>)")
    }
}

/// One remote streamable-HTTP MCP server. No `#[derive(Debug)]`: a derived one would print
/// `header_auth` and `url`, which may carry an operator-written secret.
pub struct HttpMcpClient {
    plugin_id: String,
    /// The resolved endpoint, verbatim. Never logged.
    url: String,
    /// `(name, value)` for the credential header.
    header_auth: Option<(String, String)>,
    headers: super::http_headers::HttpHeaders,
    /// Host only, for the per-call audit line.
    log_target: String,
    /// The literals the secret is known to take in a string an upstream may hand back: at most two —
    /// the raw credential and its uppercase-hex percent-encoding. Not exhaustive.
    secret_forms: Vec<String>,
    /// Built once: a fresh `ureq::Agent` re-parses the webpki root store and gives up connection reuse.
    agent: ureq::Agent,
    /// Per-request deadline for `initialize` and each `tools/list` page on the inline-awaited boot path.
    bringup_timeout: Duration,
    /// Deadline for a steady-state `tools/call`. Uncapped on purpose.
    call_timeout: Duration,
    next_id: std::sync::atomic::AtomicU64,
}

/// Which of the two budgets a round trip is spending. No default: every call site names it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    /// `initialize` / each `tools/list` page — bounded, because boot waits.
    Bringup,
    /// `tools/call` — generous, because a real tool may run for minutes.
    Call,
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
                    // A credential with nowhere to go: `Manifest::validate` makes this unreachable.
                    (None, false) => "unrouted:<redacted>".to_string(),
                    (None, true) => "none".to_string(),
                },
            )
            .finish_non_exhaustive()
    }
}

impl HttpMcpClient {
    /// Build a client from a validated `mcp_http` block plus the connector's resolved secret value.
    /// The [`HttpCredential`] and [`ResolvedMcpUrl`] parameter types are what discharge the redaction and URL-resolution constraints.
    pub fn new(
        plugin_id: &str,
        url: &ResolvedMcpUrl,
        block: &McpHttpBlock,
        api_key: Option<&HttpCredential>,
    ) -> Self {
        let url = url.as_str().to_string();
        let mut header_auth = None;
        let mut secret_forms = Vec::new();

        if let Some(key) = api_key.map(HttpCredential::as_str) {
            match block.api_key_in_parsed() {
                // The value SHAPE is the point: the upstream rejects `Authorization: <key>` without the `Bearer ` prefix.
                Some(ApiKeyIn::Bearer) => {
                    header_auth = Some(("Authorization".to_string(), format!("Bearer {key}")));
                }
                // Verbatim, no prefix — the operator controls the exact bytes.
                Some(ApiKeyIn::Header(name)) => {
                    header_auth = Some((name, key.to_string()));
                }
                // `Manifest::validate` rejects every string that lands here; send nothing rather than guess a slot. The credential is still registered for scrubbing below.
                None => {}
            }
            // Longest first: `%` is itself reserved, so a raw form like `abcde%25` is a PREFIX of its encoding
            // `abcde%2525` and raw-first would chew the encoded literal in half. Empty forms are filtered because an empty pattern makes `scrub` a memory amplifier.
            for form in [key.to_string(), percent_encode(key)] {
                if !form.is_empty() && !secret_forms.contains(&form) {
                    secret_forms.push(form);
                }
            }
            secret_forms.sort_by_key(|f| std::cmp::Reverse(f.len()));
        }

        let bringup_timeout = Duration::from_millis(block.bringup_timeout_ms());
        let call_timeout = Duration::from_millis(block.timeout_ms());
        Self {
            plugin_id: plugin_id.to_string(),
            log_target: log_target(&url),
            url,
            header_auth,
            headers: super::http_headers::HttpHeaders::default(),
            secret_forms,
            agent: ureq::AgentBuilder::new()
                // Following an upstream redirect would replay custom auth headers to the target.
                .redirects(0)
                // Connect has its own floor (see [`CONNECT_TIMEOUT_FLOOR`]); the `max` keeps a deliberately longer
                // bring-up in charge. The per-request deadline is supplied per call from [`Phase`].
                .timeout_connect(bringup_timeout.max(CONNECT_TIMEOUT_FLOOR))
                .build(),
            bringup_timeout,
            call_timeout,
            next_id: std::sync::atomic::AtomicU64::new(1),
        }
    }

    /// Headers arrive validated and are always registered for redaction before any request is possible.
    pub fn with_headers(mut self, headers: super::http_headers::HttpHeaders) -> Self {
        for value in headers.private_values() {
            let escaped = serde_json::to_string(&value).expect("string serialization");
            for form in [
                value.clone(),
                percent_encode(&value),
                escaped[1..escaped.len() - 1].to_string(),
            ] {
                if !form.is_empty() && !self.secret_forms.contains(&form) {
                    self.secret_forms.push(form);
                }
            }
        }
        self.secret_forms
            .sort_by_key(|form| std::cmp::Reverse(form.len()));
        self.log_target = self.scrub(self.log_target.clone());
        self.headers = headers;
        self
    }

    /// Host (and port) of the endpoint — the only part of the URL that is safe to log: `mcp_http.url`
    /// is an operator-written literal that may carry a secret in its query string.
    pub fn log_target(&self) -> &str {
        &self.log_target
    }

    /// Replace every literal occurrence of the API key with `<redacted>`; applied to every string this module can hand back.
    fn scrub(&self, s: String) -> String {
        scrub_with(&self.secret_forms, s)
    }

    /// Scrub, THEN clamp to `max` chars: clamping first can leave a partial key that no longer matches a scrub literal.
    fn scrub_and_clamp(&self, s: &str, max: usize) -> String {
        clamp_chars(self.scrub(s.to_string()), max)
    }

    /// Minimal `initialize`. Best-effort: a server that does not implement it must not block the connector, so the caller treats an error as informational.
    pub async fn initialize(&self) -> Result<Value, RpcError> {
        let params = json!({
            "protocolVersion": super::mcp::KERNEL_PROTOCOL_VERSION,
            "capabilities": {},
            "clientInfo": { "name": "neige-kernel", "version": env!("CARGO_PKG_VERSION") },
        });
        let result = self.request(Phase::Bringup, "initialize", params).await?;
        // Both are upstream-authored: scrubbed and bounded before they reach the operator's log.
        let server_version = self.scrub_and_clamp(
            result
                .get("protocolVersion")
                .and_then(|v| v.as_str())
                .unwrap_or("<unset>"),
            MAX_SERVER_IDENT_CHARS,
        );
        let server_name = self.scrub_and_clamp(
            result
                .pointer("/serverInfo/name")
                .and_then(|v| v.as_str())
                .unwrap_or("<unset>"),
            MAX_SERVER_IDENT_CHARS,
        );
        // Records, never compares: there is no kernel-side set of known-good external protocol versions.
        tracing::info!(
            plugin_id = %self.plugin_id,
            target = %self.log_target,
            server_protocol_version = %server_version,
            server_name = %server_name,
            "mcp-http connector initialized"
        );
        Ok(result)
    }

    /// Drain `tools/list`, returning every raw `tools` array entry. Any pagination fault fails the whole
    /// discovery: a partial catalog must never be published as though it were complete.
    pub async fn tools_list(&self) -> Result<Vec<Value>, RpcError> {
        let mut all_tools = Vec::new();
        let mut catalog_bytes = 0usize;
        let mut cursor: Option<String> = None;
        let mut seen = HashSet::new();

        for page in 0..MAX_TOOLS_LIST_PAGES {
            let params = match cursor.as_deref() {
                Some(value) => json!({ "cursor": value }),
                None => json!({}),
            };
            let result = self.request(Phase::Bringup, "tools/list", params).await?;
            let tools = result
                .get("tools")
                .and_then(Value::as_array)
                .ok_or_else(|| {
                    RpcError::internal("mcp-http tools/list: result.tools must be an array")
                })?;
            catalog_bytes += tools
                .iter()
                .map(|tool| tool.to_string().len())
                .sum::<usize>();
            if catalog_bytes > MAX_BODY_BYTES || all_tools.len() + tools.len() > 10_000 {
                return Err(RpcError::internal(
                    "mcp-http tools/list: complete catalog exceeds size limit",
                ));
            }
            all_tools.extend(tools.iter().cloned());

            let next = match result.get("nextCursor") {
                None | Some(Value::Null) => return Ok(all_tools),
                Some(Value::String(value)) if value.is_empty() => {
                    return Err(RpcError::internal(
                        "mcp-http tools/list: result.nextCursor must be a non-empty string or null",
                    ));
                }
                Some(Value::String(value)) if value.len() > MAX_TOOLS_CURSOR_BYTES => {
                    return Err(RpcError::internal(format!(
                        "mcp-http tools/list: result.nextCursor exceeds {MAX_TOOLS_CURSOR_BYTES} bytes"
                    )));
                }
                Some(Value::String(value)) => value.clone(),
                Some(_) => {
                    return Err(RpcError::internal(
                        "mcp-http tools/list: result.nextCursor must be a string or null",
                    ));
                }
            };
            if !seen.insert(next.clone()) {
                return Err(RpcError::internal(
                    "mcp-http tools/list: result.nextCursor repeated; refusing a pagination loop",
                ));
            }
            if page + 1 == MAX_TOOLS_LIST_PAGES {
                return Err(RpcError::internal(format!(
                    "mcp-http tools/list: catalog exceeded {MAX_TOOLS_LIST_PAGES} pages"
                )));
            }
            cursor = Some(next);
        }

        unreachable!("the final page-cap iteration returns")
    }

    /// `tools/call`, parsed into the same envelope stdio plugins return.
    pub async fn tools_call(
        &self,
        name: &str,
        arguments: Value,
    ) -> Result<super::mcp::CallToolResult, RpcError> {
        // Every outbound tool call records its target host; the URL itself is never logged.
        tracing::info!(
            plugin_id = %self.plugin_id,
            target = %self.log_target,
            tool = %name,
            "mcp-http connector tools/call"
        );
        let raw = self
            .request(
                Phase::Call,
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

    /// One JSON-RPC round trip, on the blocking pool; the client enforces the `phase` deadline itself.
    async fn request(&self, phase: Phase, method: &str, params: Value) -> Result<Value, RpcError> {
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
        let headers = self.headers.clone();
        let agent = self.agent.clone();
        let method_owned = method.to_string();
        let target = self.log_target.clone();
        let deadline = match phase {
            Phase::Bringup => self.bringup_timeout,
            Phase::Call => self.call_timeout,
        };
        // The non-2xx body must be scrubbed INSIDE the closure, before the clamp below.
        let secret_forms = self.secret_forms.clone();

        // Dropping a `spawn_blocking` JoinHandle does NOT cancel the closure: without this flag a request
        // abandoned by the caller's outer timeout would later fire a credential-bearing request anyway.
        let cancelled = Arc::new(AtomicBool::new(false));
        let _cancel_on_drop = CancelOnDrop(Arc::clone(&cancelled));
        let closure_cancel = Arc::clone(&cancelled);

        let text = tokio::task::spawn_blocking(move || {
            if closure_cancel.load(Ordering::SeqCst) {
                return Err("request abandoned before it was sent (caller went away)".to_string());
            }
            let mut req = agent
                .post(&url)
                // `Request::timeout` overrides the agent's read/write deadlines but NOT `timeout_connect`.
                .timeout(deadline)
                .set("content-type", "application/json")
                // Accepting both means a plain-JSON server works unchanged.
                .set("accept", "application/json, text/event-stream");
            if let Some((name, value)) = header_auth.as_ref() {
                req = req.set(name, value);
            }
            for (name, value) in headers.pairs() {
                req = req.set(name, value);
            }
            // A `ureq::Error` must NEVER be formatted with `{e}`: its `Display` prints the full URL first. Only `kind()` is safe.
            match req.send_string(&body) {
                Ok(resp) => read_capped(resp),
                Err(ureq::Error::Status(code, resp)) => {
                    // The one raw-text scrub: a non-2xx body has no JSON tree to walk. SCRUB, then clamp — reversing these leaks a partial key.
                    let detail = scrub_with(&secret_forms, read_capped(resp).unwrap_or_default());
                    let detail = clamp_chars(detail, MAX_UPSTREAM_DETAIL_CHARS);
                    Err(format!("HTTP {code} from {target}: {detail}"))
                }
                Err(e) => Err(format!("request to {target} failed: {}", e.kind())),
            }
        })
        .await
        // Never format the `JoinError` payload: a panic payload is arbitrary text and this arm does not pass through `scrub`.
        .map_err(|e| {
            let what = if e.is_cancelled() {
                "was cancelled"
            } else {
                "panicked"
            };
            RpcError::internal(format!("mcp-http {method_owned} request task {what}"))
        })?
        .map_err(|e| {
            RpcError::custom(-32002, self.scrub(format!("mcp-http {method_owned}: {e}")))
        })?;

        // The choke point: everything below is derived from this value, scrubbed as a JSON tree (decoded strings and keys), never as raw text.
        let mut parsed = parse_scrubbed(&self.secret_forms, &text, &method_owned)?;
        // A cursor is private routing data, not exposed catalog metadata. Keep
        // its exact bytes even when they overlap a credential scrub pattern.
        if method == "tools/list"
            && let Some(payload) = strip_sse_envelope(&text)
            && let Ok(raw) = serde_json::from_str::<Value>(payload)
            && let Some(cursor) = raw.pointer("/result/nextCursor")
            && let Some(result) = parsed.get_mut("result").and_then(Value::as_object_mut)
        {
            result.insert("nextCursor".into(), cursor.clone());
        }

        if let Some(err) = parsed.get("error")
            && !err.is_null()
        {
            let code = err.get("code").and_then(|c| c.as_i64()).unwrap_or(-32603);
            let message = err
                .get("message")
                .and_then(|m| m.as_str())
                .unwrap_or("upstream error")
                .to_string();
            // Upstream-authored, but `parse_scrubbed` already walked it.
            return Err(RpcError::custom(code, message));
        }
        parsed.get("result").cloned().ok_or_else(|| {
            RpcError::internal(format!(
                "mcp-http {method_owned}: response had neither `result` nor `error`"
            ))
        })
    }
}

/// Strip the SSE envelope, parse, and scrub the resulting JSON tree. Recursion is bounded by
/// `serde_json`'s own 128-level nesting limit.
fn parse_scrubbed(forms: &[String], text: &str, method: &str) -> Result<Value, RpcError> {
    let payload = strip_sse_envelope(text).ok_or_else(|| {
        RpcError::internal(format!(
            "mcp-http {method}: response carried no JSON payload"
        ))
    })?;
    // `serde_json::Error`'s Display never carries the input, but it is scrubbed anyway.
    let mut parsed: Value = serde_json::from_str(payload).map_err(|e| {
        RpcError::internal(scrub_with(
            forms,
            format!("mcp-http {method}: malformed JSON: {e}"),
        ))
    })?;
    scrub_value(forms, &mut parsed);
    Ok(parsed)
}

/// Recursively replace every literal in `forms` inside a JSON tree — string values and object keys
/// (`inputSchema` property names reach the agent-visible catalog). Rekeying is not entry-count preserving.
fn scrub_value(forms: &[String], v: &mut Value) {
    if forms.is_empty() {
        return;
    }
    if !v.is_string()
        && !v.is_array()
        && !v.is_object()
        && forms.iter().any(|form| form == &v.to_string())
    {
        *v = Value::String(safe_marker(forms).to_string());
        return;
    }
    match v {
        Value::String(s) => *s = scrub_with(forms, std::mem::take(s)),
        Value::Array(items) => {
            for item in items {
                scrub_value(forms, item);
            }
        }
        Value::Object(map) => {
            // `contains` is the exact predicate `str::replace` decides on, so a key cannot be judged by one rule and rewritten by another.
            let rekey = map
                .keys()
                .any(|k| forms.iter().any(|f| k.contains(f.as_str())));
            if rekey {
                // Accepted, lossy: sibling keys that scrub to the same marker collapse into one entry — data loss, not
                // a leak. A disambiguating suffix is worse: a minted name like `<redacted>#2` can itself be the credential.
                let taken = std::mem::take(map);
                *map = taken
                    .into_iter()
                    .map(|(k, v)| (scrub_with(forms, k), v))
                    .collect();
            }
            // Children are walked whether or not this map was rekeyed; moving this into an `else` leaks the values.
            for (_, child) in map.iter_mut() {
                scrub_value(forms, child);
            }
        }
        // Numbers, booleans and null carry no string to redact; a number-shaped credential is refused by `HttpCredential::parse`.
        _ => {}
    }
}

/// Trips its flag when dropped.
struct CancelOnDrop(Arc<AtomicBool>);

impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        self.0.store(true, Ordering::SeqCst);
    }
}

/// The marker every recognised form of the credential is rewritten to.
const REDACTED: &str = "<redacted>";

fn safe_marker(forms: &[String]) -> &'static str {
    if forms
        .iter()
        .any(|form| !form.is_empty() && REDACTED.contains(form.as_str()))
    {
        ""
    } else {
        REDACTED
    }
}

fn scrub_with(forms: &[String], s: String) -> String {
    let marker = safe_marker(forms);
    let mut out = s;
    for form in forms {
        debug_assert!(!form.is_empty(), "an empty scrub pattern is a memory bomb");
        if !form.is_empty() && out.contains(form.as_str()) {
            out = out.replace(form.as_str(), marker);
        }
    }
    // Several arbitrary header values can overlap one another or the marker.
    // Never return a credential re-formed across a replacement boundary.
    if forms
        .iter()
        .any(|form| !form.is_empty() && out.contains(form.as_str()))
    {
        marker.to_string()
    } else {
        out
    }
}

/// Clamp to `max` *characters* (never bytes — this must not split a UTF-8
/// sequence), appending an explicit marker so a reader knows it is partial.
fn clamp_chars(s: String, max: usize) -> String {
    if s.chars().nth(max).is_none() {
        return s;
    }
    let mut out: String = s.chars().take(max).collect();
    out.push_str("…(truncated)");
    out
}

/// Extract the JSON payload from either a bare JSON body or a single-frame SSE body. Multi-line `data:` frames are out of scope.
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

/// `scheme://host[:port]`, dropping path and query — the query may carry an operator-written secret.
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

/// Minimal percent-encoding, uppercase hex — the single encoded spelling worth carrying as a scrub
/// literal. Lowercase and mixed-case triplets are NOT covered.
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

/// Read a response body with [`MAX_BODY_BYTES`] enforced before buffering; `take(MAX + 1)` makes one byte over observable without allocating the whole body.
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

    /// Scrub patterns for tests that bypass `HttpMcpClient::new`.
    fn exact_forms(keys: &[&str]) -> Vec<String> {
        keys.iter().map(|k| (*k).to_string()).collect()
    }

    /// The endpoint these fixtures run against, through the real resolver with no configuration in force.
    fn resolved(block: &McpHttpBlock) -> ResolvedMcpUrl {
        super::super::manifest::resolve_mcp_http_url(block, &serde_json::Map::new())
            .unwrap_or_else(|e| panic!("fixture url must resolve: {e}"))
    }

    /// Shorthand for a credential the HTTP path will accept.
    fn cred(raw: &str) -> HttpCredential {
        HttpCredential::parse(raw)
            .unwrap_or_else(|e| panic!("{raw:?} must be a valid credential: {e}"))
    }

    #[test]
    fn a_credential_the_scrubber_cannot_handle_is_refused() {
        let cases: &[(&str, &str)] = &[
            ("", "empty"),
            (
                r#"ab":cdefgh"#,
                "quote — collides with JSON syntax in the 4xx arm",
            ),
            (r"abc\defgh", "backslash — collides with escape pairs"),
            (
                "abcd\nefgh",
                "newline — arrives JSON-escaped, never matched",
            ),
            ("abcd\u{7}efgh", "bell — control character"),
            ("abcd efgh", "space"),
            ("sk-\u{4e2d}\u{6587}-key", "non-ASCII"),
            ("short12", "one under the length floor"),
            ("e", "single character"),
            (
                "12345678",
                "all digits — echoed back as a JSON number, unscrubbable",
            ),
            ("00000000000000000000", "all digits, long"),
            ("-1234567", "negative integer"),
            ("1.234567", "decimal fraction"),
        ];
        for (bad, why) in cases {
            let err = HttpCredential::parse(bad)
                .err()
                .unwrap_or_else(|| panic!("{why}: {bad:?} must be refused"));
            // The refusal may name the rule, never the value (only meaningful for values long enough to be a credential).
            if bad.len() >= MIN_CREDENTIAL_LEN {
                assert!(!err.contains(bad), "{why}: the refusal quotes it: {err}");
            }
        }
    }

    /// Each case asserts the *number* refusal specifically: the length floor would otherwise satisfy the short spellings.
    #[test]
    fn a_number_shaped_credential_is_refused_whatever_its_spelling() {
        let mut leaks: Vec<String> = Vec::new();
        for (bad, why) in [
            ("12345678", "all digits — where the rule started"),
            ("00000000000000000000", "digits with leading zeros"),
            ("-1234567", "a leading minus is part of the number grammar"),
            ("1.234567", "a fraction"),
            (
                "1234e567",
                "exponent past f64: still a number token on the wire, and \
                 `from_str::<Value>` alone would MISS it",
            ),
            ("-1.2e-70", "sign, fraction and negative exponent together"),
            ("-1.2e-7", "the same, one character under the length floor"),
            ("1E5", "capital exponent marker"),
        ] {
            let outcome = match HttpCredential::parse(bad) {
                Ok(_) => Some("ACCEPTED".to_string()),
                Err(e) if !e.contains("parses as a JSON number") => {
                    Some(format!("refused by another rule: {e}"))
                }
                Err(e) => {
                    assert!(!e.contains(bad), "{why}: the refusal quotes it: {e}");
                    None
                }
            };
            if let Some(what) = outcome {
                leaks.push(format!("{bad:?} ({why}): {what}"));
            }
        }
        assert!(
            leaks.is_empty(),
            "these JSON numbers are not refused by the number rule, so an \
             upstream echoing one back numerically leaks it:\n  {}",
            leaks.join("\n  ")
        );
    }

    #[test]
    fn a_well_formed_credential_is_accepted() {
        for good in [
            "a".repeat(MIN_CREDENTIAL_LEN),     // exactly the floor: `<`, not `<=`
            "sk-super-secret-8213".to_string(), // the real shape
            "sk-abc12345".to_string(),          // a real key with digits in it
            "ghp_xxxxxxxx".to_string(),         // the other real shape
            "1234567a".to_string(),             // digits are fine WITH a non-digit
            "a/b+c=d&e".to_string(),            // punctuation an API key really uses
            "1234e567a".to_string(),            // number-ish, but not a number
            "-1.2e-7-".to_string(),             // ditto: the grammar, not a vibe
        ] {
            assert!(
                HttpCredential::parse(&good).is_ok(),
                "{good:?} must be accepted"
            );
        }
    }

    #[test]
    fn the_connect_deadline_is_never_below_its_own_floor() {
        let block: McpHttpBlock = serde_json::from_value(serde_json::json!({
            "url": "https://mcp.example.com/mcp",
            "bringup_timeout_ms": 400,
            "request_timeout_ms": 600_000,
        }))
        .unwrap();
        let client = HttpMcpClient::new("c", &resolved(&block), &block, None);
        assert_eq!(client.bringup_timeout, Duration::from_millis(400));
        assert!(
            CONNECT_TIMEOUT_FLOOR > client.bringup_timeout,
            "the floor must actually be doing something for this fixture"
        );
        assert!(CONNECT_TIMEOUT_FLOOR < client.call_timeout);
    }

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

    /// Asserted on the header VALUE, not its presence: a bare `Authorization: sk-…` is what the upstream rejects.
    #[test]
    fn bearer_sends_the_authorization_header_with_the_bearer_prefix() {
        let block: McpHttpBlock = serde_json::from_value(serde_json::json!({
            "url": "https://mcp.example.com/mcp",
            "api_key_secret": "K",
            "api_key_in": "bearer",
        }))
        .unwrap();
        let key = "sk-a-b-c-8213";
        let client = HttpMcpClient::new("c", &resolved(&block), &block, Some(&cred(key)));
        assert_eq!(
            client.header_auth,
            Some(("Authorization".to_string(), format!("Bearer {key}"))),
            "the header VALUE must carry the `Bearer ` prefix, not the bare key"
        );
        assert_eq!(client.url, "https://mcp.example.com/mcp");
    }

    #[test]
    fn the_url_is_the_resolved_base_verbatim_and_the_query_never_reaches_a_log() {
        let block: McpHttpBlock = serde_json::from_value(serde_json::json!({
            "url": "https://mcp.example.com/mcp?v=1",
            "api_key_secret": "K",
            "api_key_in": "bearer",
        }))
        .unwrap();
        let client = HttpMcpClient::new("c", &resolved(&block), &block, Some(&cred("abcdefgh")));
        assert_eq!(client.url, "https://mcp.example.com/mcp?v=1");
        assert_eq!(client.log_target(), "https://mcp.example.com");
    }

    /// `header:<name>` sends the credential verbatim — an `X-API-Key`-style server wants the bare bytes.
    #[test]
    fn header_key_does_not_touch_the_url() {
        let block: McpHttpBlock = serde_json::from_value(serde_json::json!({
            "url": "https://mcp.example.com/mcp",
            "api_key_secret": "K",
            "api_key_in": "header:x-api-key",
        }))
        .unwrap();
        let client = HttpMcpClient::new("c", &resolved(&block), &block, Some(&cred("abcdefgh")));
        assert_eq!(client.url, "https://mcp.example.com/mcp");
        assert_eq!(
            client.header_auth,
            Some(("x-api-key".to_string(), "abcdefgh".to_string()))
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
        HttpMcpClient::new("c", &resolved(&block), &block, Some(&cred(LEAKY)))
    }

    #[test]
    fn debug_never_prints_the_key_in_either_placement() {
        for spec in ["bearer", "header:x-api-key"] {
            let rendered = format!("{:?}", client_with(spec));
            assert!(
                !rendered.contains(LEAKY),
                "{spec}: Debug leaked the key: {rendered}"
            );
            assert!(rendered.contains("redacted"), "{spec}: {rendered}");
            assert!(rendered.contains("mcp.example.com"), "{spec}: {rendered}");
        }
    }

    /// An upstream only has to embed the credential in a URL inside an error message to produce the percent-encoded spelling.
    #[test]
    fn scrub_removes_raw_and_percent_encoded_forms() {
        let key = "sk-a/b+c";
        let block: McpHttpBlock = serde_json::from_value(serde_json::json!({
            "url": "https://mcp.example.com/mcp",
            "api_key_secret": "K",
            "api_key_in": "bearer",
        }))
        .unwrap();
        let client = HttpMcpClient::new("c", &resolved(&block), &block, Some(&cred(key)));
        let encoded = percent_encode(key);
        assert_ne!(encoded, key);

        let raw_msg = client.scrub(format!("boom: {key}"));
        assert!(!raw_msg.contains(key), "{raw_msg}");

        let enc_msg = client.scrub(format!("boom: fetching https://h/x?k={encoded} failed"));
        assert!(!enc_msg.contains(&encoded), "{enc_msg}");
        assert!(enc_msg.contains("<redacted>"), "{enc_msg}");
    }

    #[test]
    fn an_echoed_authorization_header_is_scrubbed_and_a_lowercase_triplet_is_not() {
        let key = "sk-a/b+c";
        let block: McpHttpBlock = serde_json::from_value(serde_json::json!({
            "url": "https://mcp.example.com/mcp",
            "api_key_secret": "K",
            "api_key_in": "bearer",
        }))
        .unwrap();
        let client = HttpMcpClient::new("c", &resolved(&block), &block, Some(&cred(key)));

        let echoed = client.scrub(format!("rejected: Authorization: Bearer {key}"));
        assert_eq!(echoed, "rejected: Authorization: Bearer <redacted>");

        // The LOWERCASE hex spelling is not a registered literal and is not matched — pinned, not papered over.
        let lower = "sk-a%2fb%2bc";
        assert_ne!(
            lower,
            percent_encode(key),
            "fixture must differ in hex case"
        );
        assert_eq!(
            client.scrub(format!("boom: {lower}")),
            format!("boom: {lower}"),
            "if this ever starts matching, the module header's gap list is stale"
        );
    }

    /// Clamp-first leaves a prefix of the key that is no longer a literal member of `secret_forms`, so
    /// the later `replace` matches nothing. Does not reach `request`; the call-site witness is an integration test.
    #[test]
    fn key_straddling_the_truncation_boundary_is_still_redacted() {
        let client = client_with("bearer");
        // Pad so the key STARTS well before the cap and ENDS well after it.
        let head = "x".repeat(MAX_UPSTREAM_DETAIL_CHARS - LEAKY.len() / 2);
        let body = format!("{head}{LEAKY} trailing");
        assert!(
            body.chars().count() > MAX_UPSTREAM_DETAIL_CHARS,
            "fixture must exceed the cap"
        );

        // Production order: scrub, then clamp.
        let good = clamp_chars(
            scrub_with(&client.secret_forms, body.clone()),
            MAX_UPSTREAM_DETAIL_CHARS,
        );
        // Exactly the prefix the clamp-first order lets through.
        let leaked_prefix: String = LEAKY.chars().take(LEAKY.len() / 2).collect();
        assert!(leaked_prefix.len() >= 8, "prefix must be a real leak");
        assert!(
            !good.contains(&leaked_prefix),
            "partial key survived: {good}"
        );

        let bad = scrub_with(
            &client.secret_forms,
            clamp_chars(body, MAX_UPSTREAM_DETAIL_CHARS),
        );
        assert!(
            bad.contains(&leaked_prefix),
            "the clamp-first order must demonstrably leak, else this test is vacuous: {bad}"
        );
    }

    /// `""` as a scrub pattern makes `String::replace` insert the marker at every character boundary.
    #[test]
    fn no_key_registers_no_scrub_pattern_and_an_empty_one_cannot_be_built() {
        assert!(HttpCredential::parse("").is_err());
        let block: McpHttpBlock = serde_json::from_value(serde_json::json!({
            "url": "https://mcp.example.com/mcp",
        }))
        .unwrap();
        let client = HttpMcpClient::new("c", &resolved(&block), &block, None);
        assert!(
            client.secret_forms.is_empty(),
            "no key must register no pattern, got {:?}",
            client.secret_forms
        );
        let big = "a".repeat(10_000);
        assert_eq!(client.scrub(big.clone()), big, "scrub must not amplify");
    }

    #[test]
    fn clamp_marks_truncation_and_never_splits_a_char() {
        assert_eq!(clamp_chars("abc".to_string(), 5), "abc");
        assert_eq!(clamp_chars("abcde".to_string(), 5), "abcde");
        // Multi-byte: clamping at 2 chars must yield 2 chars, not 2 bytes.
        let out = clamp_chars("日本語です".to_string(), 2);
        assert!(out.starts_with("日本"), "{out}");
        assert!(out.ends_with("(truncated)"), "{out}");
    }

    /// A credential containing `":` matches JSON's key/value separator, so raw-text replacement mangles a valid document.
    #[test]
    fn a_json_shaped_credential_does_not_corrupt_an_innocent_response() {
        let key = r#"":"#;
        let forms = exact_forms(&[key]);
        let body =
            r#"{"jsonrpc":"2.0","id":1,"result":{"tools":[{"name":"t","description":"d"}]}}"#;

        let parsed = parse_scrubbed(&forms, body, "tools/list")
            .expect("a valid JSON document must survive a hostile credential");
        assert_eq!(
            parsed.pointer("/result/tools/0/description"),
            Some(&Value::String("d".into())),
            "the document must come through byte-identical: {parsed}"
        );

        let mangled = scrub_with(&forms, body.to_string());
        assert!(
            serde_json::from_str::<Value>(&mangled).is_err(),
            "the raw-text order must demonstrably corrupt this body, else the \
             assertion above proves nothing: {mangled}"
        );
    }

    #[test]
    fn a_backslash_credential_does_not_corrupt_an_escaped_response() {
        let key = r"abc\defgh";
        let forms = exact_forms(&[key]);
        // `C:\\dir` on the wire; nothing here is the credential.
        let body = r#"{"result":{"tools":[{"name":"t","description":"C:\\dir"}]}}"#;

        let parsed = parse_scrubbed(&forms, body, "tools/list").expect("must parse");
        assert_eq!(
            parsed.pointer("/result/tools/0/description"),
            Some(&Value::String(r"C:\dir".into())),
            "an escaped backslash must decode intact: {parsed}"
        );
        assert!(
            !parsed.to_string().contains("redacted"),
            "nothing was redacted here — the credential does not appear: {parsed}"
        );
    }

    /// A credential containing a newline appears JSON-escaped on the wire, so literal matching on the raw text never finds it.
    #[test]
    fn a_control_character_credential_echoed_by_the_upstream_is_still_redacted() {
        let key = "line1\nline2xx";
        let forms = exact_forms(&[key]);
        let body = serde_json::json!({
            "result": {
                "tools": [{ "name": "t", "description": format!("rejected key {key}") }],
                "content": [{ "type": "text", "text": key }],
            }
        })
        .to_string();

        let parsed = parse_scrubbed(&forms, &body, "tools/list").expect("must parse");
        let rendered = parsed.to_string();
        assert!(
            !rendered.contains("line2xx"),
            "the decoded credential survived into the tool catalog: {rendered}"
        );
        assert!(rendered.contains("<redacted>"), "{rendered}");

        let raw_scrubbed = scrub_with(&forms, body.clone());
        assert_eq!(
            raw_scrubbed, body,
            "the raw-text order must demonstrably match nothing here, else this \
             test does not distinguish the two layers"
        );
        let leaked: Value = serde_json::from_str(&raw_scrubbed).unwrap();
        assert!(
            leaked.to_string().contains("line2xx"),
            "…and the decoded secret really does reach the caller under it"
        );
    }

    #[test]
    fn object_keys_are_scrubbed_as_well_as_values() {
        let key = "sk-keyed-8213";
        let forms = exact_forms(&[key]);
        let body = format!(r#"{{"result":{{"props":{{"{key}":1,"safe":2}}}}}}"#);
        let parsed = parse_scrubbed(&forms, &body, "tools/list").expect("must parse");
        let props = parsed
            .pointer("/result/props")
            .unwrap()
            .as_object()
            .unwrap();
        assert!(!props.contains_key(key), "key survived: {parsed}");
        assert_eq!(props.get("<redacted>"), Some(&serde_json::json!(1)));
        assert_eq!(props.get("safe"), Some(&serde_json::json!(2)));
    }

    /// The entry count is asserted exactly, not as `<= 3`: if disambiguation is ever reintroduced this must go red.
    #[test]
    fn two_keys_redacting_to_the_same_marker_collapse_into_one_entry() {
        let key = "sk-a/b+c-8213";
        let block: McpHttpBlock = serde_json::from_value(serde_json::json!({
            "url": "https://mcp.example.com/mcp",
            "api_key_secret": "K",
            "api_key_in": "bearer",
        }))
        .unwrap();
        let client = HttpMcpClient::new("c", &resolved(&block), &block, Some(&cred(key)));
        let forms = client.secret_forms.clone();
        let encoded = percent_encode(key);
        assert_ne!(encoded, key, "fixture needs two distinct literals");
        assert_eq!(
            forms,
            vec![encoded.clone(), key.to_string()],
            "the constructor must register both spellings, longest first"
        );

        let mut obj = serde_json::Map::new();
        obj.insert(key.to_string(), serde_json::json!(1));
        obj.insert(encoded.clone(), serde_json::json!(2));
        obj.insert("safe".to_string(), serde_json::json!(3));
        let mut v = Value::Object(obj);
        assert_eq!(v.as_object().unwrap().len(), 3, "fixture must start with 3");

        scrub_value(&forms, &mut v);
        let obj = v.as_object().unwrap();

        for k in obj.keys() {
            assert!(!k.contains(key), "raw key survived: {v}");
            assert!(!k.contains(&encoded), "encoded key survived: {v}");
        }
        assert_eq!(
            obj.get("safe"),
            Some(&serde_json::json!(3)),
            "a key that never held the credential must be untouched: {v}"
        );
        // WHICH one survives is `BTreeMap` ordering and is deliberately not asserted.
        assert_eq!(
            obj.len(),
            2,
            "residual 1 says these two collapse; if they no longer do, the \
             rekey branch's comment is stale: {v}"
        );
        assert!(obj.contains_key("<redacted>"), "{v}");
    }

    #[test]
    fn a_rekeyed_object_still_scrubs_its_children() {
        let key = "sk-both-8213";
        let forms = exact_forms(&[key]);
        let body = serde_json::json!({
            "result": {
                key: 1,
                "hint": format!("see key {key} in the docs"),
                "nested": { "description": format!("pass {key} as the token") },
                "tools": [
                    { "name": "t", "description": format!("auth={key}") },
                ],
            }
        })
        .to_string();

        let parsed = parse_scrubbed(&forms, &body, "tools/list").expect("must parse");
        let rendered = parsed.to_string();
        assert!(
            !rendered.contains(key),
            "the credential survived in a child of a rekeyed object: {rendered}"
        );
        assert_eq!(
            parsed.pointer("/result/hint"),
            Some(&Value::String("see key <redacted> in the docs".into())),
            "sibling string value: {parsed}"
        );
        assert_eq!(
            parsed.pointer("/result/nested/description"),
            Some(&Value::String("pass <redacted> as the token".into())),
            "nested object: {parsed}"
        );
        assert_eq!(
            parsed.pointer("/result/tools/0/description"),
            Some(&Value::String("auth=<redacted>".into())),
            "object inside an array: {parsed}"
        );
    }

    /// Pins what IS registered; `%2f` and mixed-case triplets are deliberately not covered.
    #[test]
    fn registration_is_the_raw_form_and_the_uppercase_encoding_longest_first() {
        let key = "sk-a/b+c=d";
        let block: McpHttpBlock = serde_json::from_value(serde_json::json!({
            "url": "https://mcp.example.com/mcp",
            "api_key_secret": "K",
            "api_key_in": "bearer",
        }))
        .unwrap();
        let client = HttpMcpClient::new("c", &resolved(&block), &block, Some(&cred(key)));

        let upper = percent_encode(key);
        assert_eq!(upper, "sk-a%2Fb%2Bc%3Dd", "encoder shape changed");
        assert_eq!(
            client.secret_forms,
            vec![upper.clone(), key.to_string()],
            "two literals, longest first"
        );

        for spelling in [key, upper.as_str()] {
            let msg = client.scrub(format!("upstream echoed {spelling} back"));
            assert!(!msg.contains(spelling), "{spelling}: {msg}");
            assert!(msg.contains("<redacted>"), "{spelling}: {msg}");
        }

        // Matching is byte-exact: a value differing only in ASCII case is a DIFFERENT credential.
        let other = "SK-A/B+C=D";
        assert_eq!(
            client.scrub(format!("boom {other}")),
            format!("boom {other}"),
            "the raw form must not fold case"
        );
    }

    /// A credential whose reserved characters encode to letter-free triplets (`!` is `%21`) has an encoded form distinct from the raw one yet identical in every hex case.
    #[test]
    fn an_encoded_form_with_no_hex_letter_still_matches() {
        let key = "sk-abc!d!";
        let block: McpHttpBlock = serde_json::from_value(serde_json::json!({
            "url": "https://mcp.example.com/mcp",
            "api_key_secret": "K",
            "api_key_in": "bearer",
        }))
        .unwrap();
        let client = HttpMcpClient::new("c", &resolved(&block), &block, Some(&cred(key)));

        let encoded = percent_encode(key);
        assert_eq!(
            encoded, "sk-abc%21d%21",
            "fixture needs letter-free triplets"
        );
        assert!(
            encoded
                .split('%')
                .skip(1)
                .all(|t| t.as_bytes()[..2].iter().all(u8::is_ascii_digit)),
            "fixture must have letter-free triplets: {encoded}"
        );
        assert_ne!(encoded, key, "…but still distinct from the raw form");
        assert_eq!(client.secret_forms.len(), 2, "{:?}", client.secret_forms);

        let msg = client.scrub(format!("boom {encoded}"));
        assert!(!msg.contains(&encoded), "{msg}");
        assert!(msg.contains("<redacted>"), "{msg}");
    }

    /// The dedupe must leave exactly ONE pattern; the `Bearer ` prefix is a wire detail, not a scrub form.
    #[test]
    fn an_unreserved_credential_registers_exactly_one_form() {
        let block: McpHttpBlock = serde_json::from_value(serde_json::json!({
            "url": "https://mcp.example.com/mcp",
            "api_key_secret": "K",
            "api_key_in": "header:x-api-key",
        }))
        .unwrap();
        let client = HttpMcpClient::new("c", &resolved(&block), &block, Some(&cred("abcdefgh")));
        assert_eq!(client.secret_forms, vec!["abcdefgh".to_string()]);
    }

    /// Deeply nested strings are reached, and a no-key client is a no-op.
    #[test]
    fn nesting_is_traversed_and_no_key_means_no_walk() {
        let key = "sk-nested-8213";
        let forms = exact_forms(&[key]);
        let body = format!(r#"{{"result":{{"a":[{{"b":[["{key}"]]}}]}}}}"#);
        let parsed = parse_scrubbed(&forms, &body, "x").unwrap();
        assert_eq!(
            parsed.pointer("/result/a/0/b/0/0"),
            Some(&Value::String("<redacted>".into()))
        );

        let untouched = parse_scrubbed(&[], &body, "x").unwrap();
        assert!(untouched.to_string().contains(key));
    }

    /// Every case clears `MIN_CREDENTIAL_LEN` and every other rule, so a refusal here can only be the marker rule.
    #[test]
    fn a_credential_that_overlaps_the_marker_is_refused() {
        let refused = [
            // Case 4 — the credential is a piece of the marker family.
            ("redacted", "the marker's body"),
            ("<redacte", "a prefix of the marker"),
            ("edacted>", "a suffix of the marker"),
            ("<redacted>", "the marker itself"),
            ("redacted>", "an interior slice reaching the end"),
            ("edacted>#", "reaching into the `#` of a suffixed name"),
            ("<redacted>#2", "a whole suffixed name"),
            ("acted>#12", "straddling the `#` into a two-digit suffix"),
            // Case 1 — the credential BEGINS with a suffix of the marker.
            ("redacted>y", "round 3's accepted leak, reproduced below"),
            (">y-abcdef", "one marker character is enough"),
            ("d>abcdefg", "two"),
            // Case 2 — the credential ENDS with a prefix of the marker.
            ("abcdef-<", "one marker character is enough"),
            ("abcde<re", "three"),
            // Case 3 — the credential contains the whole marker.
            ("sk-<redacted>-x", "round 3 accepted this one"),
            ("<redacted>#2x", "and this one"),
        ];
        let mut wrong: Vec<String> = Vec::new();
        for (bad, why) in refused {
            assert!(
                bad.len() >= MIN_CREDENTIAL_LEN,
                "{why}: fixture must clear the length floor, else this case \
                 proves nothing about the marker rule"
            );
            match HttpCredential::parse(bad) {
                Ok(_) => wrong.push(format!("{bad:?} ({why}): ACCEPTED")),
                Err(e) if !e.contains("overlaps the marker") => {
                    wrong.push(format!("{bad:?} ({why}): refused by another rule: {e}"));
                }
                Err(e) => {
                    // Operator-facing. Several of these values are substrings of
                    // the marker, so the message may quote neither them nor it.
                    assert!(!e.contains(bad), "{why}: the refusal quotes it: {e}");
                    assert!(
                        !e.contains(REDACTED),
                        "{why}: the refusal spells the marker, which for the \
                         substring cases is quoting the credential: {e}"
                    );
                }
            }
        }
        assert!(
            wrong.is_empty(),
            "these credentials overlap the marker and are still registrable, so \
             one upstream response can re-form them out of their own \
             redaction:\n  {}",
            wrong.join("\n  ")
        );

        // Stated positively, so the rule cannot pass by refusing everything.
        for (good, why) in [
            ("redactedX", "shares a prefix, shares no edge"),
            ("my-redacted-key", "the marker's body inside a longer key"),
            ("sk-live-abcdefgh", "an ordinary key"),
            ("a/b+c=d&e", "punctuation an API key really uses"),
        ] {
            assert!(
                HttpCredential::parse(good).is_ok(),
                "{why}: {good:?} must still be accepted — this rule refuses text \
                 shared with the marker, not everything that resembles it"
            );
        }
    }

    #[test]
    fn each_marker_overlap_clause_has_a_witness_no_other_clause_catches() {
        // Case 4 — a piece of the marker family, and nothing else.
        let only_family = "redacted";
        assert!(is_marker_family_substring(only_family));
        assert!(!tail_is_marker_prefix(only_family));
        assert!(!head_is_marker_suffix(only_family));
        assert!(!only_family.contains(REDACTED));

        // Case 2 — the tail is a prefix of the marker, and nothing else.
        let only_tail = "abcde<re";
        assert!(tail_is_marker_prefix(only_tail));
        assert!(!is_marker_family_substring(only_tail));
        assert!(!head_is_marker_suffix(only_tail));
        assert!(!only_tail.contains(REDACTED));

        // Case 1 — the head is a suffix of the marker, and nothing else.
        let only_head = ">y-abcdef";
        assert!(head_is_marker_suffix(only_head));
        assert!(!is_marker_family_substring(only_head));
        assert!(!tail_is_marker_prefix(only_head));
        assert!(!only_head.contains(REDACTED));

        // Case 3 — contains the marker, and nothing else.
        let only_contains = "sk-<redacted>-x";
        assert!(only_contains.contains(REDACTED));
        assert!(!is_marker_family_substring(only_contains));
        assert!(!tail_is_marker_prefix(only_contains));
        assert!(!head_is_marker_suffix(only_contains));

        // All four reach the guard.
        for w in [only_family, only_tail, only_head, only_contains] {
            assert!(overlaps_redaction_marker(w), "{w:?}");
        }
        // …and a credential sharing no text with the marker does not.
        assert!(!overlaps_redaction_marker("sk-live-abcdefgh"));
    }

    /// Through `parse` these near misses are useless: `<redacted>x` is not a family substring but IS caught by the contains clause.
    #[test]
    fn the_marker_family_predicate_splits_at_the_trailing_digit_run() {
        // Case 1 — wholly inside `<redacted>#`.
        assert!(is_marker_family_substring("<redacted>#"));
        assert!(is_marker_family_substring("d>"));
        // Case 2 — all digits (also refused earlier, as number-shaped).
        assert!(is_marker_family_substring("12"));
        // Case 3 — head is a suffix of `<redacted>#`, tail is digits.
        assert!(is_marker_family_substring("<redacted>#987"));
        assert!(is_marker_family_substring(">#4"));
        // …and the near misses on each case.
        assert!(
            !is_marker_family_substring("<redacted>x"),
            "not a substring: the marker is not followed by `x`"
        );
        assert!(
            !is_marker_family_substring("<redacted#2"),
            "a fragment with a character removed is not a fragment"
        );
        assert!(
            !is_marker_family_substring("redacted>2"),
            "the digit must sit behind the `#`, not against `>`"
        );
        assert!(!is_marker_family_substring("sk-abc-8213"));
    }

    #[test]
    fn mcp_setup_scrubs_all_private_header_values() {
        let block: McpHttpBlock =
            serde_json::from_value(json!({"url":"https://mcp.example.com/"})).unwrap();
        let headers =
            super::super::http_headers::HttpHeaders::parse(std::collections::BTreeMap::from([
                ("X-Tenant".to_string(), "tenant-team".to_string()),
                ("X-Second".to_string(), "private-12345".to_string()),
                (
                    "Authorization".to_string(),
                    "Bearer sk-private-quoted".to_string(),
                ),
            ]))
            .unwrap();
        let client = HttpMcpClient::new("c", &resolved(&block), &block, None).with_headers(headers);
        let value = parse_scrubbed(&client.secret_forms,
            r#"{"result":{"tenant-team":"sk-private-quoted","second":"private-12345","nested":["Bearer sk-private-quoted"]}}"#, "tools/list").unwrap();
        let output = value.to_string();
        for secret in ["tenant-team", "private-12345", "sk-private-quoted"] {
            assert!(!output.contains(secret), "{output}");
        }
        assert!(!format!("{client:?}").contains("sk-private-quoted"));
    }

    #[test]
    fn mcp_setup_scrubbing_arbitrary_headers_cannot_reform_a_private_value() {
        let leaky = "redacted>y";
        assert!(HttpCredential::parse(leaky).is_err());
        let once = scrub_with(&exact_forms(&[leaky]), "hit redacted>yy end".to_string());
        assert!(
            !once.contains(leaky),
            "header value re-formed across the marker: {once}"
        );

        // Driven through the production constructor so the registered form list is the real one.
        let good = "redactedZy";
        let block: McpHttpBlock = serde_json::from_value(serde_json::json!({
            "url": "https://mcp.example.com/mcp",
            "api_key_secret": "K",
            "api_key_in": "bearer",
        }))
        .unwrap();
        let client = HttpMcpClient::new("c", &resolved(&block), &block, Some(&cred(good)));
        let body = format!("hit {good}y end");
        let scrubbed = client.scrub(body);
        assert!(
            !scrubbed.contains(good),
            "one pass left the credential in the output: {scrubbed}"
        );
        assert_eq!(
            client.scrub(scrubbed.clone()),
            scrubbed,
            "a second pass changed the string, so the scrubber is not idempotent"
        );

        // The encoding can carry no `<` or `>`, so it cannot overlap the marker either.
        let two_form = "sk-a/b+c=d";
        let client = HttpMcpClient::new("c", &resolved(&block), &block, Some(&cred(two_form)));
        assert_eq!(client.secret_forms.len(), 2);
        for spelling in [two_form.to_string(), percent_encode(two_form)] {
            let out = client.scrub(format!("upstream echoed {spelling}!"));
            for form in &client.secret_forms {
                assert!(
                    !out.contains(form.as_str()),
                    "{spelling}: a registered form survived one pass: {out}"
                );
            }
            assert_eq!(client.scrub(out.clone()), out, "{spelling}: not idempotent");
        }
    }

    /// `Manifest::validate` makes this arm unreachable through the manifest route; driving the constructor directly is the only way to reach it.
    #[test]
    fn an_unroutable_api_key_in_sends_the_credential_nowhere() {
        let key = "sk-unrouted-8213";
        for placement in ["query:api_key", "cookie:k", "body:token", "api_key"] {
            let block = McpHttpBlock {
                url: "https://mcp.example.com/mcp".to_string(),
                api_key_secret: Some("K".to_string()),
                api_key_in: Some(placement.to_string()),
                header_secrets: std::collections::BTreeMap::new(),
                tools_all: false,
                tools_allow: Vec::new(),
                request_timeout_ms: None,
                bringup_timeout_ms: None,
            };
            let client = HttpMcpClient::new("c", &resolved(&block), &block, Some(&cred(key)));
            assert_eq!(client.header_auth, None, "`{placement}` invented a header");
            assert_eq!(
                client.url, "https://mcp.example.com/mcp",
                "`{placement}` touched the url"
            );
            // Unrouted is not a reason to stop redacting: the secret was read
            // off disk and may already be in a string somewhere.
            assert_eq!(client.secret_forms, vec![key.to_string()], "`{placement}`");
            assert!(
                format!("{client:?}").contains("unrouted:<redacted>"),
                "`{placement}`: {client:?}"
            );
        }
    }

    #[test]
    fn scrub_is_identity_without_a_key() {
        let block: McpHttpBlock = serde_json::from_value(serde_json::json!({
            "url": "https://mcp.example.com/mcp",
        }))
        .unwrap();
        let client = HttpMcpClient::new("c", &resolved(&block), &block, None);
        assert_eq!(client.scrub("plain".to_string()), "plain");
        assert!(format!("{client:?}").contains("none"));
    }
}
