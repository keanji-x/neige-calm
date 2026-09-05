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
//!   `tokio::time::timeout` of `2 × bringup_timeout_ms + CONNECTOR_BRINGUP_SLACK`
//!   (that constant lives in `plugin_host::mod`, and it is NOT the separate
//!   500 ms margin the connector-phase ceiling carries) — a multiple,
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
//! **The API key must never reach a string a human or an agent can read.**
//! Since #1194 the credential THIS MODULE places rides in a request HEADER —
//! `Authorization: Bearer <key>` for `api_key_in: bearer`, `<name>: <key>`
//! verbatim for `header:<name>` — and this module appends nothing to the URL.
//!
//! That is a statement about the auth branch in [`HttpMcpClient::new`] and
//! nothing else. It is **not** "no credential is ever in the URL": `mcp_http.url`
//! is an operator-written literal, and `manifest::resolve_mcp_http_url` renders
//! `{{config.*}}` slots into it, so `https://h.example/mcp?api_key=sk-a%2Fb`
//! validates and is sent exactly as written — percent-encoding and all.
//!
//! So the retirement closes the sharpest path into an operator-visible string,
//! not the class: `ureq::Error`'s `Display` still prints the request URL, and an
//! upstream is still free to quote our credential back at us in a 4xx body or a
//! tool result. All three rules stand, all enforced by tests: format a
//! `ureq::Error` only via `kind()`; run every outgoing string — error OR success
//! — through [`HttpMcpClient::scrub`]; and **scrub before you truncate**, never
//! after.
//!
//! The middle rule is enforced at ONE choke point, in
//! [`HttpMcpClient::request`]. Scrubbing only the two identity strings
//! `initialize` logs would leave the success path open: `tools/list`
//! descriptions become `ExposedTool` entries that agents and operators read,
//! and a `tools/call` result reaches the track transcript — both are
//! upstream-authored and both could echo our credential back. Header auth does
//! not change that: `{"error":"Invalid API key: sk-…"}` is a common upstream
//! shape and it reaches the track transcript — the conversation record the
//! operator and the agent read — through `tools_call`, regardless of which slot
//! we put the credential in.
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
//! `structuredContent` would put it in the tool result and the track transcript.
//! That rule is the number **grammar** (`-1234567`, `1.234567`, `1E5`,
//! `1234e567` are all of them), delegated to serde_json rather than re-derived
//! from characters — see [`is_number_shaped`].
//! Constraining the credential removes *that* case: a number-shaped credential
//! is no longer representable, so the JSON-number hole is closed structurally.
//! The same move closes a second one, and this one is the sharper of the two: a
//! credential that **shares any text with the redaction marker** can be re-formed
//! out of its own redaction, because one `str::replace` pass never rescans what
//! it wrote. `redacted>yy` scrubbed for the credential `redacted>y` yields
//! `<redacted>y` — a "redacted" string that contains the credential verbatim, on
//! its way to `ExposedTool` and the track transcript. There are exactly four ways
//! a window can overlap the marker, they are derived rather than listed, and all
//! four are refused at this layer; see [`overlaps_redaction_marker`]. That is
//! what lets [`scrub_with`] be a single pass and still state idempotence as a
//! theorem.
//!
//! **Nothing above is a completeness claim, and this header must not be read as
//! one** (#1194 residual 2 — the residual is exactly that this list *reads*
//! exhaustive and is not). [`HttpMcpClient::new`] registers **at most two
//! literals**, both matched byte-exactly by [`str::replace`]: the raw
//! credential, and the uppercase-hex percent-encoding [`percent_encode`]
//! produces. Exactly one when the credential has no reserved character, because
//! then the encoding is the identity and the dedupe collapses the pair; and
//! **none at all** when the connector holds no key, in which case
//! [`HttpMcpClient::scrub`] is the identity function.
//!
//! **What #1194 changed here, and what it did not.** It removed the
//! *emission*: this client no longer appends a percent-encoded credential to
//! its own query string, so the original reason for registering the encoded
//! literal ("an upstream echoing our own query string quotes exactly this
//! back") is gone. The literal is still registered, on a weaker but real
//! reason: an upstream only has to embed the credential in a URL inside an
//! error message to produce a percent-encoded spelling, and uppercase hex is
//! what most encoders emit.
//!
//! An earlier revision of this header claimed the retirement made the
//! case-mixed percent-encoding class *structurally disappear*. **That was
//! wrong and is recorded here because it was believed for a whole round.**
//! Retiring `query:<name>` removed one *source* of encoded spellings — ours.
//! It removed no *sink*: the upstream chooses the encoding of whatever it
//! echoes, and nothing about where we put the credential constrains that
//! choice. Percent-encoding is exactly as partially covered as it was before
//! #1194: the uppercase spelling is a registered literal, every other hex case
//! is not.
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
//! One gap of a different kind — manufactured HERE rather than chosen by the
//! upstream — used to sit in this list and is now CLOSED, recorded because the
//! list above must not be read as "everything we know about", and because two
//! earlier rounds of this file stated its status wrongly. `str::replace` does not
//! rescan what it wrote, so a credential overlapping the marker's own text was
//! re-formed by its own redaction (`redacted>y` + upstream `redacted>yy` →
//! `<redacted>y`). It is closed by refusal at the constructor rather than by
//! iterating the scrub to a fixpoint: [`overlaps_redaction_marker`] derives the
//! four ways a window can overlap the marker and refuses all four, and
//! [`scrub_with`] carries the resulting idempotence proof. It is NOT closed by
//! anything on the encoding axis, so it does not shrink the list above.
//!
//! **A residual that is accepted rather than closed** (#1194 residual 1): two
//! sibling object keys that scrub to the same marker collapse into one entry.
//! That is data loss, not disclosure, it needs a pathological upstream to
//! trigger, and the disambiguating-suffix alternative was implemented and
//! withdrawn on defects it produced. The reasoning is on the rekey branch in
//! [`scrub_value`], which is where a reader who hits the behaviour will land.
//!
//! **Why the remaining gaps are not closed by adding literals or a smarter
//! matcher.** Enumeration does not converge (an unbounded set of alphabets, and
//! within percent-encoding alone 2ⁿ hex spellings). A matcher does converge for
//! the hex case in particular — and one was written and then REVERTED, on
//! measurement: comparing every `%XX` triplet case-insensitively means a
//! hand-rolled scan instead of `str::replace`'s two-way search, and its cost is
//! paid on the CLEAN path, per `tools/call`, whether or not the body contains
//! anything resembling the credential. Measured on this box with a 257-byte
//! credential and a 4 MiB response body: `client.scrub` took **991 ms** with the
//! per-triplet matcher against **9.8 ms** with `str::replace` — a scan that
//! grows with (body × credential length) against one that is linear in the
//! body. That measurement predates #1194 and is unaffected by it, in both
//! directions: the matcher's cost is a property of the SCAN, so it is not an
//! argument against carrying a second literal either — one more `str::replace`
//! is the cost class this module already had, and it is what pays for the
//! uppercase spelling above.
//!
//! **[`scrub_value`] is not deleted by header auth, and any comment saying so is
//! wrong.** Moving the credential out of the URL changes whether *our own*
//! transport errors carry it. It changes nothing about whether the *upstream*
//! echoes it back, which is what the recursive tree scrub exists for.
//!
//! The raw-text scrub survives in exactly one place: the non-2xx arm, where the
//! body is an arbitrary error page with nothing to parse. There the
//! scrub-before-truncate rule applies and is not pedantry — an error body
//! containing the credential anywhere in it gets truncated to 512 chars, and if
//! that boundary falls inside the key the surviving prefix is no longer a
//! literal member of `secret_forms`, so a later `replace` misses it entirely
//! and a partial credential ships.
//!
//! The example used to be "an upstream that echoes the request URL back", and
//! after #1194 that is a narrower case than it reads: the request URL carries a
//! credential only when the operator hand-wrote one into `mcp_http.url` (see
//! the qualifier above). The rule is unchanged and is load-bearing for ANY echo
//! of the credential — `{"error":"Invalid API key: sk-…"}` reaches this same
//! arm — so the example is stated at that width instead.

