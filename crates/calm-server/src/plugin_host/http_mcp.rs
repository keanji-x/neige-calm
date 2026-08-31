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
//! **Timeouts are a correctness requirement, not a nicety, and there are TWO
//! of them because they answer opposite questions.** `tools/list` runs inside
//! `spawn_admitted`, which `AppState::new` awaits inline via
//! `autospawn_enabled`: while bring-up runs, the server does not serve, so the
//! bring-up deadline must be short and hard-bounded
//! ([`super::manifest::MCP_HTTP_MAX_BRINGUP_TIMEOUT_MS`], enforced at manifest
//! parse time). A steady-state `tools/call` is on nobody's boot path and may
//! legitimately take minutes, so its deadline is generous and uncapped. One
//! knob carrying both constraints is what produced three successive rounds of
//! "adjust the arithmetic, watch the defect reappear one level up"; see that
//! constant for the history.
//!
//! Neither deadline is a TOTAL bound on bring-up: `initialize` and `tools/list`
//! are two round trips, and `spawn_blocking` queue delay plus DNS sit outside
//! ureq's own clock. There are two bounds above this one:
//!
//! * `PluginHost::spawn_mcp_http` wraps ONE connector's bring-up in a
//!   `tokio::time::timeout` of `2 × bringup_timeout_ms + slack` — a multiple,
//!   because the configured value is per-REQUEST and this path makes two
//!   requests. Since `bringup_timeout_ms` has a validated ceiling, this
//!   product has one too;
//! * `PluginHost::autospawn_enabled` bounds the connector portion of boot as a
//!   WHOLE — spawn, reconciliation and every persisted emission, under one
//!   `connector_phase_ceiling`. Bring-up is still inline and still serial
//!   (acceptance §4 #7 needs materialization to precede the boot audit's
//!   `exposes_tools` read), so without that second bound N unreachable
//!   connectors would still cost N × the per-connector cap — and without it
//!   covering the emissions too, a slow event store would cost N × a repo
//!   write on top.
//!
//! **The API key must never reach a string a human or an agent can read.** It
//! rides in the URL's query string, and `ureq::Error`'s `Display` prints that
//! URL first — so a `{e}` anywhere in this module puts the credential into the
//! `tracing` line, the persisted+broadcast `Event::PluginState.last_error`, the
//! `POST /enable` 503 body, and the wave transcript. Three rules, all enforced
//! by tests: format a `ureq::Error` only via `kind()`; run every outgoing
//! string — error OR success — through [`HttpMcpClient::scrub`]; and
//! **scrub before you truncate**, never after.
//!
//! The middle rule is enforced at ONE choke point, in
//! [`HttpMcpClient::request`]. Scrubbing only the two identity strings
//! `initialize` logs would leave the success path open: `tools/list`
//! descriptions become `ExposedTool` entries that agents and operators read,
//! and a `tools/call` result reaches the wave transcript — both are
//! upstream-authored and both could echo our query string back.
//!
//! **That choke point sits AFTER the parse, not before it** ([`parse_scrubbed`]
//! → [`scrub_value`]). Literal-replacing a credential in the raw response text
//! is the wrong layer, and it is wrong in both directions:
//!
//! * it **corrupts valid JSON** whenever the credential happens to look like
//!   JSON structure. A credential of `":` matches the separator between a key
//!   and a string value; one containing `\` collides with escape pairs in
//!   documents that never echoed the credential at all; a short one (`e`)
//!   breaks bare tokens like `true`; one that is a substring of a field name
//!   silently deletes part of that key. All of those turn a healthy upstream
//!   into "malformed JSON";
//! * it **misses the secret it exists to catch**. A credential containing a
//!   newline or any control character appears JSON-*escaped* in the raw text
//!   (`\n`), so a literal search does not find it — and the DECODED secret then
//!   reaches `tools/list` descriptions and `tools/call` results on the success
//!   path. That is a leak, not merely corruption.
//!
//! Parsing first and then walking the tree — string values AND object keys —
//! removes both: every string the module can hand back is scrubbed in its
//! decoded form, and the document's syntax is never touched.
//!
//! **The shapes the scrubber cannot handle are refused before a client exists**
//! ([`HttpCredential::parse`]). That check is on the constructor's parameter
//! type, not in `read_secrets`, for two reasons: `HttpMcpClient::new` is
//! `pub` and used to accept any `&str`, so the constraint could be bypassed by
//! constructing a client in-process; and a `cli-query` secret is not on the
//! HTTP-redaction path at all and has no business obeying HTTP-redaction rules.
//! A credential that is entirely digits is refused there too — `scrub_value`
//! deliberately does not touch JSON numbers (coercing them would corrupt the
//! payload), so an upstream echoing an all-digit key back as a number in
//! `structuredContent` would put it in the tool result and the wave transcript.
//! Constraining the credential removes the case; there is nothing left to
//! classify as residual.
//!
//! The raw-text scrub survives in exactly one place: the non-2xx arm, where the
//! body is an arbitrary error page with nothing to parse. There the
//! scrub-before-truncate rule applies and is not pedantry — an upstream that
//! echoes the request URL back inside a long error body gets truncated to 512
//! chars, and if that boundary falls inside the key the surviving prefix is no
//! longer a literal member of `secret_forms`, so a later `replace` misses it
//! entirely and a partial credential ships.

