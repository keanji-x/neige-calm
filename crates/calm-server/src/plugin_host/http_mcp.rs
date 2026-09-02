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
//! A credential that JSON would parse as a *number* is refused there too —
//! `scrub_value` deliberately does not touch JSON numbers (coercing them would
//! corrupt the payload), so an upstream echoing such a key back as a number in
//! `structuredContent` would put it in the tool result and the wave transcript.
//! That rule is the number **grammar** (`-1234567`, `1.234567`, `1E5`,
//! `1234e567` are all of them), delegated to serde_json rather than re-derived
//! from characters — see [`is_number_shaped`].
//! Constraining the credential removes *that* case: a number-shaped credential
//! is no longer representable, so the JSON-number hole is closed structurally.
//! The same move closes a second one: a credential that is a **fragment of the
//! redaction marker itself** (`redacted`, `edacted>#`, `<redacted>#2`) makes the
//! rekey pass's fail-closed self-check unsatisfiable — every name it could mint
//! still contains the credential — which is a runtime-hang, not a leak. That
//! shape is refused at the same layer; see [`overlaps_redaction_marker`].
//!
//! **Nothing above is a completeness claim, and this header must not be read as
//! one** (#1194 residual 2 — the residual is exactly that this list *reads*
//! exhaustive and is not). [`HttpMcpClient::new`] registers **at most two
//! literals**, both matched byte-exactly by [`str::replace`]: the raw
//! credential, and the uppercase-hex percent-encoding [`percent_encode`]
//! produces — which is the spelling this client itself puts on the wire. A
//! credential with no reserved characters encodes to itself, so the dedupe
//! leaves a single literal.
//!
//! **This is a net over observed shapes, not containment.** The shapes below
//! walk straight through it, and the list of them is not exhaustive either:
//!
//! * **percent-encoding in any other hex case** — `%2f` for `%2F`, and every
//!   mixed spelling such as `sk-a%2Fb%2bc%3Dd`. The case of each `%XX` triplet
//!   is chosen INDEPENDENTLY by whichever hop last re-encoded the string, so an
//!   n-triplet credential can arrive in 2ⁿ spellings and the one literal
//!   registered here is one of them;
//! * **any other encoding of the same bytes** — base64 (`c2stYWJj`), URL-safe
//!   base64, hex, HTML entities, double percent-encoding. The set of alphabets
//!   an upstream may choose is not ours to bound;
//! * **the credential split across two JSON strings** — `["sk-ab", "cd"]`, or a
//!   key holding one half and its value the other. No single string contains
//!   the literal, so no single `replace` matches, yet a reader of the rendered
//!   document reassembles it for free;
//! * **any non-literal re-derivation** — a hash the operator can reverse from a
//!   short key, a checksum, the credential embedded in an upstream-built URL
//!   with different escaping than [`percent_encode`] produces.
//!
//! One more gap, of a different kind, because it is manufactured HERE rather
//! than chosen by the upstream: [`str::replace`] does not rescan what it wrote,
//! so a credential that overlaps the marker's own text can be re-formed by its
//! own redaction. With the credential `redacted>y`, the upstream string
//! `redacted>yy` scrubs to `<redacted>y` — which contains the credential again.
//! [`HttpCredential::parse`] refuses credentials that are *fragments* of the
//! marker (they hang the rekey pass — see [`overlaps_redaction_marker`]), but it
//! accepts ones that merely overlap it, so this residual is open. Closing it
//! means either iterating the scrub to a fixpoint or refusing every credential
//! that shares an edge with the marker; both are decisions #1194 has not taken,
//! and neither is worth taking while the real answer is to move the key out of
//! the URL.
//!
//! **Why the gaps are not closed by adding literals or a smarter matcher.**
//! Enumeration does not converge (2ⁿ hex spellings, an unbounded set of
//! alphabets). A matcher does converge for the hex case specifically — and one
//! was written and then REVERTED, on measurement: comparing every `%XX` triplet
//! case-insensitively means a hand-rolled scan instead of `str::replace`'s
//! two-way search, and its cost is paid on the CLEAN path, per `tools/call`,
//! whether or not the body contains anything resembling the credential. Measured
//! on this box with a 257-byte credential and a 4 MiB response body:
//! `client.scrub` took **991 ms** with the per-triplet matcher against **9.8 ms**
//! with `str::replace` — a scan that grows with (body × credential length)
//! against one that is linear in the body. Buying one of 2ⁿ spellings for two
//! orders of magnitude on every healthy call is not a trade this module makes.
//!
//! What actually closes the class is not on this axis at all: it is not putting
//! the credential where an upstream can echo it (`api_key_in: header`, the main
//! proposal on #1194). With the key in a header there is no percent-encoded
//! form to miss, and most of this machinery deletes itself.
//!
//! The raw-text scrub survives in exactly one place: the non-2xx arm, where the
//! body is an arbitrary error page with nothing to parse. There the
//! scrub-before-truncate rule applies and is not pedantry — an upstream that
//! echoes the request URL back inside a long error body gets truncated to 512
//! chars, and if that boundary falls inside the key the surviving prefix is no
//! longer a literal member of `secret_forms`, so a later `replace` misses it
//! entirely and a partial credential ships.

use std::collections::HashMap;
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
    /// * **not something JSON would parse as a number** — [`scrub_value`] does
    ///   not descend into JSON numbers, so a number-shaped credential echoed
    ///   back as `{"key": 12345678}` in `structuredContent` reaches the tool
    ///   result and the wave transcript unredacted. Making numbers scrubbable is
    ///   not an option (it would rewrite unrelated values and change the
    ///   payload's type); making the credential un-number-like is;
    /// * **at least [`MIN_CREDENTIAL_LEN`] characters** — see that constant;
    /// * **not a fragment of the redaction marker** — see
    ///   [`overlaps_redaction_marker`]. Such a credential makes the rekey pass
    ///   in [`scrub_value`] spin forever on a tokio worker thread.
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
        if is_number_shaped(raw) {
            return Err(
                "parses as a JSON number: an upstream is free to echo such a value back \
                 as a JSON *number*, which carries no string to redact, so it would reach \
                 tool results and wave transcripts in the clear. Add a non-numeric \
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
            // The marker itself is deliberately NOT spelled in this message:
            // every credential this rule refuses is a substring of it, so
            // quoting it would quote the credential — the one thing an
            // operator-facing string on this path may never do.
            return Err("is a fragment of the marker string this module rewrites \
                 credentials to (or of the `#<n>`-suffixed key names derived \
                 from it): every name the redactor could give a rewritten \
                 object key would still contain this credential, so the \
                 fail-closed name search in `scrub_value` could never accept \
                 one and a single upstream response would spin a runtime worker \
                 instead of returning. Choose a credential that is not a piece \
                 of that marker."
                .to_string());
        }
        Ok(Self(raw.to_string()))
    }

    fn as_str(&self) -> &str {
        &self.0
    }
}