use std::io::Read as _;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use serde_json::{Value, json};

use super::manifest::{ApiKeyIn, McpHttpBlock, ResolvedMcpUrl};
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
    /// The five rules, each tied to a concrete failure of the redaction layer
    /// and each with an `if` of its own below, in this order:
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
    ///   result and the track transcript unredacted. Making numbers scrubbable is
    ///   not an option (it would rewrite unrelated values and change the
    ///   payload's type); making the credential un-number-like is;
    /// * **at least [`MIN_CREDENTIAL_LEN`] characters** — see that constant;
    /// * **shares no text with the redaction marker** — see
    ///   [`overlaps_redaction_marker`], which enumerates the four (and only
    ///   four) ways a credential can be re-formed out of its own redaction.
    ///   Such a credential leaks: one scrub pass hands back a string that
    ///   contains it verbatim, and that string is what reaches `ExposedTool`
    ///   and the track transcript.
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
            // The marker itself is deliberately NOT spelled in this message:
            // several credentials this rule refuses are substrings of it, so
            // quoting it would quote the credential — the one thing an
            // operator-facing string on this path may never do. For the same
            // reason it does not say WHICH of the four overlap cases fired.
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

/// Can a single [`scrub_with`] pass **re-form `raw` out of its own redaction**?
///
/// This is the guard that makes one-pass scrubbing sound, and it is derived,
/// not enumerated. [`str::replace`] never rescans what it wrote, so the output
/// of one pass is `A₀ · M · A₁ · M · … · Aₙ` — upstream text `Aᵢ` with the
/// marker `M` = [`REDACTED`] spliced in at the places a form was removed. The
/// `Aᵢ` are attacker-chosen; `M` is not.
///
/// Ask when the credential `raw` can occur in that output. Every occurrence is
/// a contiguous window `X = raw`. A window lying wholly inside some `Aᵢ` is
/// impossible for the form that pass replaced — `replace` removed all of them
/// from exactly that text. So any surviving occurrence **must intersect an
/// inserted `M`**, and a contiguous window intersecting a contiguous marker
/// leaves exactly four positional relationships:
///
/// 1. `X` starts inside `M` and ends past it ⇒ `X` BEGINS with a nonempty
///    suffix of `M` — [`head_is_marker_suffix`];
/// 2. `X` starts before `M` and ends inside it ⇒ `X` ENDS with a nonempty
///    prefix of `M` — [`tail_is_marker_prefix`];
/// 3. `X` starts before `M` and ends past it ⇒ `M ⊆ X` — `raw.contains(M)`;
/// 4. `X` lies wholly inside `M` ⇒ `X ⊆ M` — [`is_marker_family_substring`],
///    which is the same case widened to the `#<n>`-suffixed family for the
///    historical reason recorded on it.
///
/// The four are exhaustive by construction (the window either contains `M`'s
/// start, `M`'s end, both, or neither-but-overlapping), so refusing all four at
/// the constructor is what lets [`scrub_with`] state idempotence as a theorem
/// rather than a hope. See that function for the corollary.
///
/// Each case is upstream-triggerable with one response, and each is a LEAK, not
/// a hang: with the credential `redacted>y` (case 1) the body `redacted>yy`
/// scrubs to `<redacted>y`, whose text contains the credential verbatim — and
/// that string is what reaches `ExposedTool` and the track transcript. Round 3
/// of #1194 refused only case 4 and stated cases 1–3 as an open residual; this
/// is that residual closed.
fn overlaps_redaction_marker(raw: &str) -> bool {
    is_marker_family_substring(raw)
        || tail_is_marker_prefix(raw)
        || head_is_marker_suffix(raw)
        || raw.contains(REDACTED)
}