use std::io::Read as _;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
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

/// How much of an upstream error body survives into the operator-facing
/// message. Applied strictly AFTER [`scrub_with`] — see the module header.
///
/// `pub` so the end-to-end leak test can position a key across THIS boundary
/// rather than a copy of the number that could silently stop lining up.
pub const MAX_UPSTREAM_DETAIL_CHARS: usize = 512;

/// Cap on upstream-authored identity strings we record in a `tracing` line
/// (`serverInfo.name`, `protocolVersion`). Bounded and scrubbed, because both
/// are attacker-controlled text that lands in the operator's log.
const MAX_SERVER_IDENT_CHARS: usize = 128;

/// Shortest credential this client will carry. Not a strength requirement — it
/// is a *scrubbing* requirement: redaction is by literal match, so a short value
/// matches constantly inside unrelated upstream text and turns redaction into
/// corruption (a one-character credential of `e` rewrites every `true`).
pub const MIN_CREDENTIAL_LEN: usize = 8;
/// The TCP-connect deadline is never allowed below this.
///
/// Connect is a THIRD phase, and it belongs to neither [`Phase`]: it is network
/// RTT plus a TLS handshake, work that is identical whether the tool behind it
/// answers in 200 ms or ten minutes. ureq resolves `timeout_connect` **ahead of**
/// the per-request deadline (`connect_host` in ureq 2.12 builds its connect
/// deadline from `timeout_connect` when set and only falls back to the request
/// deadline otherwise), so it cannot be relaxed per call: setting it from the
/// bring-up budget alone made every steady-state `tools/call` subject to the
/// bring-up deadline the moment the pooled connection was cold or idle-expired.
/// The repo's own fixture (`bringup 400 ms`, `request 600 s`) would have failed
/// against any real TLS endpoint; the long-call test misses it only because it
/// reuses the connection bring-up opened.
///
/// Raising the floor does not weaken any boot bound: boot is bounded by
/// `tokio::time::timeout` in `spawn_mcp_http` and by the connector-phase fence
/// in `autospawn_enabled_within`, neither of which delegates to ureq's clock.
/// The cost of a black-holed host is that the abandoned blocking-pool closure
/// can linger for this long after the async caller has given up — one thread,
/// bounded, and it already could for the read phase.
pub const CONNECT_TIMEOUT_FLOOR: Duration = Duration::from_secs(10);

/// A credential that has been checked against everything the HTTP path's
/// redaction machinery requires of it.
///
/// **This type is the invariant.** [`HttpMcpClient::new`] takes one instead of a
/// `&str`, and the only way to obtain one is [`HttpCredential::parse`], so no
/// call site — including one in a future module, or a test — can register a
/// scrub pattern that the scrubber cannot handle. Putting the same rules in
/// `read_secrets` did not achieve that: `HttpMcpClient::new` is `pub` and was
/// reachable with an arbitrary `&str`, and `read_secrets` simultaneously applied
/// HTTP-redaction rules to `cli-query` secrets that never touch this path.
#[derive(Clone)]
pub struct HttpCredential(String);