/// Is `raw` a value an upstream could echo back as a bare JSON **number**?
///
/// The rule this answers is a *grammar*, not a lexical shape, and the first
/// version of it got that wrong: it asked "are all the characters digits?",
/// which leaves `-1234567`, `1.234567`, `-1.2e-7` and `1E5` accepted — every one
/// of them a JSON number that [`scrub_value`] (which deliberately never descends
/// into `Value::Number`) would hand back in the clear. So do not re-derive the
/// grammar here; ask serde_json, the same parser the response path uses.
///
/// The obvious spelling — `matches!(from_str::<Value>(raw), Ok(Value::Number(_)))`
/// — is *not* the grammar, and the gap is not hypothetical: `1234e567` is a
/// perfectly legal JSON number token that `Value` refuses with `number out of
/// range`, because building a `Value` also performs the f64 conversion. Such a
/// credential would sail through and could still be echoed back numerically.
/// `IgnoredAny` runs serde_json's *scanner* with no conversion behind it, so it
/// answers the syntactic question the `Value` constructor cannot.
///
/// `IgnoredAny` also accepts the other bare literals (`true`, `null`, `[1,2]`).
/// Given the charset already enforced above — printable ASCII, no space, no
/// quote, no backslash — "is a standalone JSON document" is a superset of "is a
/// JSON number" whose extra members are not credentials either, so refusing them
/// costs nothing and keeps this a single delegated call.
///
/// The explicit digits-only arm covers what serde_json's grammar *rejects*:
/// `00000000000000000000` is not a legal JSON number (leading zeros), so the
/// scanner does not flag it. It is retained exactly as it shipped — a
/// digits-only value is number-shaped to every human and every non-strict
/// producer, and no real credential lives in that class.
fn is_number_shaped(raw: &str) -> bool {
    raw.chars().all(|c| c.is_ascii_digit())
        || serde_json::from_str::<serde::de::IgnoredAny>(raw).is_ok()
}

/// Is `raw` a **substring of some name the redactor can mint**?
///
/// The names [`scrub_value`]'s rekey pass can produce are exactly the marker
/// family
///
/// > `M = {REDACTED} ∪ {REDACTED + "#" + <nonempty decimal digits>}`
///
/// and this predicate answers `∃m ∈ M. raw ⊆ m`. Such a credential is a hang,
/// not a leak, and it is upstream-triggerable with a single response: with the
/// credential `redacted`, an upstream answering `{"xredactedy": 1}` scrubs that
/// key to `x<redacted>y`, whose every candidate name (`x<redacted>y`,
/// `…#2`, `…#3`, …) contains the credential, so the fail-closed self-check
/// `scrub_with(candidate) == candidate` is unsatisfiable and the loop spins. It
/// spins on a tokio WORKER (`parse_scrubbed` runs after the `spawn_blocking`
/// join, not inside it) at 100% of one core and cannot be cancelled — measured
/// at 9 min 17 s without exiting before the reviewer killed it.
///
/// **The decision procedure**, derived rather than enumerated. Write
/// `P = REDACTED + "#"`. `REDACTED` is a prefix of `P`, so substrings of
/// `REDACTED` are already substrings of `P` and the first half of `M` needs no
/// case of its own. `P` contains no digit, so for any `m = P·D` a substring `x`
/// of `m` is exactly one of:
///
/// 1. wholly inside `P` — i.e. `P.contains(x)`;
/// 2. wholly inside `D` — i.e. `x` is all digits, and since `D` ranges over
///    every nonempty digit string, any such `x` qualifies;
/// 3. straddling the join — `x = s·t` with `s` a nonempty suffix of `P` and `t`
///    a nonempty prefix of `D`. Because `P` holds no digit and `D` holds
///    nothing else, that split is forced to fall exactly at `x`'s maximal
///    trailing digit run.
///
/// So: strip the maximal trailing digit run; no digits ⇒ case 1; no head ⇒
/// case 2; otherwise ⇒ case 3, which is `P.ends_with(head)`. Case 2 is already
/// unreachable behind [`is_number_shaped`] (an all-digit credential is refused
/// as number-shaped first) and is kept only so this function answers the
/// question its name asks, independently of its caller's ordering.
///
/// **What it deliberately does NOT refuse**, and the residual that leaves:
/// a credential that merely *contains* the marker (`sk-<redacted>-x`) or
/// *overlaps its edge* (`redacted>y`) is accepted, because it is not a fragment
/// of any name the redactor mints, so the rekey candidates `…#2`, `…#3` do
/// eventually clear the self-check. Those credentials still have a sharper
/// edge of their own: [`str::replace`] does not rescan what it wrote, so
/// scrubbing `redacted>yy` for the credential `redacted>y` yields
/// `<redacted>y`, which contains the credential again. That is a residual of
/// the ONE-PASS scrubber (recorded with the other residuals in the module
/// header), not of this rule, and it is why nothing downstream may assume
/// `scrub_with` is idempotent — see the probe budget in [`scrub_value`], which
/// is what actually bounds the rekey loop.
fn overlaps_redaction_marker(raw: &str) -> bool {
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
    /// The literals the secret is known to take on the wire: the raw
    /// credential, and the uppercase-hex percent-encoding this client puts in
    /// its own query string. **Not an exhaustive list of the shapes an upstream
    /// can echo** — see the module header for the ones that walk through.
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
            // AT MOST TWO literals — the raw credential and the spelling this
            // client itself puts in the query string — and exactly one when the
            // credential has no reserved character, because then
            // `percent_encode` is the identity and the dedupe below collapses
            // them. **This list is NOT exhaustive**, deliberately and with the
            // gaps named: see the module header, including why the
            // case-insensitive percent matcher that briefly lived here was
            // reverted on measurement (991 ms vs 9.8 ms on a clean 4 MiB body).
            //
            // Longest first, so scrubbing an encoded form is not pre-empted by a
            // shorter raw substring match (the encoded form is never shorter
            // than the raw one — `percent_encode` either copies a byte or
            // expands it to three).
            //
            // The dedupe + `is_empty` filter are the braces to
            // `HttpCredential`'s belt. Empty: an empty pattern turns `scrub`
            // into a memory amplifier (see `scrub_with`), and this is the last
            // place that could register one — `percent_encode("")` is `""` too,
            // so the filter has to cover the encoded form as well. Duplicate: a
            // repeated pattern is wasted work whose second pass would scan the
            // already-substituted text.
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
    // Fail-closed on the one shape the rekey pass cannot name: hand back an
    // error instead of a document, never an unredacted or a lossy one. The
    // message names no key and quotes no upstream text.
    scrub_value(forms, &mut parsed).map_err(|RekeyBudgetExhausted| {
        RpcError::internal(format!(
            "mcp-http {method}: response could not be redacted — an object's keys \
             exhausted the rekey probe budget"
        ))
    })?;
    Ok(parsed)
}