/// Case 4 — `raw` is a substring of some name in the marker family
///
/// > `M = {REDACTED} ∪ {REDACTED + "#" + <nonempty decimal digits>}`
///
/// The `#<n>` half is wider than the derivation above needs (only `REDACTED`
/// itself is ever spliced in today), and it is kept deliberately: those names
/// were minted by the disambiguating-suffix rekey pass that #1194 round 4
/// withdrew, and re-introducing that pass — the one thing that would make them
/// reachable again — must not silently re-open a credential class. Refusing a
/// handful of extra 8-plus-character strings costs an operator nothing.
///
/// **The decision procedure.** Write `P = REDACTED + "#"`. `REDACTED` is a
/// prefix of `P`, so substrings of `REDACTED` are already substrings of `P` and
/// the first half of the family needs no case of its own. `P` contains no
/// digit, so for any `m = P·D` a substring `x` of `m` is exactly one of:
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

/// Case 2 — `raw` ends with a nonempty prefix of [`REDACTED`], so an upstream
/// can place the rest of `raw` immediately before a splice point and have the
/// marker's own opening text complete it (`abcde<re` + a redaction starting
/// `<re…`).
///
/// `REDACTED` is ASCII, so byte slicing it at any index is a char boundary.
fn tail_is_marker_prefix(raw: &str) -> bool {
    (1..=REDACTED.len()).any(|k| raw.ends_with(&REDACTED[..k]))
}

/// Case 1 — `raw` begins with a nonempty suffix of [`REDACTED`], so an upstream
/// can place the rest of `raw` immediately after a splice point and have the
/// marker's own closing text start it (`>y-abcdef` after a redaction ending
/// `…>`). This is the case that admitted `redacted>y`, the credential round 3
/// accepted and round 4 measured a real leak from.
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