impl HttpCredential {
    /// The four rules, each tied to a concrete failure of the redaction layer:
    ///
    /// * **non-empty** — an empty value registers `""` as a scrub pattern, and
    ///   `"".replace("", "<redacted>")` inserts the marker at EVERY character
    ///   boundary: a 4 MiB upstream error body expands by an order of magnitude
    ///   and is then cloned into the live status entry, the broadcast
    ///   `PluginState` event, and the HTTP error body;
    /// * **printable ASCII, no space, no `"`, no `\`** — a credential containing
    ///   a newline or a control character appears JSON-*escaped* on the wire, so
    ///   the raw-text matcher used on the non-2xx arm never finds it; quote and
    ///   backslash are the two characters that collide with JSON syntax there.
    ///   None of them appear in any real API key;
    /// * **at least [`MIN_CREDENTIAL_LEN`] characters** — see that constant;
    /// * **at least one non-digit** — [`scrub_value`] does not descend into JSON
    ///   numbers, so an all-digit credential echoed back as `{"key": 12345678}`
    ///   in `structuredContent` reaches the tool result and the wave transcript
    ///   unredacted. Making numbers scrubbable is not an option (it would
    ///   rewrite unrelated values and change the payload's type); making the
    ///   credential un-number-like is.
    ///
    /// The error text never quotes the credential — only the single offending
    /// character class — because this string is operator-facing and lands in
    /// `PluginState.last_error`.
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
        if raw.len() < MIN_CREDENTIAL_LEN {
            return Err(format!(
                "is shorter than {MIN_CREDENTIAL_LEN} characters, which would make \
                 redaction match unrelated upstream text"
            ));
        }
        if raw.chars().all(|c| c.is_ascii_digit()) {
            return Err(
                "is entirely digits: an upstream is free to echo such a value back as \
                 a JSON *number*, which carries no string to redact, so it would reach \
                 tool results and wave transcripts in the clear. Add a non-digit \
                 character to the credential."
                    .to_string(),
            );
        }
        Ok(Self(raw.to_string()))
    }

    fn as_str(&self) -> &str {
        &self.0
    }
}

/// Redacting, so a credential cannot reach a log line through a `{:?}` on a
/// struct that happens to hold one.
impl std::fmt::Debug for HttpCredential {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("HttpCredential(<redacted>)")
    }
}

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
    /// Deadline for `initialize` + `tools/list` — the pair on the inline-awaited
    /// boot path. Hard-capped at manifest parse time.
    bringup_timeout: Duration,
    /// Deadline for a steady-state `tools/call`. Uncapped on purpose.
    call_timeout: Duration,
    next_id: std::sync::atomic::AtomicU64,
}