/// Recursively replace every literal in `forms` inside a JSON tree — string
/// values **and object keys**.
///
/// Keys matter as much as values: an upstream is free to answer
/// `{"<our-api-key>": "…"}`, and a `tools/list` entry's `inputSchema`
/// properties are object keys that reach `ExposedTool` and, from there, the
/// agent-visible tool catalog.
///
/// Rekeying is entry-count preserving: keys that collide after redaction are
/// disambiguated rather than merged. See the comment on the rekey branch for
/// why that is not cosmetic.
///
/// Returns [`RekeyBudgetExhausted`] instead of looping forever when no name can
/// be found for a rewritten key — see the budget on the rekey branch.
fn scrub_value(forms: &[String], v: &mut Value) -> Result<(), RekeyBudgetExhausted> {
    if forms.is_empty() {
        return Ok(());
    }
    match v {
        // `scrub_with` already skips the allocation when nothing matches, so
        // this arm costs one substring search per form per string on the clean
        // path — the same shape as the raw-text scrub it replaced, minus the
        // 4 MiB `String::replace` copies.
        Value::String(s) => *s = scrub_with(forms, std::mem::take(s)),
        Value::Array(items) => {
            for item in items {
                scrub_value(forms, item)?;
            }
        }
        Value::Object(map) => {
            // `contains` is not a second matcher beside `scrub_with`'s: it is
            // the exact predicate `str::replace` decides on (and the one
            // `scrub_with` itself uses to skip the allocation), so a key cannot
            // be judged "needs rekeying" by one rule and rewritten by another.
            let rekey = map
                .keys()
                .any(|k| forms.iter().any(|f| k.contains(f.as_str())));
            if rekey {
                // #1194 residual 1 — this used to `collect()` straight back
                // into a `Map`, which SILENTLY DROPS entries: two distinct keys
                // that both scrub to `<redacted>` collapse to one, and the
                // surviving value is whichever came last. That is data loss on
                // a path whose entire job is to hand the caller a faithful
                // document minus the credential — and it is reachable without
                // anything exotic, because an upstream that echoes our query
                // string may well report the raw and the percent-encoded form
                // as sibling keys, both of which are scrub patterns
                // (`HttpMcpClient::new` registers the raw and the
                // percent-encoded spelling).
                //
                // A collision therefore gets a disambiguating suffix
                // (`<redacted>`, `<redacted>#2`, …) instead of disappearing.
                // The reader still learns nothing about the credential — every
                // colliding key rendered to the same marker by construction —
                // but the entry count, and every value hanging off it, survive.
                //
                // THREE properties the naive spelling of that got wrong:
                //
                // 1. **Untouched keys keep their names** (#1194 B4). A key the
                //    scrubber did not change is upstream data — a real schema
                //    property, possibly literally `<redacted>` — and renaming it
                //    corrupts a document that never carried the credential. The
                //    old single pass processed keys in `BTreeMap` order, so a
                //    credential key sorting before a genuine `<redacted>` key
                //    took that name and pushed the innocent one to
                //    `<redacted>#2`. Pass 1 below reserves every unchanged key
                //    first; pass 2 only ever picks a name that is still free.
                // 2. **A generated name can never spell the credential**
                //    (#1194 B1). `<redacted>#2` is itself a LEGAL credential
                //    (printable ASCII, 12 chars, not number-shaped), so an
                //    operator with that key would have this code MINT the secret
                //    — after redaction, straight into `ExposedTool` and the wave
                //    transcript. The candidate is therefore re-scrubbed and
                //    rejected unless it survives unchanged: fail-closed on the
                //    scrubber's own verdict, not on a "does it look like a
                //    marker" heuristic.
                // 3. **Assignment is linear, not quadratic** (#1194 B3).
                //    Restarting the suffix search at 2 for every key makes a
                //    document whose keys all redact to one base cost O(n²)
                //    probes — measured at 59 s for a 2.4 MiB body of 19 683
                //    such keys, from a stateless upstream we do not trust. One
                //    monotone counter PER BASE makes the total probe count
                //    linear in the entry count.
                //
                // Assignment is deterministic and independent of how the
                // document was built: `serde_json::Map` is a `BTreeMap` here
                // (the `preserve_order` feature is off), so `into_iter` yields
                // keys in sorted order and the same key set always produces the
                // same suffixes.
                let taken = std::mem::take(map);
                let taken_len = taken.len();
                let mut rebuilt = serde_json::Map::with_capacity(taken_len);
                let mut redacted: Vec<(String, Value)> = Vec::new();
                // Pass 1 — reserve the names that are upstream's own.
                for (k, v) in taken {
                    let scrubbed = scrub_with(forms, k.clone());
                    if scrubbed == k {
                        rebuilt.insert(k, v);
                    } else {
                        redacted.push((scrubbed, v));
                    }
                }
                // Pass 2 — name the redacted ones around what pass 1 reserved.
                //
                // `next[base]` is how many names this base has already been
                // given, so probing never revisits a taken suffix. `u64` cannot
                // overflow here by a wide margin: the counter is bounded by the
                // entry count plus the handful of skips rules (1) and (2) can
                // force, and the entry count is bounded by `MAX_BODY_BYTES`
                // (4 MiB) over the two bytes a `{}` -delimited entry needs.
                //
                // **TERMINATION** is by budget, not by faith (#1194 round-3).
                // The self-check in rule (2) is a fail-closed test on a
                // scrubber that is NOT idempotent — `str::replace` does not
                // rescan what it wrote, so for a credential that overlaps the
                // marker's text (`redacted>y`) every candidate a base can
                // produce may fail the check forever. `redacted>yy` as an
                // upstream key is enough. Each iteration of the loop below
                // either breaks or consumes one probe from a finite budget, so
                // it runs at most `probes` times; exhausting the budget aborts
                // the whole response rather than spinning a tokio worker (the
                // shape a reviewer measured at 9 min 17 s, 99.6% CPU, on the
                // strictly worse credentials that `overlaps_redaction_marker`
                // now refuses outright).
                //
                // The budget cannot fire on a healthy document, and that is
                // arithmetic rather than a hope: for one map, a probe is
                // skipped only because the candidate name is already in
                // `rebuilt` or because it failed the self-check. Skips of the
                // first kind are at most `taken.len()` in total, because the
                // per-base counter is monotone, so each skip burns a DISTINCT
                // candidate name, and every name in `rebuilt` can block at most
                // one of them. Each of the `redacted.len()` keys additionally
                // spends the one probe it succeeds on. So absent self-check
                // failures the total is at most `2 × taken.len()`, and the
                // budget is that plus a margin.
                let mut probes: u64 = 2 * (taken_len as u64) + 2;
                let mut next: HashMap<String, u64> = HashMap::new();
                for (base, v) in redacted {
                    let counter = next.entry(base.clone()).or_insert(0);
                    let name = loop {
                        if probes == 0 {
                            return Err(RekeyBudgetExhausted);
                        }
                        probes -= 1;
                        let candidate = if *counter == 0 {
                            base.clone()
                        } else {
                            format!("{base}#{}", *counter + 1)
                        };
                        *counter += 1;
                        if !rebuilt.contains_key(&candidate)
                            && scrub_with(forms, candidate.clone()) == candidate
                        {
                            break candidate;
                        }
                    };
                    rebuilt.insert(name, v);
                }
                *map = rebuilt;
            }
            for (_, child) in map.iter_mut() {
                scrub_value(forms, child)?;
            }
        }
        // Numbers, booleans and null carry no string to redact. A number-shaped
        // credential — the one value an upstream could echo back HERE, as a
        // JSON number — cannot exist: `HttpCredential::parse` refuses anything
        // serde_json's scanner accepts as a bare literal (`is_number_shaped`).
        _ => {}
    }
    Ok(())
}