/// One remote streamable-HTTP MCP server.
///
/// Cheap to construct, `Send + Sync`, and held behind an `Arc` inside
/// [`super::ConnectorClient`] so the process-table lock can clone it out
/// without being held across an await.
///
/// **No `#[derive(Debug)]`.** A derived `Debug` prints `header_auth` (name AND
/// value), which would defeat the hand-written redacting `Debug` on
/// [`super::ConnectorClient`] the moment anything formatted the inner client.
/// It prints `url` too, and while #1194 took the credential out of the URL,
/// `mcp_http.url` is an operator-written literal that may still carry a query
/// string of the operator's own.
pub struct HttpMcpClient {
    plugin_id: String,
    /// The resolved endpoint, verbatim. Since #1194 nothing is appended to it:
    /// the credential rides in a header. Still not logged — see
    /// [`Self::log_target`].
    url: String,
    /// `(name, value)` for the credential header: `("Authorization",
    /// "Bearer <credential>")` for `api_key_in: bearer`, `(name, credential)`
    /// verbatim for `api_key_in: header:<name>`.
    header_auth: Option<(String, String)>,
    /// Host only, for the per-call audit line required by risk R2.
    log_target: String,
    /// The literals the secret is known to take in a string an upstream may
    /// hand back: **at most two** — the raw credential and its uppercase-hex
    /// percent-encoding — exactly one when [`percent_encode`] is the identity
    /// on this credential, and **none** when the connector holds no key.
    /// **Not an exhaustive list of the shapes an upstream can echo** — see the
    /// module header for the ones that walk through. [`Self::scrub`] strips
    /// them from any string that could reach a log line, an
    /// `Event::PluginState.last_error`, an HTTP body, or a track transcript.
    /// This is the belt to the "never format a `ureq::Error`" braces: an
    /// *upstream* 4xx body may quote our credential back at us, and that path
    /// is not ours to control.
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
                    // A credential with nowhere to go: `Manifest::validate`
                    // makes this unreachable, and `new` sends nothing rather
                    // than guessing a slot. Named so the state is legible if a
                    // regression ever produces it.
                    (None, false) => "unrouted:<redacted>".to_string(),
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
    /// The key is folded into the credential header here, once, so no call site
    /// can forget it. Since #1194 it is never folded into the URL.
    ///
    /// The parameter is an [`HttpCredential`], not a `&str`, and that is the
    /// whole point: every constraint the redaction layer depends on is
    /// discharged by the only constructor of that type, so this function cannot
    /// be handed a value it cannot scrub — from here, from a test, or from a
    /// module that does not exist yet.
    ///
    /// The endpoint arrives as a [`ResolvedMcpUrl`], never as `block.url`, and
    /// that is the same argument the `api_key` parameter's type makes one line
    /// down (#1284 §2.3(c)): the only constructor of that type is
    /// `manifest::resolve_mcp_http_url`, which renders the url's
    /// `{{config.*}}` slots, re-runs the manifest's own URL validator on the
    /// result and — for a connector that holds an API key — refuses any render
    /// that moved the origin. Taking a `&str` here would leave "did anyone
    /// resolve this?" as a thing every call site has to remember.
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
                // The value SHAPE is the point, not just the location: the
                // probed upstream rejects `Authorization: <key>` without the
                // `Bearer ` prefix, so spelling this as `header:Authorization`
                // would have failed at request time. See `ApiKeyIn`.
                Some(ApiKeyIn::Bearer) => {
                    header_auth = Some(("Authorization".to_string(), format!("Bearer {key}")));
                }
                // Verbatim, no prefix — this is the `X-API-Key`-style escape
                // hatch and its whole value is that the operator controls the
                // exact bytes.
                Some(ApiKeyIn::Header(name)) => {
                    header_auth = Some((name, key.to_string()));
                }
                // `Manifest::validate` rejects every string that lands here
                // (unknown scheme, and the retired `query:<name>`); treat a
                // future regression as "send nothing" rather than guessing a
                // slot and leaking the key into it. The credential is still
                // registered for scrubbing below — an unrouted key is not a
                // reason to stop redacting it, and `read_secrets` may already
                // have put it somewhere this client did not.
                None => {}
            }
            // AT MOST TWO literals — the raw credential and its uppercase-hex
            // percent-encoding — exactly one when the credential has no
            // reserved character (then `percent_encode` is the identity and the
            // dedupe below collapses them), and NONE when the connector holds
            // no key at all, because this whole block is inside `if let
            // Some(key)`. **This list is NOT exhaustive**, deliberately and with
            // the gaps named: see the module header, including why the
            // case-insensitive percent matcher that briefly lived here was
            // reverted on measurement (991 ms vs 9.8 ms on a clean 4 MiB body).
            //
            // **Why the encoded form is still registered after #1194 retired
            // `query:<name>`.** It is no longer the spelling THIS client puts on
            // the wire — nothing is appended to the URL any more — so the
            // original justification ("an upstream echoing our own query string
            // quotes this back") is gone. It is kept on a different and weaker
            // one: an upstream only has to embed the credential in a URL inside
            // an error message to produce a percent-encoded spelling, and
            // uppercase hex is the spelling most encoders emit. Registering it
            // costs one more `str::replace` per response — the same cost class
            // this module carried before #1194, and NOT the reverted matcher's
            // cost, which was a per-triplet case-insensitive scan. Deleting the
            // emission is right and stays done; deleting the redaction form
            // bought nothing and gave up real coverage.
            //
            // Longest first, so scrubbing the encoded form is not pre-empted by
            // a shorter raw substring match. The encoded form is never shorter
            // than the raw one (`percent_encode` either copies a byte or expands
            // it to three), so this sort puts the encoded form first whenever the
            // two differ.
            //
            // Round 5 argued this ordering is decorative on the grounds that the
            // raw form can never occur INSIDE the encoded one — `R ≠ E` implies
            // `R` holds a reserved character, and a reserved character is not in
            // `E`. That argument is wrong for exactly one character: `%` is
            // reserved, and `percent_encode` emits `%` itself. Witness:
            // `R = "abcde%25"` (a credential `HttpCredential::parse` accepts)
            // encodes to `E = "abcde%2525"`, of which `R` is a PREFIX. On a body
            // echoing `E` back, raw-first would yield `<redacted>25` — the
            // encoded literal chewed in half, with the tail no longer matching
            // any registered form. Longest-first yields `<redacted>`. So keep
            // the sort; it is load-bearing for real inputs, not for the theorem
            // on `scrub_with` (which quantifies over the whole form list and
            // does not depend on the order).
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
            log_target: log_target(&url),
            url,
            header_auth,
            secret_forms,
            agent: ureq::AgentBuilder::new()
                // The manifest names the one endpoint this credential belongs
                // to. Following an upstream redirect would replay arbitrary
                // custom auth headers (for example `X-API-Key`) to the target.
                .redirects(0)
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

    /// Host (and port) of the endpoint — the only part of the URL that is safe
    /// to log.
    ///
    /// **This survives #1194 and must not be deleted.** The kernel's credential
    /// no longer rides in the query string, but `mcp_http.url` is an
    /// operator-written literal: an operator is free to write
    /// `https://h.example/mcp?api_key=…` (or any other secret) directly into
    /// it, and `resolve_mcp_http_url` renders `{{config.*}}` slots into it on
    /// top of that. "The credential is not in the URL" is a statement about
    /// what THIS module appends, not about what the URL contains.
    pub fn log_target(&self) -> &str {
        &self.log_target
    }

    /// Replace every literal occurrence of the API key with `<redacted>`.
    ///
    /// Applied to EVERY string this module can hand back, because those
    /// strings all converge on operator- and agent-visible sinks:
    /// `tracing::warn!(reason)`, the persisted+broadcast
    /// `Event::PluginState.last_error`, the `POST /enable` 503 body, and (via
    /// `tools_call`) the track transcript.
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
        // itself is never logged: the kernel's credential is in a header since
        // #1194, but `mcp_http.url` is an operator-written literal that may
        // carry a secret of the operator's own in its query string.
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
            // `Display` prints the full URL first. This client no longer folds
            // the API key into that URL (#1194), but the URL is an
            // operator-written literal that may carry one anyway. Only `kind()`
            // — a closed set of English descriptions — is safe.
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
        // `tools/call` payload bound for the track transcript, and the
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
///
/// Rekeying is NOT entry-count preserving, deliberately — see the comment on
/// the rekey branch for the accepted behaviour and why the alternative was
/// withdrawn.
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
            // `contains` is not a second matcher beside `scrub_with`'s: it is
            // the exact predicate `str::replace` decides on (and the one
            // `scrub_with` itself uses to skip the allocation), so a key cannot
            // be judged "needs rekeying" by one rule and rewritten by another.
            let rekey = map
                .keys()
                .any(|k| forms.iter().any(|f| k.contains(f.as_str())));
            if rekey {
                // #1194 residual 1, and the accepted behaviour is the LOSSY
                // one: two sibling keys that scrub to the same marker collapse
                // into a single entry, and the surviving value is whichever the
                // `collect()` below inserts last (`BTreeMap` order — the
                // `preserve_order` feature is off).
                //
                // **What that costs, precisely.** It is DATA LOSS, not a leak:
                // every colliding key rendered to the same marker by
                // construction, so the reader learns nothing about the
                // credential from the collapse, and no value is disclosed that
                // redaction was supposed to hide. What is lost is one entry of
                // an upstream document.
                //
                // **What it takes to trigger.** Two sibling keys of one object
                // must render to the same marker. Since #1194 the form list is
                // a single literal — the raw credential — so the two spellings
                // that used to do it (raw + the percent-encoding this client
                // emitted) no longer both exist. What remains is an upstream
                // that returns the credential as one key AND the literal string
                // `<redacted>` as a sibling key of the same object: the first is
                // rewritten to the marker, the second already is it. A server
                // that reflects our credential into its own schema keys next to
                // a hard-coded `<redacted>` sibling is already pathological;
                // #1194 says so in as many words and offers "a comment or a
                // dedupe suffix" as alternative acceptance criteria. This is the
                // comment.
                //
                // **Why not the dedupe suffix.** It was implemented on this
                // branch and withdrawn, on defects the implementation actually
                // produced rather than imagined ones:
                //
                // * a minted name can BE the credential. `<redacted>#2` passes
                //   every rule in `HttpCredential::parse` as it stood, so an
                //   operator holding that key would have the redactor emit the
                //   secret verbatim — after redaction — into `ExposedTool` and
                //   the track transcript. Turning "data loss" into a leak is a
                //   strictly worse trade than the loss;
                // * guarding that by re-scrubbing each candidate name and
                //   accepting only a fixpoint does not terminate. The scrubber
                //   was not idempotent for credentials overlapping the marker's
                //   own text, so every candidate for a base could fail the
                //   self-check forever — measured at 9 min 17 s of 100% CPU on
                //   a tokio worker, uncancellable, from ONE upstream response;
                // * bounding that by a probe budget then made a redaction pass
                //   able to refuse a whole healthy response, on arithmetic that
                //   itself needed a proof.
                //
                // The marker-overlap guard in `HttpCredential::parse` has since
                // made `scrub_with` idempotent for every credential this client
                // can hold, so the second bullet no longer bites — but the first
                // one is untouched by that, and the whole apparatus buys back
                // one entry of a pathological document. Both directions of this
                // trade have been paid for once; do not re-open it without a
                // real upstream that needs the entry.
                let taken = std::mem::take(map);
                *map = taken
                    .into_iter()
                    .map(|(k, v)| (scrub_with(forms, k), v))
                    .collect();
            }
            // Children are walked whether or not this map was rekeyed. Moving
            // this into an `else` is a LEAK, not a refactor:
            // `{"<credential>": "see key <credential> in the docs"}` would come
            // back with the key redacted and the value verbatim. It survived a
            // 139-case suite once; `a_rekeyed_object_still_scrubs_its_children`
            // is what kills it now.
            for (_, child) in map.iter_mut() {
                scrub_value(forms, child);
            }
        }
        // Numbers, booleans and null carry no string to redact. A number-shaped
        // credential — the one value an upstream could echo back HERE, as a
        // JSON number — cannot exist: `HttpCredential::parse` refuses anything
        // serde_json's scanner accepts as a bare literal (`is_number_shaped`).
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
/// **This is one pass per pattern, and one pass is enough — but only because
/// the constructor makes it enough.** `str::replace` scans left to right and
/// never rescans what it wrote, so a substitution CAN in principle manufacture
/// a match the pass has already walked past: with the credential `redacted>y`,
/// `"hit redacted>yy end"` comes back as `"hit <redacted>y end"` — the
/// credential, re-formed out of the marker's own text. Two successive revisions
/// of this comment got the resulting property wrong in opposite directions
/// (round 2 claimed "substitution does not manufacture a new match", disproved
/// with `"hit redacted%3E end"` for the credential `redacted>`; round 3 then
/// declared the class simply open). Round 4 closed it at the constructor, so
/// the property is restated here from scratch rather than inherited:
///
/// > **For every form list [`HttpMcpClient::new`] can build, the output of one
/// > `scrub_with` pass contains no registered form.** Hence
/// > `scrub_with(f, scrub_with(f, x)) == scrub_with(f, x)`.
///
/// At most two forms can be registered (and none for a keyless connector, for
/// which this function is never reached with a non-empty list), and only ONE of
/// them is a value [`HttpCredential::parse`] ever inspected: the raw
/// credential. The other is the percent-encoding, which is derived after the
/// check. So establish it first, before the induction leans on it. Either the
/// encoding equals the raw credential (no reserved characters — and then the
/// dedupe in `new` collapses the two), or it contains a `%` triplet, and in
/// every case [`percent_encode`] emits `<` and `>` as `%3C`/`%3E`. A form with
/// no `<` and no `>` cannot begin with a suffix of `<redacted>` (all of which
/// end in `>`), cannot end with a prefix of it (all of which begin with `<`),
/// and cannot contain it; the one marker-family substring free of both
/// characters is `redacted`-shaped text, which the encoding of a credential can
/// only be if the credential already is that text — refused. **Both**
/// registered forms therefore satisfy all four refusals, which is what the
/// induction below quantifies over.
///
/// Proof, by induction over the forms in order. After the pass for form `Fₖ`,
/// the string is carried-over text with the marker `M` spliced in. No
/// occurrence of `Fₖ` can lie wholly inside carried text — `replace`'s scan is
/// exhaustive left to right, so a full occurrence at a position it did not
/// consume is a contradiction — therefore any surviving occurrence intersects
/// an inserted `M`, which forces one of the four relationships enumerated on
/// [`overlaps_redaction_marker`], all four of which are refused for BOTH
/// registered forms (for the raw credential by [`HttpCredential::parse`]; for
/// the percent-encoding by the paragraph above). Earlier forms `F₁…Fₖ₋₁` were
/// already absent from this pass's input by the induction hypothesis, and a
/// substring of a string that lacks `Fᵢ` still lacks it, so the only new
/// occurrences they could gain are ones intersecting the `M`s this pass
/// inserted — refused by the same four cases, on the same two grounds.
///
/// The scope of the theorem is exactly "form lists `new` builds". This is a free
/// function taking an arbitrary `&[String]`, and tests pass hand-built lists
/// that do NOT satisfy the precondition; for those, idempotence is not claimed
/// and nothing in production depends on it.
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