/// Which of the two budgets a round trip is spending. There is no default:
/// picking the wrong one is the whole defect class this type exists to make
/// unrepresentable, so every call site names it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    /// `initialize` / `tools/list` — bounded, because boot waits on it.
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
    ///
    /// The parameter is an [`HttpCredential`], not a `&str`, and that is the
    /// whole point: every constraint the redaction layer depends on is
    /// discharged by the only constructor of that type, so this function cannot
    /// be handed a value it cannot scrub — from here, from a test, or from a
    /// module that does not exist yet.
    pub fn new(plugin_id: &str, block: &McpHttpBlock, api_key: Option<&HttpCredential>) -> Self {
        let base = block.url.trim().to_string();
        let mut url = base.clone();
        let mut header_auth = None;
        let mut secret_forms = Vec::new();

        if let Some(key) = api_key.map(HttpCredential::as_str) {
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
            //
            // The `is_empty` filters are the braces to `HttpCredential`'s belt:
            // an empty pattern turns `scrub` into a memory amplifier (see
            // `scrub_with`), and this is the last place that could register
            // one. `percent_encode("")` is also `""`, hence both guards.
            let encoded = percent_encode(key);
            if !key.is_empty() {
                secret_forms.push(key.to_string());
            }
            if encoded != key && !encoded.is_empty() {
                secret_forms.push(encoded);
            }
            secret_forms.sort_by_key(|s| std::cmp::Reverse(s.len()));
        }

        let bringup_timeout = Duration::from_millis(block.bringup_timeout_ms());
        let call_timeout = Duration::from_millis(block.timeout_ms());
        Self {
            plugin_id: plugin_id.to_string(),
            log_target: log_target(&base),
            url,
            header_auth,
            secret_forms,
            agent: ureq::AgentBuilder::new()
                // Connect gets its OWN floor because it is a third phase with a
                // third constraint — see [`CONNECT_TIMEOUT_FLOOR`]. It is not
                // the bring-up budget (which would cut off a cold connection on
                // the `tools/call` path, since ureq will not let a per-request
                // deadline relax `timeout_connect`) and it is not the call
                // budget (which is uncapped, and would hand a black-holed host
                // an operator-controlled stall). The `max` keeps an operator who
                // deliberately configures a LONGER bring-up in charge of it.
                //
                // The overall per-request deadline is NOT set here: it is
                // supplied per call from [`Phase`], because the two phases have
                // opposite constraints. Neither is a TOTAL bound — the caller
                // wraps the whole connector spawn in one outer
                // `tokio::time::timeout` (§2.2).
                .timeout_connect(bringup_timeout.max(CONNECT_TIMEOUT_FLOOR))
                .build(),
            bringup_timeout,
            call_timeout,
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
        scrub_with(&self.secret_forms, s)
    }

    /// Scrub, THEN clamp to `max` chars. Order matters — see the module header.
    fn scrub_and_clamp(&self, s: &str, max: usize) -> String {
        clamp_chars(self.scrub(s.to_string()), max)
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
        let result = self.request(Phase::Bringup, "initialize", params).await?;
        // BOTH of these are upstream-authored. The success path is not exempt
        // from the module's scrub invariant: a server that echoes our query
        // string into `serverInfo.name` would otherwise put the API key in the
        // operator's log, and an unbounded `name` would let it flood the log.
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
        let result = self
            .request(Phase::Bringup, "tools/list", json!({}))
            .await?;
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

    /// One JSON-RPC round trip.
    ///
    /// The blocking HTTP call is moved onto the blocking pool so the async
    /// runtime keeps turning; the deadline is enforced by the client itself,
    /// so the blocking task cannot outlive `phase`'s budget by more than
    /// scheduling jitter.
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
        let agent = self.agent.clone();
        let method_owned = method.to_string();
        let target = self.log_target.clone();
        let deadline = match phase {
            Phase::Bringup => self.bringup_timeout,
            Phase::Call => self.call_timeout,
        };
        // Scrubbing the non-2xx body must happen INSIDE the closure, before the
        // 512-char clamp below (module header). Cloning a handful of short
        // strings per request is cheaper than the class of bug the alternative
        // admits.
        let secret_forms = self.secret_forms.clone();

        // Dropping a `spawn_blocking` JoinHandle does NOT cancel the closure:
        // the caller's outer `tokio::time::timeout` around connector bring-up
        // (`PluginHost::spawn_mcp_http`) can elapse while this request is still
        // queued, and without this flag the closure would later fire a
        // credential-bearing request at an upstream whose connector is already
        // `Unavailable`/disabled/uninstalled — once per re-enable, forever.
        // The guard trips on ANY drop of this future; on the normal path it is
        // dropped only after the `.await` below has already joined the closure.
        let cancelled = Arc::new(AtomicBool::new(false));
        let _cancel_on_drop = CancelOnDrop(Arc::clone(&cancelled));
        let closure_cancel = Arc::clone(&cancelled);

        let text = tokio::task::spawn_blocking(move || {
            if closure_cancel.load(Ordering::SeqCst) {
                return Err("request abandoned before it was sent (caller went away)".to_string());
            }
            let mut req = agent
                .post(&url)
                // Per-request, from `phase`: `Request::timeout` overrides the
                // agent's read/write deadlines but NOT `timeout_connect`, which
                // is exactly the split we want.
                .timeout(deadline)
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
                    // The one surviving RAW-text scrub, and the right layer for
                    // it: a non-2xx body is an arbitrary error page (HTML, a
                    // proxy banner, plain text) with no JSON tree to walk.
                    // SCRUB, then clamp. Reversing these two lines is the
                    // partial-key leak the module header describes.
                    let detail = scrub_with(&secret_forms, read_capped(resp).unwrap_or_default());
                    let detail = clamp_chars(detail, MAX_UPSTREAM_DETAIL_CHARS);
                    Err(format!("HTTP {code} from {target}: {detail}"))
                }
                Err(e) => Err(format!("request to {target} failed: {}", e.kind())),
            }
        })
        .await
        // Never format the `JoinError` payload: a panic payload is an
        // arbitrary string built from arbitrary locals, and this is the one
        // arm that does not pass through `scrub`. `is_cancelled`/`is_panic`
        // carry every bit of information the operator can act on anyway.
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

        // THE choke point for the module's "every outgoing string is scrubbed"
        // rule (see the module header). Everything below — `result`, the
        // `tools/list` catalog that becomes `ExposedTool` descriptions, a
        // `tools/call` payload bound for the wave transcript, and the
        // upstream-authored `error.message` — is derived from this value, and
        // it is scrubbed as a JSON TREE (decoded strings and object keys),
        // never as raw text.
        let parsed = parse_scrubbed(&self.secret_forms, &text, &method_owned)?;

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
            // we can actually recognize). `parse_scrubbed` already walked it —
            // this is a decoded string value inside the tree it scrubbed.
            return Err(RpcError::custom(code, message));
        }
        parsed.get("result").cloned().ok_or_else(|| {
            RpcError::internal(format!(
                "mcp-http {method_owned}: response had neither `result` nor `error`"
            ))
        })
    }
}