/// The rekey pass ran out of probes for one object's keys — see the budget in
/// [`scrub_value`]. Carries nothing: there is no detail here that is safe to
/// print, and the caller turns it into a fixed operator-facing sentence.
#[derive(Debug, PartialEq, Eq)]
struct RekeyBudgetExhausted;

/// Trips its flag when dropped. See the comment at its construction site in
/// [`HttpMcpClient::request`].
struct CancelOnDrop(Arc<AtomicBool>);

impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        self.0.store(true, Ordering::SeqCst);
    }
}

/// The marker every recognised form of the credential is rewritten to.
const REDACTED: &str = "<redacted>";

/// Replace every occurrence of every registered literal with [`REDACTED`].
///
/// Free function (not a method) so the blocking closure in
/// [`HttpMcpClient::request`] can scrub before truncating without capturing
/// `&self`.
///
/// **Empty patterns are impossible by construction** — [`HttpMcpClient::new`]
/// refuses to register one, and [`HttpCredential::parse`] refuses an empty
/// credential upstream of that. Both halves matter: `"".replace("", "<redacted>")`
/// inserts the marker at every character boundary, so a 4 MiB upstream error body
/// would expand ~11x and then be cloned into the status entry, the broadcast
/// event, and the HTTP error body. The `debug_assert` keeps the invariant honest
/// if a future caller builds `secret_forms` some other way.
///
/// **This is one pass per pattern, and it is NOT idempotent.** `str::replace`
/// scans left to right and never rescans what it wrote, so a substitution CAN
/// manufacture a match the same pass has already walked past: with the
/// credential `redacted>y`, `"hit redacted>yy end"` comes back as
/// `"hit <redacted>y end"` — the credential, re-formed out of the marker's own
/// text. An earlier revision of this comment claimed the opposite ("substitution
/// does not manufacture a new match for a later form"); it was disproved by
/// review with `"hit redacted%3E end"` for the credential `redacted>`, which
/// yields `"hit <<redacted> end"`.
///
/// [`HttpCredential::parse`] closes the sharpest corner of that class — a
/// credential that is a FRAGMENT of the marker, which additionally made the
/// rekey pass unable to name a key at all — but the overlapping shapes above
/// stay open (see [`overlaps_redaction_marker`] and the module header). So no
/// caller may assume `scrub_with(scrub_with(x)) == scrub_with(x)`: the one place
/// that needs a fixpoint asks for it explicitly and is budgeted for the answer
/// never arriving ([`scrub_value`]'s pass 2).
fn scrub_with(forms: &[String], s: String) -> String {
    let mut out = s;
    for form in forms {
        debug_assert!(!form.is_empty(), "an empty scrub pattern is a memory bomb");
        if form.is_empty() {
            continue;
        }
        // `contains` first: `replace` would allocate a fresh `String` even when
        // nothing matches, and the clean path is every healthy response body.
        if out.contains(form.as_str()) {
            out = out.replace(form.as_str(), REDACTED);
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

/// Minimal percent-encoding for query-string values, uppercase hex — the form
/// that goes on the wire. We only need the characters that would break a query
/// parameter; `url` is not a dependency of this crate and pulling one for eight
/// bytes of logic is not worth it.
///
/// **Uppercase hex is a wire fact, not a matching rule.** This is the spelling
/// the client sends, so it is the one an upstream echoing our query string
/// verbatim will quote back — and it is the only encoded spelling registered as
/// a scrub literal. Other hex cases (`%2f`, and mixed per-triplet spellings a
/// re-encoding hop may produce) are NOT covered; two attempts to cover them —
/// a second lowercase literal, then a per-triplet case-insensitive matcher —
/// were both reverted, the first as non-convergent enumeration and the second
/// on measured clean-path cost. The module header carries the numbers and the
/// full gap list.
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

    /// Scrub patterns for the tests that drive `parse_scrubbed` / `scrub_value`
    /// with a hand-picked credential rather than through `HttpMcpClient::new`.
    fn exact_forms(keys: &[&str]) -> Vec<String> {
        keys.iter().map(|k| (*k).to_string()).collect()
    }

    /// `scrub_value` on a tree that is known not to exercise the rekey budget,
    /// so the vast majority of cases below read as they did before that branch
    /// could fail.
    fn scrub_tree(forms: &[String], v: &mut Value) {
        scrub_value(forms, v).expect("fixture must not exhaust the rekey budget");
    }

    /// Shorthand for a credential the HTTP path will accept.
    fn cred(raw: &str) -> HttpCredential {
        HttpCredential::parse(raw)
            .unwrap_or_else(|e| panic!("{raw:?} must be a valid credential: {e}"))
    }

    /// The invariant `HttpMcpClient::new`'s parameter type carries: every shape
    /// the redaction layer cannot handle is refused before a client exists.
    ///
    /// Each case is a concrete failure of the scrubber, not a style rule — see
    /// [`HttpCredential::parse`]. The number-shaped cases are round-5 finding 3:
    /// the tree scrub does not descend into JSON numbers, so an upstream echoing
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
            // The number rule is a grammar, not "all digits"; the table in
            // `a_number_shaped_credential_is_refused_whatever_its_spelling`
            // is the witness for the rest of that grammar.
            ("-1234567", "negative integer"),
            ("1.234567", "decimal fraction"),
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

    /// The number rule is the JSON number **grammar**, not the single lexical
    /// shape "every character is a digit" it was first written as.
    ///
    /// Every value below is a valid JSON number that the all-digit check
    /// accepted; `scrub_value` never descends into `Value::Number`, so any of
    /// them echoed back numerically by an upstream reaches the tool result and
    /// the wave transcript in the clear — the exact leak the rule exists to
    /// close. Restore `raw.chars().all(char::is_ascii_digit)` as the whole rule
    /// and every case here except the two all-digit ones fails.
    ///
    /// Each case asserts the *number* refusal specifically, not merely "some
    /// error": the length floor would otherwise satisfy the short spellings
    /// (`1E5`, `-1.2e-7`) without the number rule existing at all.
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
            // Collected rather than asserted case by case, so a regression
            // reports the whole surface it reopened instead of the first
            // spelling in the table.
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

    /// Stated positively, so the tests above cannot pass by refusing
    /// everything — including the boundary of each numeric rule.
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
        let forms = exact_forms(&[key]);
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

    /// #1194 residual 1 — two DIFFERENT keys that redact to the same marker
    /// must not collapse into one entry.
    ///
    /// Drives `scrub_value` directly rather than restating what it does: build
    /// an object whose keys are the raw and the percent-encoded form of the
    /// same credential (exactly what an upstream echoing our query string back
    /// alongside its decoded parse produces), then assert on the entry COUNT
    /// and on both values being reachable. The old `collect()` kept whichever
    /// came last and dropped the other — a green "the key is redacted"
    /// assertion would have passed over it, which is why the count is the
    /// load-bearing line here.
    #[test]
    fn two_keys_redacting_to_the_same_marker_do_not_collapse() {
        let key = "sk-a/b+c-8213";
        let encoded = percent_encode(key);
        assert_ne!(encoded, key, "fixture needs two distinct literals");
        let forms = vec![encoded.clone(), key.to_string()];

        let mut obj = serde_json::Map::new();
        obj.insert(key.to_string(), serde_json::json!(1));
        obj.insert(encoded.clone(), serde_json::json!(2));
        obj.insert("safe".to_string(), serde_json::json!(3));
        let mut v = Value::Object(obj);
        assert_eq!(v.as_object().unwrap().len(), 3, "fixture must start with 3");

        scrub_tree(&forms, &mut v);
        let obj = v.as_object().unwrap();

        assert_eq!(obj.len(), 3, "an entry was silently dropped: {v}");
        assert_eq!(obj.get("safe"), Some(&serde_json::json!(3)));
        // Neither credential form survives in any key…
        for k in obj.keys() {
            assert!(!k.contains(key), "raw key survived: {v}");
            assert!(!k.contains(&encoded), "encoded key survived: {v}");
        }
        // …and both values are still reachable, under distinguishable markers.
        let mut values: Vec<u64> = obj.values().filter_map(Value::as_u64).collect();
        values.sort_unstable();
        assert_eq!(values, vec![1, 2, 3], "a value was lost: {v}");
        // ⚠️ DO NOT relax the next two lines into a set assertion. They are not
        // only about this document: WHICH key gets the bare marker and which
        // gets `#2` is decided by `BTreeMap` iteration order, and `%` (0x25)
        // sorts before `/` (0x2F), so the ENCODED key is processed first. That
        // holds only while `serde_json`'s `preserve_order` feature is off — the
        // assumption the rekey branch's determinism comment states and that
        // nothing else in this crate checks. Turn the feature on (directly or
        // through a dependency unifying features) and insertion order takes
        // over, these two lines swap, and this test is the sentinel that says
        // so. A set assertion would pass silently and delete that signal.
        assert_eq!(obj.get("<redacted>"), Some(&serde_json::json!(2)));
        assert_eq!(obj.get("<redacted>#2"), Some(&serde_json::json!(1)));
    }

    /// The suffix must step over a marker the upstream itself already used,
    /// otherwise the collision handling reintroduces the very drop it exists to
    /// prevent — one level along.
    #[test]
    fn a_preexisting_marker_key_does_not_get_overwritten() {
        let key = "sk-collide-8213";
        let forms = exact_forms(&[key]);
        let mut obj = serde_json::Map::new();
        obj.insert(key.to_string(), serde_json::json!(1));
        obj.insert("<redacted>".to_string(), serde_json::json!(2));
        obj.insert("<redacted>#2".to_string(), serde_json::json!(3));
        let mut v = Value::Object(obj);
        scrub_tree(&forms, &mut v);
        let obj = v.as_object().unwrap();
        assert_eq!(obj.len(), 3, "an entry was dropped: {v}");
        let mut values: Vec<u64> = obj.values().filter_map(Value::as_u64).collect();
        values.sort_unstable();
        assert_eq!(values, vec![1, 2, 3], "a value was lost: {v}");
    }

    /// #1194 B4 — a key the scrubber did NOT touch is upstream data, and its
    /// name is not ours to change.
    ///
    /// The old single pass walked the map in `BTreeMap` order and let whoever
    /// came first take `<redacted>`. When the credential key sorted before a
    /// genuine `<redacted>` property, the credential took the name and the
    /// innocent schema property was renamed to `<redacted>#2` — a rewrite of a
    /// document that has nothing to do with the credential, landing in
    /// `inputSchema.properties` and therefore in the agent-visible catalog.
    ///
    /// Both sort orders are covered because the defect is order-dependent:
    /// with the credential sorting AFTER, the old code happened to be right,
    /// so a single-order test would have passed over it.
    #[test]
    fn an_untouched_upstream_key_keeps_its_own_name_in_either_sort_order() {
        // `-` (0x2D) sorts before `<` (0x3C); `s` (0x73) sorts after it.
        for (key, where_) in [
            (
                "-abc-defg",
                "credential key sorts BEFORE the innocent marker key",
            ),
            ("sk-abc-defg", "credential key sorts AFTER it"),
        ] {
            let forms = exact_forms(&[key]);
            assert!(
                HttpCredential::parse(key).is_ok(),
                "{where_}: fixture must be a real credential"
            );
            let mut obj = serde_json::Map::new();
            obj.insert(
                key.to_string(),
                serde_json::json!("from the credential key"),
            );
            obj.insert(
                "<redacted>".to_string(),
                serde_json::json!("upstream's own"),
            );
            obj.insert("safe".to_string(), serde_json::json!(3));
            let mut v = Value::Object(obj);
            scrub_tree(&forms, &mut v);
            let obj = v.as_object().unwrap();

            assert_eq!(obj.len(), 3, "{where_}: an entry was dropped: {v}");
            // THE assertion: the innocent key kept BOTH its name and its value.
            assert_eq!(
                obj.get("<redacted>"),
                Some(&serde_json::json!("upstream's own")),
                "{where_}: a key that never held the credential was renamed \
                 and/or had its value swapped: {v}"
            );
            assert_eq!(
                obj.get("safe"),
                Some(&serde_json::json!(3)),
                "{where_}: {v}"
            );
            // …and the redacted one went somewhere else, without vanishing.
            assert_eq!(
                obj.get("<redacted>#2"),
                Some(&serde_json::json!("from the credential key")),
                "{where_}: {v}"
            );
        }
    }

    /// #1194 B1 — the disambiguating suffix must not be able to RE-MINT the
    /// credential.
    ///
    /// `<redacted>#2` passes every rule in [`HttpCredential::parse`]: printable
    /// ASCII, no quote/backslash/space, twelve characters, not number-shaped. An
    /// operator may legitimately hold it. The suffix is appended AFTER the key
    /// has been scrubbed, so a naive counter hands `<redacted>#2` — the
    /// credential, verbatim — to `ExposedTool` and the wave transcript, on the
    /// success path, with the redaction machinery reporting success.
    ///
    /// The fix is fail-closed on the scrubber's own verdict: a candidate name is
    /// accepted only if re-scrubbing leaves it unchanged.
    ///
    /// **Round 3 moved the belt and left this as the braces.**
    /// `HttpCredential::parse` now REFUSES `<redacted>#2` outright
    /// ([`overlaps_redaction_marker`]) — that shape does not merely re-mint the
    /// credential, it makes the name search unsatisfiable and hangs a runtime
    /// worker. So this case can no longer be built through
    /// `HttpMcpClient::new`, and the first assertion below pins that. The
    /// self-check itself stays and is still exercised here on a hand-built form
    /// list, because it is what keeps a minted name honest for *any* pattern set
    /// a future caller assembles, not only the two `new` registers.
    #[test]
    fn a_disambiguating_suffix_can_never_spell_a_registered_pattern() {
        // The belt: this credential is refused before a client exists.
        assert!(
            HttpCredential::parse("<redacted>#2").is_err(),
            "a marker fragment must not be registrable as a credential"
        );

        // The braces, on a form list production can no longer produce: two
        // distinct keys collapse to the same base, and the obvious second name
        // for it is itself a registered pattern.
        let forms = exact_forms(&["k1secret", "k2secret", "<redacted>#2"]);
        let mut obj = serde_json::Map::new();
        obj.insert("k1secret".to_string(), serde_json::json!(1));
        obj.insert("k2secret".to_string(), serde_json::json!(2));
        let mut v = Value::Object(obj);
        scrub_tree(&forms, &mut v);

        let obj = v.as_object().unwrap();
        assert_eq!(obj.len(), 2, "an entry was dropped: {v}");
        let mut values: Vec<u64> = obj.values().filter_map(Value::as_u64).collect();
        values.sort_unstable();
        assert_eq!(values, vec![1, 2], "a value was lost: {v}");
        // Stated positively so the test cannot pass by refusing to name
        // anything: it stepped OVER `#2` and landed on `#3`.
        assert!(obj.contains_key("<redacted>"), "{v}");
        assert!(
            obj.contains_key("<redacted>#3"),
            "the suffix search handed out a name that is itself a scrub \
             pattern: {v}"
        );
    }

    /// #1194 channel-A A1 — an object whose KEY was rekeyed must still have its
    /// children walked.
    ///
    /// Moving the `for (_, child) in map.iter_mut()` recursion inside the `else`
    /// of the rekey branch survives the whole 139-case suite, and it is a LEAK:
    /// `{"<credential>": "see key <credential> in the docs"}` comes back with
    /// the key redacted and the string value verbatim — into the `ExposedTool`
    /// description an agent reads and into the wave transcript.
    ///
    /// Three shapes, because "the children" is not one thing: a scalar string
    /// beside the rekeyed entry, a nested object one level down, and an object
    /// inside an array. All three hang off the SAME rekeyed map, so all three
    /// are lost by that one mutation.
    #[test]
    fn a_rekeyed_object_still_scrubs_its_children() {
        let key = "sk-both-8213";
        let forms = exact_forms(&[key]);
        let body = serde_json::json!({
            "result": {
                // The rekey trigger and a sibling string value in one object.
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
        // Each shape named individually, so a partial regression is legible.
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

    /// #1194 residual 2, in the shape the residual actually asked for: pin what
    /// IS registered, so the module header's honesty about what is not can be
    /// read against something.
    ///
    /// Exactly two literals — the raw credential and the uppercase-hex
    /// percent-encoding this client sends — longest first. There is deliberately
    /// no assertion here about `%2f` or mixed-case triplets: they are NOT
    /// covered, that gap is documented in the module header rather than
    /// enshrined in a test that asserts a leak exists.
    #[test]
    fn registration_is_the_raw_form_and_the_uppercase_encoding_longest_first() {
        // Reserved characters in three classes — `/`, `+`, `=` — so the encoded
        // form is visibly a different string.
        let key = "sk-a/b+c=d";
        let block: McpHttpBlock = serde_json::from_value(serde_json::json!({
            "url": "https://mcp.example.com/mcp",
            "api_key_secret": "K",
            "api_key_in": "query:api_key",
        }))
        .unwrap();
        let client = HttpMcpClient::new("c", &block, Some(&cred(key)));

        let upper = percent_encode(key);
        assert_eq!(upper, "sk-a%2Fb%2Bc%3Dd", "encoder shape changed");
        assert_eq!(
            client.secret_forms,
            vec![upper.clone(), key.to_string()],
            "two literals, longest first"
        );

        // Both spellings really are scrubbed…
        for spelling in [key, upper.as_str()] {
            let msg = client.scrub(format!("upstream echoed ?api_key={spelling} back"));
            assert!(!msg.contains(spelling), "{spelling}: {msg}");
            assert!(msg.contains("<redacted>"), "{spelling}: {msg}");
        }

        // …and matching is byte-exact: a value differing only in ASCII case
        // from the credential is a DIFFERENT credential and must not be
        // rewritten.
        let other = "SK-A/B+C=D";
        assert_eq!(
            client.scrub(format!("boom {other}")),
            format!("boom {other}"),
            "the raw form must not fold case"
        );
    }

    /// #1194 channel-A N4 — the "encoded but hex-letter-free" input class.
    ///
    /// A credential whose reserved characters encode to letter-free triplets
    /// (`!` is `%21`) has an encoded form that is distinct from the raw one yet
    /// identical in every hex case. It is registered and matched like any other
    /// encoded form; the case is kept because that middle state is a real input
    /// class and two successive rounds of this residual tripped over it.
    #[test]
    fn an_encoded_form_with_no_hex_letter_still_matches() {
        let key = "sk-abc!d!";
        let block: McpHttpBlock = serde_json::from_value(serde_json::json!({
            "url": "https://mcp.example.com/mcp",
            "api_key_secret": "K",
            "api_key_in": "query:api_key",
        }))
        .unwrap();
        let client = HttpMcpClient::new("c", &block, Some(&cred(key)));

        let encoded = percent_encode(key);
        assert_eq!(
            encoded, "sk-abc%21d%21",
            "fixture needs letter-free triplets"
        );
        // The point of the fixture: every hex digit here is a DIGIT, so this
        // spelling is the same in either hex case — the one encoded form that
        // needs no case rule to be matched.
        assert!(
            encoded
                .split('%')
                .skip(1)
                .all(|t| t.as_bytes()[..2].iter().all(u8::is_ascii_digit)),
            "fixture must have letter-free triplets: {encoded}"
        );
        assert_ne!(encoded, key, "…but still distinct from the raw form");
        assert_eq!(client.secret_forms.len(), 2, "{:?}", client.secret_forms);

        let msg = client.scrub(format!("?api_key={encoded}"));
        assert!(!msg.contains(&encoded), "{msg}");
        assert!(msg.contains("<redacted>"), "{msg}");
    }

    /// A credential with no reserved characters encodes to itself: the dedupe
    /// must leave exactly ONE pattern, not two copies whose second pass would
    /// re-scan the already-substituted text.
    #[test]
    fn an_unreserved_credential_registers_exactly_one_form() {
        let block: McpHttpBlock = serde_json::from_value(serde_json::json!({
            "url": "https://mcp.example.com/mcp",
            "api_key_secret": "K",
            "api_key_in": "query:api_key",
        }))
        .unwrap();
        let client = HttpMcpClient::new("c", &block, Some(&cred("abcdefgh")));
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

    /// #1194 B3 measurement harness — NOT a gate (it is `#[ignore]`d because it
    /// allocates a multi-MiB document and is only meaningful in `--release`).
    ///
    /// Run with:
    /// `cargo test --release -p calm-server --lib -- --ignored --nocapture
    ///  plugin_host::http_mcp::tests::collision_suffix_assignment_scales`
    ///
    /// The adversarial construction is the one the review channel proposed:
    /// concatenate segments, each independently spelled as one of the registered
    /// literals, so every key is DISTINCT on the wire yet redacts to the same
    /// base. That is what makes a "probe from 2 every time" loop quadratic — a
    /// construction where each base has only O(1) pre-images cannot show it,
    /// which is why the two review channels reached opposite verdicts on the
    /// same code.
    ///
    /// **The numbers below were measured under the round-2 registration**, which
    /// recognised THREE spellings, over 3⁹ = 19 683 keys / 2 480 058 key bytes,
    /// `--release`, on this box:
    ///
    /// | `scrub_value` | wall     |
    /// |---------------|----------|
    /// | probe-from-2  | 58.976 s |
    /// | per-base counter | 29.4 ms |
    ///
    /// Round 3 reverted that registration to two literals, so the construction
    /// below now uses two spellings over 2¹⁴ = 16 384 keys. The asymptotics are
    /// what the table is about and they are unchanged; the wall times have NOT
    /// been re-measured for the two-spelling shape — re-run the command above if
    /// you need current figures.
    #[test]
    #[ignore = "measurement harness; multi-MiB fixture, --release only"]
    fn collision_suffix_assignment_scales() {
        let key = "sk-a/b+c=d";
        let block: McpHttpBlock = serde_json::from_value(serde_json::json!({
            "url": "https://mcp.example.com/mcp",
            "api_key_secret": "K",
            "api_key_in": "query:api_key",
        }))
        .unwrap();
        let client = HttpMcpClient::new("c", &block, Some(&cred(key)));
        // The alphabet is stated HERE rather than read out of `secret_forms`,
        // so a change to what gets registered cannot silently change what this
        // measures.
        let spellings = [key.to_string(), percent_encode(key)];

        let mut obj = serde_json::Map::new();
        let segments = 14usize;
        let total = spellings.len().pow(segments as u32);
        for i in 0..total {
            let mut k = String::new();
            let mut n = i;
            for _ in 0..segments {
                k.push_str(&spellings[n % spellings.len()]);
                n /= spellings.len();
            }
            obj.insert(k, serde_json::json!(1));
        }
        let n = obj.len();
        let bytes: usize = obj.keys().map(String::len).sum();
        let mut v = Value::Object(obj);
        let t0 = std::time::Instant::now();
        scrub_tree(&client.secret_forms, &mut v);
        let elapsed = t0.elapsed();
        eprintln!(
            "B3: spellings={} keys={n} key_bytes={bytes} scrub_value={:?}",
            spellings.len(),
            elapsed
        );
        assert_eq!(v.as_object().unwrap().len(), n, "entries were dropped");
    }

    /// #1194 round 3 — a credential that is a FRAGMENT of the redaction marker
    /// is refused, in both directions of the rule.
    ///
    /// The refused column is the hang class: every name the rekey pass could
    /// mint for a key containing such a credential still contains it, so the
    /// fail-closed self-check is unsatisfiable. Both review channels reproduced
    /// it independently (`redacted` with `{"xredactedy":1}`; `edacted>#` with a
    /// sibling `<redacted>` key), each time as an unkillable 100%-CPU tokio
    /// worker.
    ///
    /// The accepted column is why the rule is stated as SUBSTRING-OF-THE-MARKER
    /// and not "contains any of its characters": `sk-<redacted>-x` contains the
    /// marker outright and `redactedX` shares a long prefix with it, yet neither
    /// is a piece of a name the redactor mints, so `#2`/`#3` do clear the
    /// self-check and the loop ends. Those credentials carry a residual of their
    /// own — one-pass `str::replace` can re-form them out of the marker's text —
    /// which is documented on [`overlaps_redaction_marker`] and bounded by the
    /// rekey probe budget, not by this rule.
    #[test]
    fn a_credential_that_is_a_fragment_of_the_marker_is_refused() {
        // Every case is ≥ MIN_CREDENTIAL_LEN and passes every other rule, so a
        // refusal here can only be this one.
        let refused = [
            ("redacted", "the marker's body — channel A's repro"),
            ("<redacte", "a prefix of the marker"),
            ("edacted>", "a suffix of the marker"),
            ("<redacted>", "the marker itself"),
            ("redacted>", "an interior slice reaching the end"),
            ("edacted>#", "reaching into the `#` of a suffixed name"),
            ("<redacted>#2", "a whole suffixed name — the B1 credential"),
            ("acted>#12", "straddling the `#` into a two-digit suffix"),
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
                Err(e) if !e.contains("fragment of the marker") => {
                    wrong.push(format!("{bad:?} ({why}): refused by another rule: {e}"));
                }
                Err(e) => {
                    // Operator-facing, and every one of these values is a
                    // substring of the marker — so the message must not spell
                    // the marker either.
                    assert!(!e.contains(bad), "{why}: the refusal quotes it: {e}");
                }
            }
        }
        assert!(
            wrong.is_empty(),
            "these marker fragments are still registrable, so one upstream \
             response can hang a runtime worker:\n  {}",
            wrong.join("\n  ")
        );

        for (good, why) in [
            ("redactedX", "shares a prefix, is not a substring"),
            (
                "my-redacted-key",
                "the marker's body embedded in a longer key",
            ),
            (
                "sk-<redacted>-x",
                "CONTAINS the marker, is not a piece of it",
            ),
            ("<redacted>#2x", "a suffixed name plus a trailing non-digit"),
        ] {
            assert!(
                HttpCredential::parse(good).is_ok(),
                "{why}: {good:?} must still be accepted — this rule refuses \
                 fragments of the marker, not everything that resembles it"
            );
        }
    }

    /// The predicate's own boundary, stated on the function rather than through
    /// `parse`, so the digit-run reasoning is pinned where it is derived.
    #[test]
    fn the_marker_fragment_predicate_splits_at_the_trailing_digit_run() {
        // Case 1 — wholly inside `<redacted>#`.
        assert!(overlaps_redaction_marker("<redacted>#"));
        assert!(overlaps_redaction_marker("d>"));
        // Case 2 — all digits (also refused earlier, as number-shaped).
        assert!(overlaps_redaction_marker("12"));
        // Case 3 — head is a suffix of `<redacted>#`, tail is digits.
        assert!(overlaps_redaction_marker("<redacted>#987"));
        assert!(overlaps_redaction_marker(">#4"));
        // …and the near misses on each case.
        assert!(
            !overlaps_redaction_marker("<redacted>x"),
            "not a substring: the marker is not followed by `x`"
        );
        assert!(
            !overlaps_redaction_marker("<redacted#2"),
            "a fragment with a character removed is not a fragment"
        );
        assert!(
            !overlaps_redaction_marker("redacted>2"),
            "the digit must sit behind the `#`, not against `>`"
        );
        assert!(!overlaps_redaction_marker("sk-abc-8213"));
    }

    /// The rekey loop TERMINATES on a credential the guard above deliberately
    /// accepts, instead of spinning a runtime worker.
    ///
    /// `redacted>y` is not a fragment of the marker, so `HttpCredential::parse`
    /// takes it — but `str::replace` does not rescan what it wrote, so the
    /// upstream key `redacted>yy` scrubs to `<redacted>y`, which contains the
    /// credential again. Every candidate name for that base therefore fails the
    /// fail-closed self-check, forever. The probe budget is what ends it: the
    /// whole response is refused, deterministically.
    ///
    /// Run on a worker thread with a timeout so a regression FAILS instead of
    /// wedging the test binary — `parse_scrubbed` is not cancellable and the
    /// spin is 100% CPU.
    #[test]
    fn an_unnameable_key_exhausts_the_probe_budget_instead_of_spinning() {
        let key = "redacted>y";
        assert!(
            HttpCredential::parse(key).is_ok(),
            "the premise: this credential really is accepted today"
        );
        let block: McpHttpBlock = serde_json::from_value(serde_json::json!({
            "url": "https://mcp.example.com/mcp",
            "api_key_secret": "K",
            "api_key_in": "query:api_key",
        }))
        .unwrap();
        let client = HttpMcpClient::new("c", &block, Some(&cred(key)));
        // The premise of the premise: one scrub pass really does re-form the
        // credential, which is what makes the self-check unsatisfiable.
        assert!(
            scrub_with(&client.secret_forms, "redacted>yy".to_string()).contains(key),
            "fixture no longer reproduces the non-idempotent scrub"
        );

        let forms = client.secret_forms.clone();
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let body = r#"{"result":{"redacted>yy":1}}"#;
            let _ = tx.send(parse_scrubbed(&forms, body, "tools/list").is_err());
        });
        match rx.recv_timeout(Duration::from_secs(10)) {
            Ok(is_err) => assert!(
                is_err,
                "the response must be refused, not silently handed back"
            ),
            Err(_) => panic!(
                "`parse_scrubbed` did not return within 10 s — the rekey loop is \
                 spinning again, on a tokio worker in production"
            ),
        }
    }

    /// The budget's own arithmetic, on the healthy side: a document with many
    /// colliding keys AND many pre-existing marker names must NOT trip it.
    ///
    /// This is the false-positive direction of the branch above. `2 × entries`
    /// is not a round number picked to look safe: pass 1 reserves the 40
    /// upstream `<redacted>#k` names, and the single credential key then has to
    /// probe past all of them.
    #[test]
    fn the_probe_budget_is_not_tripped_by_a_document_full_of_marker_names() {
        let key = "sk-budget-8213";
        let forms = exact_forms(&[key]);
        let mut obj = serde_json::Map::new();
        obj.insert("<redacted>".to_string(), serde_json::json!(0));
        for k in 2..=40u64 {
            obj.insert(format!("<redacted>#{k}"), serde_json::json!(k));
        }
        obj.insert(key.to_string(), serde_json::json!(1_000));
        let n = obj.len();
        let mut v = Value::Object(obj);
        scrub_value(&forms, &mut v).expect("a healthy document must not exhaust the budget");
        let obj = v.as_object().unwrap();
        assert_eq!(obj.len(), n, "an entry was dropped: {v}");
        assert_eq!(
            obj.get("<redacted>#41"),
            Some(&serde_json::json!(1_000)),
            "the credential key must have stepped past every reserved name: {v}"
        );
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