/// `scheme://host[:port]`, dropping path and query.
///
/// Retained after #1194 took the kernel's credential out of the query string:
/// `mcp_http.url` is an operator-written literal (with `{{config.*}}` slots
/// rendered into it), so a secret in its query string is the operator's to put
/// there and this module's to not log. See [`HttpMcpClient::log_target`].
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

/// Minimal percent-encoding, uppercase hex. `url` is not a dependency of this
/// crate and pulling one for eight bytes of logic is not worth it.
///
/// **What this is for, restated after #1194.** Until #1194 this produced the
/// spelling the client itself put in its own query string, so an upstream
/// echoing that query string back quoted exactly this. The client appends
/// nothing to the URL any more, so that is no longer why it exists. It exists
/// now because an upstream that embeds the credential in a URL inside an error
/// message (or in any other percent-encoded context) emits a percent-encoded
/// spelling of its own, and uppercase hex is what most encoders emit — so this
/// is the single encoded spelling worth carrying as a scrub literal.
///
/// **Uppercase hex is one spelling, not a matching rule.** Other hex cases
/// (`%2f`, and mixed per-triplet spellings a re-encoding hop may produce) are
/// NOT covered; two attempts to cover them — a second lowercase literal, then a
/// per-triplet case-insensitive matcher — were both reverted, the first as
/// non-convergent enumeration and the second on measured clean-path cost. The
/// module header carries the numbers and the full gap list.
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

    /// The endpoint these fixtures run against — through the REAL resolver
    /// (#1284 §2.3(c)), with no configuration in force. Every block below
    /// carries a literal url, so this is the identity plus the manifest's own
    /// URL validation; going through the resolver anyway is what keeps these
    /// tests unable to construct a client the production path could not.
    fn resolved(block: &McpHttpBlock) -> ResolvedMcpUrl {
        super::super::manifest::resolve_mcp_http_url(block, &serde_json::Map::new())
            .unwrap_or_else(|e| panic!("fixture url must resolve: {e}"))
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
    /// the track transcript in the clear — the exact leak the rule exists to
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
        let client = HttpMcpClient::new("c", &resolved(&block), &block, None);
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

    /// #1194 — `bearer` must send `Authorization: Bearer <credential>`.
    ///
    /// The assertion is on the header VALUE, not on the header's presence, and
    /// that is the whole point: the bug this form fixes is that today's
    /// `header:<name>` sets the value to the RAW credential, so a manifest
    /// spelled `header:Authorization` sends `Authorization: sk-…` and the probed
    /// upstream answers `No API key provided`. A test asserting only "an
    /// `Authorization` header exists" passes on exactly that bug.
    ///
    /// **Mutation witness** — change the `Bearer` arm in `HttpMcpClient::new`
    /// to `header_auth = Some(("Authorization".into(), key.to_string()))` (the
    /// raw-value bug). This test goes red on
    /// `left: Some(("Authorization", "sk-a-b-c-8213"))` /
    /// `right: Some(("Authorization", "Bearer sk-a-b-c-8213"))`.
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
        // …and nothing was appended to the URL.
        assert_eq!(client.url, "https://mcp.example.com/mcp");
    }

    /// The URL is the resolved base, verbatim — including an operator-written
    /// query string. #1194 deleted the folding branch; this pins that nothing
    /// re-grows it, and that `log_target` still drops the operator's query.
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

    /// `header:<name>` keeps sending the credential VERBATIM. That is the whole
    /// value of the form — an `X-API-Key`-style server wants the bare bytes —
    /// and it is why `bearer` is a separate variant rather than a spelling of
    /// `header:Authorization`.
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

    /// A derived `Debug` would print `header_auth` (name AND value), defeating
    /// `ConnectorClient`'s own redacting `Debug`.
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

    /// The scrubber must catch the percent-encoded form too.
    ///
    /// **Why this test survived #1194.** Its original justification — "that is
    /// the literal an upstream echoing our query string back would use" — died
    /// with the query string. One round of this branch deleted the encoded
    /// assertion on that basis and replaced this test with one that PINNED the
    /// resulting leak. Both review channels measured the same counter-example
    /// (`sk-a/b+c` in a body reading `boom: sk-a%2Fb%2Bc` came back unredacted)
    /// and the deletion was overruled: an upstream only has to put the
    /// credential in a URL inside an error message to produce this spelling,
    /// and one more `str::replace` is what it costs to keep catching it.
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

        // The exact counter-example the review channels ran. It is written as
        // an upstream error body quoting a URL, because that — not our own
        // query string — is the shape that produces this spelling now.
        let enc_msg = client.scrub(format!("boom: fetching https://h/x?k={encoded} failed"));
        assert!(!enc_msg.contains(&encoded), "{enc_msg}");
        assert!(enc_msg.contains("<redacted>"), "{enc_msg}");
    }

    /// The credential inside the header value we send is one literal
    /// occurrence, so an upstream quoting the whole header back is covered —
    /// and the still-uncovered neighbour is pinned rather than papered over.
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

        // The gap the module header names, pinned so the header's claim and the
        // code stay in step: the LOWERCASE hex spelling of the same bytes is
        // not a registered literal and is not matched.
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

    /// THE round-2 finding: the upstream body used to be clamped to 512 chars
    /// and only scrubbed afterwards. When the boundary falls inside the key the
    /// surviving prefix is no longer a literal member of `secret_forms`, so the
    /// later `replace` matches nothing and a partial credential reaches the 503
    /// body, `PluginState.last_error`, and the track transcript.
    ///
    /// **Scope, honestly stated.** This is a unit test of the two free
    /// functions and of the claim that their ORDER matters: the second half
    /// proves the reversed order demonstrably leaks, so the first half is not
    /// vacuous. It does NOT reach `request`, so on its own it cannot fail when
    /// the production lines are swapped. The call-site witness is the
    /// integration test `a_4xx_body_echoing_the_credential_never_leaks_a_partial_key`
    /// in `tests/cases/connector_host.rs`, which drives a real `spawn` against
    /// a stub that echoes the `Authorization` header inside an over-long 4xx
    /// body and asserts the surviving `last_error` carries no key prefix.
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

    /// #1194 residual 1 — the ACCEPTED behaviour, pinned so it cannot drift
    /// silently in either direction.
    ///
    /// Two sibling keys that scrub to the same marker collapse into ONE entry.
    /// The issue offers "a comment or a dedupe suffix" as acceptance criteria;
    /// round 4 took the comment — it is on the rekey branch in `scrub_value`,
    /// with the defects the suffix machine actually produced — and this test is
    /// what keeps that comment describing the code.
    ///
    /// The load-bearing assertions are the SECURITY ones: no surviving key
    /// holds either credential form, and a key that never held it is untouched.
    /// The collapse itself is data loss, not disclosure. The entry count is
    /// asserted exactly rather than as `<= 3` on purpose: if disambiguation is
    /// ever reintroduced, this must go red and be re-read, not pass silently
    /// under a changed contract.
    ///
    /// The form list is the PRODUCTION one — built by `HttpMcpClient::new` from
    /// a credential `HttpCredential::parse` accepts — not a hand-rolled `vec!`:
    /// a fixture that mints its own literals could keep asserting the collapse
    /// after the constructor stopped producing the pair that causes it.
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

        // Neither credential form survives in any key — the property the pass
        // exists for, and the one the collapse does not touch.
        for k in obj.keys() {
            assert!(!k.contains(key), "raw key survived: {v}");
            assert!(!k.contains(&encoded), "encoded key survived: {v}");
        }
        assert_eq!(
            obj.get("safe"),
            Some(&serde_json::json!(3)),
            "a key that never held the credential must be untouched: {v}"
        );
        // The accepted loss: three entries in, two out, one of the two
        // credential-keyed values gone. WHICH one survives is `BTreeMap`
        // ordering and is deliberately not asserted — it is not a property this
        // module promises anybody.
        assert_eq!(
            obj.len(),
            2,
            "residual 1 says these two collapse; if they no longer do, the \
             rekey branch's comment is stale: {v}"
        );
        assert!(obj.contains_key("<redacted>"), "{v}");
    }

    /// #1194 channel-A A1 — an object whose KEY was rekeyed must still have its
    /// children walked.
    ///
    /// Moving the `for (_, child) in map.iter_mut()` recursion inside the `else`
    /// of the rekey branch survives the whole 139-case suite, and it is a LEAK:
    /// `{"<credential>": "see key <credential> in the docs"}` comes back with
    /// the key redacted and the string value verbatim — into the `ExposedTool`
    /// description an agent reads and into the track transcript.
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
    /// At most two literals — the raw credential and the uppercase-hex
    /// percent-encoding — longest first. There is deliberately no assertion
    /// here about `%2f` or mixed-case triplets: they are NOT covered, and that
    /// gap is documented in the module header (and pinned by
    /// `an_echoed_authorization_header_is_scrubbed_and_a_lowercase_triplet_is_not`)
    /// rather than enshrined here.
    #[test]
    fn registration_is_the_raw_form_and_the_uppercase_encoding_longest_first() {
        // Reserved characters in three classes — `/`, `+`, `=` — so the encoded
        // form is visibly a different string.
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

        // Both spellings really are scrubbed…
        for spelling in [key, upper.as_str()] {
            let msg = client.scrub(format!("upstream echoed {spelling} back"));
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
            "api_key_in": "bearer",
        }))
        .unwrap();
        let client = HttpMcpClient::new("c", &resolved(&block), &block, Some(&cred(key)));

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

        let msg = client.scrub(format!("boom {encoded}"));
        assert!(!msg.contains(&encoded), "{msg}");
        assert!(msg.contains("<redacted>"), "{msg}");
    }

    /// A credential with no reserved characters encodes to itself: the dedupe
    /// must leave exactly ONE pattern, not two copies whose second pass would
    /// re-scan the already-substituted text. The `header:<name>` placement
    /// registers the same literals as `bearer` does — the `Bearer ` prefix is a
    /// wire detail, not a scrub form.
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

    /// #1194 round 4 — a credential that OVERLAPS the redaction marker is
    /// refused, in each of the four ways a window can overlap it.
    ///
    /// Every refused value is a LEAK, not a style rule: one scrub pass hands
    /// back a string that contains the credential verbatim, and that string is
    /// what reaches `ExposedTool` and the track transcript. Round 3 refused only
    /// the substring case (on a different argument — it hung the withdrawn rekey
    /// pass) and recorded the rest as an open residual, with `redacted>y` +
    /// upstream `redacted>yy` as the witness. Two entries below are round 3's
    /// verdicts inverted: `sk-<redacted>-x` and `<redacted>#2x` were ACCEPTED
    /// then and are refused now, because they contain the marker outright.
    ///
    /// Every case clears `MIN_CREDENTIAL_LEN` and every other rule, so a refusal
    /// here can only be the marker rule — which is asserted, not assumed.
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
            // Case 1 — the credential BEGINS with a suffix of the marker, so an
            // upstream supplies its tail immediately after a splice point.
            ("redacted>y", "round 3's accepted leak, reproduced below"),
            (">y-abcdef", "one marker character is enough"),
            ("d>abcdefg", "two"),
            // Case 2 — the credential ENDS with a prefix of the marker, so an
            // upstream supplies its head immediately before a splice point.
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
        // These are real API-key shapes and shapes that merely RESEMBLE the
        // marker without sharing an edge with it.
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

    /// Each of the four clauses has a witness NO other clause catches.
    ///
    /// This is the mutation surface, made explicit: short-circuit any single
    /// clause of `overlaps_redaction_marker` to `false` and exactly one group
    /// below goes red. A lump assertion (`overlaps(x)` for a pile of values)
    /// would let three clauses cover for a deleted fourth.
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

    /// The family predicate's own boundary, stated on the function rather than
    /// through `parse`, so the digit-run reasoning is pinned where it is
    /// derived. (Through `parse` these near misses are useless: `<redacted>x`
    /// is not a family substring but IS caught by the contains clause.)
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

    /// The idempotence theorem on [`scrub_with`], pinned at the boundary the
    /// guard now draws.
    ///
    /// One `str::replace` pass never rescans what it wrote, so "one pass is
    /// enough" is a property of the CREDENTIAL, not of the scrubber. This test
    /// stands on both sides of the line: the shape the guard refuses really does
    /// re-form itself in one pass (so the rule is not decoration), and its
    /// nearest legal neighbour — same length, one character off the marker's
    /// edge — comes out clean and stays clean under a second pass.
    #[test]
    fn one_scrub_pass_is_enough_exactly_because_the_guard_refuses_the_overlap() {
        // The refused side. `exact_forms` is hand-built on purpose: production
        // can no longer assemble this list, which is the point.
        let leaky = "redacted>y";
        assert!(
            HttpCredential::parse(leaky).is_err(),
            "this credential is what round 3 accepted; it must be refused now"
        );
        let once = scrub_with(&exact_forms(&[leaky]), "hit redacted>yy end".to_string());
        assert!(
            once.contains(leaky),
            "the premise of the whole rule: one pass really does re-form this \
             credential out of the marker's own text: {once}"
        );

        // The legal neighbour, driven through the production constructor so the
        // registered form list is the real one.
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

        // The same, for a credential whose percent-encoded form is a second
        // registered literal: the encoding can carry no `<` or `>`, so it cannot
        // overlap the marker either, and two forms do not reopen the class.
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

    /// The `None`/unparseable arm of `HttpMcpClient::new`: a credential with no
    /// legal placement is sent NOWHERE, and is still registered for scrubbing.
    ///
    /// `Manifest::validate` makes this unreachable through the manifest route —
    /// including for the retired `query:api_key`, which is the exact string
    /// used here. This drives the constructor directly, which is the only way
    /// to reach the arm, and pins the fail-closed choice: no header is
    /// invented, the URL is untouched, nothing is guessed.
    ///
    /// **Mutation witness** — make the `None` arm fall back to
    /// `header_auth = Some(("Authorization".into(), format!("Bearer {key}")))`.
    /// The first assertion goes red with
    /// `left: Some(("Authorization", "Bearer sk-unrouted-8213"))` /
    /// `right: None`.
    #[test]
    fn an_unroutable_api_key_in_sends_the_credential_nowhere() {
        let key = "sk-unrouted-8213";
        for placement in ["query:api_key", "cookie:k", "body:token", "api_key"] {
            let block = McpHttpBlock {
                url: "https://mcp.example.com/mcp".to_string(),
                api_key_secret: Some("K".to_string()),
                api_key_in: Some(placement.to_string()),
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

    /// No key configured ⇒ nothing to scrub, and no accidental blanket
    /// replacement of the empty string.
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