/// Strip the SSE envelope, parse, and scrub the resulting JSON **tree**.
///
/// This is the production choke point for the success path — `request` calls
/// exactly this and does nothing else with the raw body — so a unit test that
/// drives this function is driving the real code, not a re-implementation of
/// it. See the module header for why the scrub must not happen on the raw text.
///
/// Recursion is bounded by `serde_json`'s own 128-level nesting limit, which
/// rejects deeper documents before this function ever sees them; an
/// attacker-controlled body therefore cannot drive this into a stack overflow.
fn parse_scrubbed(forms: &[String], text: &str, method: &str) -> Result<Value, RpcError> {
    let payload = strip_sse_envelope(text).ok_or_else(|| {
        RpcError::internal(format!(
            "mcp-http {method}: response carried no JSON payload"
        ))
    })?;
    // `serde_json::Error`'s Display carries a line/column and a category, never
    // the input — but it is passed through `scrub_with` anyway, because "this
    // error text is safe" is exactly the assumption this module refuses to make
    // anywhere else.
    let mut parsed: Value = serde_json::from_str(payload).map_err(|e| {
        RpcError::internal(scrub_with(
            forms,
            format!("mcp-http {method}: malformed JSON: {e}"),
        ))
    })?;
    scrub_value(forms, &mut parsed);
    Ok(parsed)
}

/// Recursively replace every literal in `forms` inside a JSON tree — string
/// values **and object keys**.
///
/// Keys matter as much as values: an upstream is free to answer
/// `{"<our-api-key>": "…"}`, and a `tools/list` entry's `inputSchema`
/// properties are object keys that reach `ExposedTool` and, from there, the
/// agent-visible tool catalog.
fn scrub_value(forms: &[String], v: &mut Value) {
    if forms.is_empty() {
        return;
    }
    match v {
        // `scrub_with` already skips the allocation when nothing matches, so
        // this arm costs one substring search per form per string on the clean
        // path — the same shape as the raw-text scrub it replaced, minus the
        // 4 MiB `String::replace` copies.
        Value::String(s) => *s = scrub_with(forms, std::mem::take(s)),
        Value::Array(items) => {
            for item in items {
                scrub_value(forms, item);
            }
        }
        Value::Object(map) => {
            let rekey = map
                .keys()
                .any(|k| forms.iter().any(|f| k.contains(f.as_str())));
            if rekey {
                let taken = std::mem::take(map);
                *map = taken
                    .into_iter()
                    .map(|(k, v)| (scrub_with(forms, k), v))
                    .collect();
            }
            for (_, child) in map.iter_mut() {
                scrub_value(forms, child);
            }
        }
        // Numbers, booleans and null carry no string to redact. An all-digit
        // credential — the one value an upstream could echo back HERE, as a
        // JSON number — cannot exist: `HttpCredential::parse` refuses it.
        _ => {}
    }
}

/// Trips its flag when dropped. See the comment at its construction site in
/// [`HttpMcpClient::request`].
struct CancelOnDrop(Arc<AtomicBool>);

impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        self.0.store(true, Ordering::SeqCst);
    }
}

/// Replace every literal in `forms` with `<redacted>`.
///
/// Free function (not a method) so the blocking closure in
/// [`HttpMcpClient::request`] can scrub before truncating without capturing
/// `&self`.
///
/// **Empty forms are impossible by construction** — [`HttpMcpClient::new`]
/// refuses to register one, and [`HttpCredential::parse`] refuses an empty
/// credential upstream of that. Both halves matter: `"".replace("", "<redacted>")` inserts
/// the marker at every character boundary, so a 4 MiB upstream error body would
/// expand ~11× and then be cloned into the status entry, the broadcast event,
/// and the HTTP error body. The `debug_assert` keeps the invariant honest if a
/// future caller builds `secret_forms` some other way.
fn scrub_with(forms: &[String], s: String) -> String {
    let mut out = s;
    for form in forms {
        debug_assert!(!form.is_empty(), "an empty scrub pattern is a memory bomb");
        if form.is_empty() {
            continue;
        }
        if out.contains(form.as_str()) {
            out = out.replace(form.as_str(), "<redacted>");
        }
    }
    out
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

    /// Shorthand for a credential the HTTP path will accept.
    fn cred(raw: &str) -> HttpCredential {
        HttpCredential::parse(raw)
            .unwrap_or_else(|e| panic!("{raw:?} must be a valid credential: {e}"))
    }

    /// The invariant `HttpMcpClient::new`'s parameter type carries: every shape
    /// the redaction layer cannot handle is refused before a client exists.
    ///
    /// Each case is a concrete failure of the scrubber, not a style rule — see
    /// [`HttpCredential::parse`]. The all-digit case is round-5 finding 3: the
    /// tree scrub does not descend into JSON numbers, so an upstream echoing
    /// such a value back as a number leaks it in full.
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
        ];
        for (bad, why) in cases {
            let err = HttpCredential::parse(bad)
                .err()
                .unwrap_or_else(|| panic!("{why}: {bad:?} must be refused"));
            // The refusal reaches `PluginState.last_error`; it may name the
            // rule, never the value. (Only meaningful for values long enough
            // to be a credential: `"e"` occurs in English prose.)
            if bad.len() >= MIN_CREDENTIAL_LEN {
                assert!(!err.contains(bad), "{why}: the refusal quotes it: {err}");
            }
        }
    }

    /// Stated positively, so the test above cannot pass by refusing
    /// everything — including the boundary of each numeric rule.
    #[test]
    fn a_well_formed_credential_is_accepted() {
        for good in [
            "a".repeat(MIN_CREDENTIAL_LEN),     // exactly the floor: `<`, not `<=`
            "sk-super-secret-8213".to_string(), // the real shape
            "1234567a".to_string(),             // digits are fine WITH a non-digit
            "a/b+c=d&e".to_string(),            // punctuation an API key really uses
        ] {
            assert!(
                HttpCredential::parse(&good).is_ok(),
                "{good:?} must be accepted"
            );
        }
    }

    /// Connect is a third phase with its own constraint: ureq resolves
    /// `timeout_connect` ahead of the per-request deadline, so a bring-up-sized
    /// connect timeout would make every `tools/call` subject to the bring-up
    /// budget the moment the pooled connection is cold.
    #[test]
    fn the_connect_deadline_is_never_below_its_own_floor() {
        let block: McpHttpBlock = serde_json::from_value(serde_json::json!({
            "url": "https://mcp.example.com/mcp",
            "bringup_timeout_ms": 400,
            "request_timeout_ms": 600_000,
        }))
        .unwrap();
        let client = HttpMcpClient::new("c", &block, None);
        // The fixture that would have failed against a real TLS endpoint.
        assert_eq!(client.bringup_timeout, Duration::from_millis(400));
        assert!(
            CONNECT_TIMEOUT_FLOOR > client.bringup_timeout,
            "the floor must actually be doing something for this fixture"
        );
        // …and it is never the (uncapped) call budget either.
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

    #[test]
    fn query_key_is_appended_and_encoded_and_never_in_log_target() {
        let block: McpHttpBlock = serde_json::from_value(serde_json::json!({
            "url": "https://mcp.example.com/mcp",
            "api_key_secret": "K",
            "api_key_in": "query:api_key",
        }))
        .unwrap();
        let client = HttpMcpClient::new("c", &block, Some(&cred("sk/a+b=c")));
        assert_eq!(
            client.url,
            "https://mcp.example.com/mcp?api_key=sk%2Fa%2Bb%3Dc"
        );
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
        let client = HttpMcpClient::new("c", &block, Some(&cred("abcdefgh")));
        assert_eq!(
            client.url,
            "https://mcp.example.com/mcp?v=1&api_key=abcdefgh"
        );
    }

    #[test]
    fn header_key_does_not_touch_the_url() {
        let block: McpHttpBlock = serde_json::from_value(serde_json::json!({
            "url": "https://mcp.example.com/mcp",
            "api_key_secret": "K",
            "api_key_in": "header:x-api-key",
        }))
        .unwrap();
        let client = HttpMcpClient::new("c", &block, Some(&cred("abcdefgh")));
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
        HttpMcpClient::new("c", &block, Some(&cred(LEAKY)))
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
        let key = "sk-a/b+c";
        let block: McpHttpBlock = serde_json::from_value(serde_json::json!({
            "url": "https://mcp.example.com/mcp",
            "api_key_secret": "K",
            "api_key_in": "query:api_key",
        }))
        .unwrap();
        let client = HttpMcpClient::new("c", &block, Some(&cred(key)));
        let encoded = percent_encode(key);
        assert_ne!(encoded, key);

        let raw_msg = client.scrub(format!("boom: {key}"));
        assert!(!raw_msg.contains(key), "{raw_msg}");
        let enc_msg = client.scrub(format!("https://h/mcp?api_key={encoded} failed"));
        assert!(!enc_msg.contains(&encoded), "{enc_msg}");
        assert!(enc_msg.contains("<redacted>"), "{enc_msg}");
    }

    /// THE round-2 finding: the upstream body used to be clamped to 512 chars
    /// and only scrubbed afterwards. When the boundary falls inside the key the
    /// surviving prefix is no longer a literal member of `secret_forms`, so the
    /// later `replace` matches nothing and a partial credential reaches the 503
    /// body, `PluginState.last_error`, and the wave transcript.
    ///
    /// **Scope, honestly stated.** This is a unit test of the two free
    /// functions and of the claim that their ORDER matters: the second half
    /// proves the reversed order demonstrably leaks, so the first half is not
    /// vacuous. It does NOT reach `request`, so on its own it cannot fail when
    /// the production lines are swapped. The call-site witness is the
    /// integration test `a_4xx_body_echoing_the_query_never_leaks_a_partial_key`
    /// in `tests/cases/connector_host.rs`, which drives a real `spawn` against
    /// a stub that echoes the query string inside an over-long 4xx body and
    /// asserts the surviving `last_error` carries no key prefix.
    #[test]
    fn key_straddling_the_truncation_boundary_is_still_redacted() {
        let client = client_with("query:api_key");
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
        // Exactly the prefix that survives the old (clamp-first) order: the
        // key starts at `MAX - len/2`, so `len/2` of its characters fit.
        let leaked_prefix: String = LEAKY.chars().take(LEAKY.len() / 2).collect();
        assert!(leaked_prefix.len() >= 8, "prefix must be a real leak");
        assert!(
            !good.contains(&leaked_prefix),
            "partial key survived: {good}"
        );

        // Mutation witness: the reversed order really does leak, so the
        // assertion above is testing something.
        let bad = scrub_with(
            &client.secret_forms,
            clamp_chars(body, MAX_UPSTREAM_DETAIL_CHARS),
        );
        assert!(
            bad.contains(&leaked_prefix),
            "the clamp-first order must demonstrably leak, else this test is vacuous: {bad}"
        );
    }

    /// `""` as a scrub pattern makes `String::replace` insert the marker at
    /// every character boundary — an amplifier, not a redaction. It is now
    /// unrepresentable rather than merely filtered: `new` takes an
    /// `HttpCredential`, and there is no empty one.
    #[test]
    fn no_key_registers_no_scrub_pattern_and_an_empty_one_cannot_be_built() {
        assert!(HttpCredential::parse("").is_err());
        let block: McpHttpBlock = serde_json::from_value(serde_json::json!({
            "url": "https://mcp.example.com/mcp",
        }))
        .unwrap();
        let client = HttpMcpClient::new("c", &block, None);
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

    // ---- Round-4 finding B: scrub the parsed tree, not the raw text -------
    //
    // These drive `parse_scrubbed`, which is the whole of what `request` does
    // with a 2xx body — not a re-implementation of it. Each one also runs the
    // OLD order (`scrub_with` on the raw text, then parse) and asserts it
    // demonstrably misbehaves, so none of them is vacuous.

    /// The corruption direction. A credential containing `":` is a literal
    /// match for JSON's key/value separator, so replacing it in the raw text
    /// mangles a document that is otherwise perfectly valid — and the connector
    /// reports a healthy upstream as "malformed JSON".
    ///
    /// `read_secrets` now refuses to let this value be authored, but that is
    /// the second line of defence, not this one: `HttpMcpClient` is handed a
    /// credential string and must be correct for whatever it gets.
    #[test]
    fn a_json_shaped_credential_does_not_corrupt_an_innocent_response() {
        let key = r#"":"#;
        let forms = vec![key.to_string()];
        let body =
            r#"{"jsonrpc":"2.0","id":1,"result":{"tools":[{"name":"t","description":"d"}]}}"#;

        let parsed = parse_scrubbed(&forms, body, "tools/list")
            .expect("a valid JSON document must survive a hostile credential");
        assert_eq!(
            parsed.pointer("/result/tools/0/description"),
            Some(&Value::String("d".into())),
            "the document must come through byte-identical: {parsed}"
        );

        // Mutation witness: the raw-text order really does destroy it.
        let mangled = scrub_with(&forms, body.to_string());
        assert!(
            serde_json::from_str::<Value>(&mangled).is_err(),
            "the raw-text order must demonstrably corrupt this body, else the \
             assertion above proves nothing: {mangled}"
        );
    }

    /// A credential containing a backslash collides with JSON escape pairs in
    /// responses that never echoed the credential at all.
    #[test]
    fn a_backslash_credential_does_not_corrupt_an_escaped_response() {
        let key = r"abc\defgh";
        let forms = vec![key.to_string()];
        // The upstream describes a Windows path: `C:\dir` — `C:\\dir` on the
        // wire. Nothing here is the credential.
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

    /// The LEAK direction, which is the serious one. A credential containing a
    /// newline appears JSON-escaped on the wire, so literal matching on the raw
    /// text never finds it — and the DECODED secret then reaches the
    /// `ExposedTool` description and the `tools/call` result.
    #[test]
    fn a_control_character_credential_echoed_by_the_upstream_is_still_redacted() {
        let key = "line1\nline2xx";
        let forms = vec![key.to_string()];
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

        // Mutation witness: raw-text scrubbing misses it entirely, because the
        // wire form is `line1\nline2xx` (an escape pair), not the literal.
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

    /// Keys are scrubbed too: an upstream is free to answer
    /// `{"<our-key>": …}`, and `inputSchema.properties` names reach the
    /// agent-visible tool catalog.
    #[test]
    fn object_keys_are_scrubbed_as_well_as_values() {
        let key = "sk-keyed-8213";
        let forms = vec![key.to_string()];
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

    /// Deeply nested strings are reached, and a no-key client is a no-op.
    #[test]
    fn nesting_is_traversed_and_no_key_means_no_walk() {
        let key = "sk-nested-8213";
        let forms = vec![key.to_string()];
        let body = format!(r#"{{"result":{{"a":[{{"b":[["{key}"]]}}]}}}}"#);
        let parsed = parse_scrubbed(&forms, &body, "x").unwrap();
        assert_eq!(
            parsed.pointer("/result/a/0/b/0/0"),
            Some(&Value::String("<redacted>".into()))
        );

        let untouched = parse_scrubbed(&[], &body, "x").unwrap();
        assert!(untouched.to_string().contains(key));
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
