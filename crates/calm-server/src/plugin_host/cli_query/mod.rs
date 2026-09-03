//! #1164 P3 — the `kind: cli-query` connector execution runtime.
//!
//! P1 taught the manifest to parse and validate a `cli_query` block; P3 is the
//! half that actually runs it. A `cli-query` connector is a **read-only local
//! query CLI**: the kernel pins one absolute binary path at enable time and, per
//! `tools/call`, execs it directly with a fixed argv template.
//!
//! # What this deliberately is NOT
//!
//! * **It does not go through the forge-action adapter.** That adapter generates
//!   a `/bin/sh` script and hands the child the forge credential passthrough
//!   (`FORGE_CREDENTIAL_ENV_KEYS`). A query connector is authored by whoever
//!   wrote the manifest, is reachable by any agent that can call its tools, and
//!   has no business holding the operator's git identity — so it gets
//!   `env_clear()` plus an explicit, enumerated environment, **and** the
//!   enumeration is denylisted against that exact set so a manifest cannot name
//!   its way back in (design §2.3, §4 acceptance #4).
//! * **It never consults `trusted_forge_plugin`.** Connector tools materialize
//!   with `kind: None`, so they cannot reach the forge arm of dispatch at all;
//!   this module does not re-derive that decision.
//! * **There is no shell.** `Command::new(<pinned absolute path>).args(...)`.
//!   A `{{slot}}` template occupies a WHOLE argv element and is replaced
//!   wholesale by exactly one argument (`manifest::argv_slot`). No string
//!   concatenation, no word splitting, no glob expansion — so a value like
//!   `; rm -rf /` is one literal argv element and nothing else.
//!
//! # Shape
//!
//! * [`bring_up`] runs ONCE per enable: resolve + pin the command, read
//!   `secrets.json`, build the child environment, probe an informational
//!   fingerprint. Everything it can fail on produces an operator-facing reason
//!   string, which `PluginHost::spawn_cli_query` turns into
//!   `Unavailable{reason}` + a 503.
//! * [`CliQueryRuntime::tools_call`] runs per call: render argv, exec, capture,
//!   cap, and answer in the same [`CallToolResult`] envelope
//!   `HttpMcpClient::tools_call` uses.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use serde_json::Value;

use super::child_process::{
    ChildFinishError, SpawnTimedOut, finish_within, read_capped, set_process_group_leader,
    spawn_within,
};
use super::manifest::{ArgvSlot, CliQueryTool, argv_slot};
use super::mcp::{CallToolResult, ContentBlock, RpcError};

/// Wall-clock bound on ONE `cli-query` bring-up (resolution + the `--version`
/// fingerprint probe).
///
/// It exists for the same reason `mcp_http`'s does: `AppState::new` awaits the
/// autospawn path **inline**, so an unbounded bring-up is a boot stall. A
/// `--version` that hangs (a binary that waits on stdin, an NFS mount that
/// stopped answering) is exactly that.
///
/// Not operator-configurable on purpose. `cli_query.timeout_ms` is the
/// steady-state `tools/call` budget and may legitimately be long; borrowing it
/// for boot would re-create the "one knob, two opposite constraints" defect
/// documented on [`super::manifest::MCP_HTTP_MAX_BRINGUP_TIMEOUT_MS`].
/// [`super::connector_bringup_budget`] returns this for a `cli-query` manifest,
/// and it is well under [`super::MAX_CONNECTOR_BRINGUP_BUDGET`], so the boot
/// ceiling stays the documented one.
pub const CLI_QUERY_BRINGUP_BUDGET: Duration = Duration::from_secs(5);

/// Sub-budget for the `--version` probe alone, inside
/// [`CLI_QUERY_BRINGUP_BUDGET`]. Strictly smaller so a hung probe fails as
/// "fingerprint unavailable" (which is informational and must NOT fail
/// bring-up) rather than by consuming the whole outer bound and taking the
/// enable down with it.
const VERSION_PROBE_BUDGET: Duration = Duration::from_secs(2);

/// stderr capture cap. Unlike stdout — whose cap is the manifest's
/// `max_output_bytes`, because stdout is the answer — stderr is diagnostics, so
/// a fixed, small window is enough and keeps a chatty binary from being a
/// memory amplifier.
pub const CLI_QUERY_MAX_STDERR_BYTES: usize = 4 * 1024;

/// stdout cap for the `--version` probe. Only the FIRST LINE is ever used, so
/// this is generous already; it exists because `.output()` used to buffer the
/// whole stream inside [`VERSION_PROBE_BUDGET`].
const PROBE_MAX_STDOUT_BYTES: usize = 4 * 1024;

// ---------------------------------------------------------------------------
// Runtime
// ---------------------------------------------------------------------------

/// One enabled `cli-query` connector.
///
/// Held behind an `Arc` inside [`super::ConnectorClient::Cli`]; every field is
/// resolved once at bring-up so a `tools/call` does no PATH lookup, no secret
/// read and no manifest walking.
pub struct CliQueryRuntime {
    plugin_id: String,
    /// The pinned **absolute** program path. `tools_call` execs exactly this —
    /// there is no second PATH resolution at call time, so replacing an earlier
    /// `PATH` entry after enable cannot re-target an already-running connector.
    program: PathBuf,
    /// Informational only (`<command> --version`'s first line, or size+mtime).
    /// Logged at bring-up; never a bring-up failure.
    fingerprint: String,
    /// The complete child environment, including secret values. This is why
    /// [`super::ConnectorClient`]'s `Debug` prints no payload.
    env: BTreeMap<String, String>,
    /// Declared tools by name.
    tools: BTreeMap<String, CliQueryTool>,
    /// #1284 §2.3(b) — the rendered `{{config.<key>}}` slot values, resolved
    /// ONCE at bring-up from `defaults ⊕ user_config`.
    ///
    /// A separate map from the agent's `arguments` by construction, which is
    /// the isolation: `tools_call` never merges the two and never falls back
    /// from one to the other, so an argument named `config.x` has nowhere to
    /// land. Cached here for the same reason [`Self::env`] is (§2.4, F10) — a
    /// configuration change takes effect on the next bring-up, not mid-flight.
    config: BTreeMap<String, String>,
    /// `cli_query.timeout_ms`.
    timeout: Duration,
    /// `cli_query.max_output_bytes`.
    max_output_bytes: usize,
}

impl CliQueryRuntime {
    /// The pinned absolute program path.
    pub fn program(&self) -> &Path {
        &self.program
    }

    /// `<command> --version`'s first line, or a `size=…, mtime=…` fallback.
    pub fn fingerprint(&self) -> &str {
        &self.fingerprint
    }

    /// Test/diagnostic view of the child environment KEYS. Deliberately not the
    /// values: they include secrets.
    pub fn env_keys(&self) -> Vec<&str> {
        self.env.keys().map(String::as_str).collect()
    }

    /// The `PATH` the child is actually exec'd with.
    ///
    /// The one child-environment VALUE that is safe to expose: it is derived
    /// from the service PATH and the manifest's `search_path_extra`, never from
    /// `secrets.json`. Exposed because "what the child's PATH ends up being" is
    /// the property r2 G2 is about, and asserting it on the resolution helper
    /// instead would test a different function than the one that builds the
    /// environment.
    /// The stdout cap the runtime will actually enforce.
    ///
    /// Exposed so a test can pin that bring-up read `CliQueryBlock`'s CLAMPING
    /// getter rather than the raw `Option<usize>` field (r4 I3). Asserting only
    /// on a small child's output cannot see that difference: `"hello\n"` is
    /// under both the ceiling and `usize::MAX`, so the raw-field mutation stays
    /// green no matter how the answer is checked.
    pub fn max_output_bytes(&self) -> usize {
        self.max_output_bytes
    }

    pub fn child_path(&self) -> &str {
        self.env.get("PATH").map(String::as_str).unwrap_or_default()
    }

    /// Run one declared tool.
    ///
    /// Mirrors [`super::http_mcp::HttpMcpClient::tools_call`]'s envelope: an
    /// `Ok(CallToolResult)` whose `is_error` reports the CHILD's verdict, and an
    /// `Err(RpcError)` only for things that are not a child verdict at all
    /// (unknown tool, malformed arguments, spawn failure, budget expiry).
    pub async fn tools_call(
        &self,
        name: &str,
        arguments: Value,
    ) -> Result<CallToolResult, RpcError> {
        let tool = self.tools.get(name).ok_or_else(|| {
            RpcError::method_not_found(&format!("tools/call: {}_{name}", self.plugin_id))
        })?;
        let argv = render_argv(tool, &arguments, &self.config).map_err(RpcError::invalid_params)?;

        tracing::info!(
            plugin_id = %self.plugin_id,
            tool = %name,
            program = %self.program.display(),
            argc = argv.len(),
            "cli-query connector tools/call"
        );

        // ONE deadline for the whole call, spent across two phases: the spawn
        // and the capture. The spawn is inside it because `fork`+`execve`
        // against a wedged mount can block for as long as the mount is wedged
        // (r2 G8) — a bound that starts only after the child exists is not the
        // bound `cli_query.timeout_ms` advertises.
        let deadline = tokio::time::Instant::now() + self.timeout;

        let mut cmd = tokio::process::Command::new(&self.program);
        cmd.args(&argv)
            // `env_clear` FIRST, then only what `build_child_env` enumerated.
            // Everything the design refuses this connector — the forge
            // credential passthrough above all — is excluded by construction
            // rather than by a denylist.
            .env_clear()
            .envs(&self.env)
            // A query CLI has no input. Leaving stdin inherited would let a
            // binary that prompts block on the server's own stdin.
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            // The child must not outlive the future that owns it: a dropped
            // `tools_call` (client hangup, task abort) would otherwise leak a
            // process with the connector's secret environment. `kill_on_drop`
            // covers the DIRECT child only, which is why the spawn below also
            // makes the child a process-group leader and `GroupChild` carries
            // the teardown to the rest of that group.
            .kill_on_drop(true);
        set_process_group_leader(&mut cmd);

        let budget_expired = || {
            RpcError::internal(format!(
                "cli-query `{}`: `{}` exceeded its {} ms budget \
                 (cli_query.timeout_ms) and was killed",
                self.plugin_id,
                self.program.display(),
                self.timeout.as_millis()
            ))
        };

        // ---- Phase 1: spawn, off the async path and inside the deadline. ---
        let mut child = match spawn_within(cmd, deadline).await {
            Ok(Ok(child)) => child,
            Ok(Err(e)) => {
                return Err(RpcError::internal(format!(
                    "cli-query `{}`: spawning {} failed: {e}",
                    self.plugin_id,
                    self.program.display()
                )));
            }
            Err(SpawnTimedOut) => return Err(budget_expired()),
        };
        let mut child_stdout = child.stdout().ok_or_else(|| {
            RpcError::internal(format!("cli-query `{}`: stdout not piped", self.plugin_id))
        })?;
        let mut child_stderr = child.stderr().ok_or_else(|| {
            RpcError::internal(format!("cli-query `{}`: stderr not piped", self.plugin_id))
        })?;

        // ---- Phases 2+3: drain BOTH pipes to EOF, then reap. ----------------
        //
        // Reading before waiting is the order that cannot deadlock: it is what
        // unblocks a child filling a 64 KiB pipe buffer. Waiting FIRST is the
        // deadlock; waiting CONCURRENTLY (as this once did) avoids the deadlock
        // but lets `wait()` reap the leader while the drain is still running,
        // which is neither an ordering anyone can reason about nor one any test
        // can pin.
        //
        // Each read is capped BEFORE buffering (`read_capped`): the cap is on
        // bytes because that is what bounds memory, and a `read_to_end` that
        // truncates afterwards bounds nothing at all.
        let mut out_buf = Vec::new();
        let mut err_buf = Vec::new();
        let stdout_cap = self.max_output_bytes;
        let finished = finish_within(
            deadline,
            async {
                let (r_out, r_err) = tokio::join!(
                    read_capped(&mut child_stdout, stdout_cap, &mut out_buf),
                    read_capped(&mut child_stderr, CLI_QUERY_MAX_STDERR_BYTES, &mut err_buf),
                );
                r_out?;
                r_err?;
                Ok::<(), std::io::Error>(())
            },
            child.wait_and_release_group(),
        )
        .await;
        let (status, released_pgid) = match finished {
            Ok(value) => value,
            Err(ChildFinishError::Drain(e)) => {
                return Err(RpcError::internal(format!(
                    "cli-query `{}`: reading the child failed: {e}",
                    self.plugin_id
                )));
            }
            // Dropping `child` on the way out sweeps the group, and does so
            // BEFORE any reap — the unambiguous half of the guarantee.
            Err(ChildFinishError::TimedOut) => return Err(budget_expired()),
        };

        // `finish_within` drains before it starts the reap, preserving the
        // child's own exit status, and gives both phases this call's one
        // absolute deadline (r3 H2/H3).

        // ---- Phase 4: sweep the group. -------------------------------------
        //
        // `wait_and_release_group` disarmed `GroupChild`'s own teardown, so
        // this line is the ONLY thing that reaches the descendants — delete it
        // and a backgrounded daemon survives. That separation is the point: it
        // is what makes the step testable at all.
        //
        // What it catches: a tool that leaves work running in ITS OWN process
        // group, e.g. `( daemon --token "$LB_TOKEN" & ) >/dev/null 2>&1; echo
        // ok`, which exits 0 and would otherwise hold every `secret_env` value
        // indefinitely. What it does NOT catch: a tool that daemonizes
        // PROPERLY, with its own `fork` + `setsid`, because it has left this
        // group and `kill(-pgid)` no longer names it. That residual is real and
        // is not closed here (r3 H9).
        released_pgid.sweep();

        let status = status.map_err(|e| {
            RpcError::internal(format!(
                "cli-query `{}`: reaping the child failed: {e}",
                self.plugin_id
            ))
        })?;

        let stdout = capped_text(&out_buf, self.max_output_bytes);
        let stderr = capped_text(&err_buf, CLI_QUERY_MAX_STDERR_BYTES);
        let success = status.success();

        let mut content = vec![text_block(stdout)];
        if !success {
            // The failing exit is REPORTED, never retried and never a panic:
            // `is_error: true` plus the output the child did produce is what an
            // agent can act on.
            content.push(text_block(format!(
                "command exited with {status}{}",
                if stderr.is_empty() {
                    String::new()
                } else {
                    format!("; stderr:\n{stderr}")
                }
            )));
        } else if !stderr.is_empty() {
            content.push(text_block(format!("stderr:\n{stderr}")));
        }

        Ok(CallToolResult {
            content,
            is_error: Some(!success),
            meta: None,
            structured_content: None,
        })
    }
}

fn text_block(text: String) -> ContentBlock {
    ContentBlock {
        kind: "text".to_string(),
        text: Some(text),
        extra: serde_json::Map::new(),
    }
}

// ---------------------------------------------------------------------------
// argv templating (§2.3)
// ---------------------------------------------------------------------------

/// Render `tool.args` against the call's `arguments` object.
///
/// A `{{slot}}` element (recognised by [`argv_slot`], which matches only WHOLE
/// elements) is replaced by exactly one argv element; every other element is
/// passed literally, `--sym={{x}}` included — that partial form is refused at
/// manifest-parse time, so reaching it here would mean a template that never
/// loaded.
///
/// **v0 does not do full JSON-Schema validation.** `input_schema` is the
/// connector author's contract with the agent; the kernel enforces only what it
/// must to build a safe argv — that every slot has exactly one scalar value.
/// Keys in `arguments` that match no slot are therefore IGNORED rather than
/// rejected: refusing them would break every author who declares an optional
/// property they render elsewhere, and accepting them costs nothing because an
/// unreferenced key never reaches the child.
///
/// **Two populations, two maps, no fallback** (#1284 §2.3(b)). An
/// [`ArgvSlot::Argument`] is looked up in `arguments` and nowhere else; an
/// [`ArgvSlot::Config`] is looked up in `config` and nowhere else. Neither
/// lookup falls through to the other map on a miss — a miss is a refusal that
/// names the slot. That is what makes "an agent cannot supply a configuration
/// value" a structural property rather than an ordering convention: a
/// `tools/call` carrying `{"config.endpoint": "http://attacker"}` reaches this
/// function as a key in `arguments`, which no `Config` slot ever reads and no
/// `Argument` slot can name (the manifest validator refuses a `config.`-prefixed
/// input property).
fn render_argv(
    tool: &CliQueryTool,
    arguments: &Value,
    config: &BTreeMap<String, String>,
) -> Result<Vec<String>, String> {
    let obj = match arguments {
        Value::Object(m) => Some(m),
        // `null`/absent arguments are legal for a tool with no slots.
        Value::Null => None,
        other => {
            return Err(format!(
                "tool `{}`: `arguments` must be a JSON object, got {}",
                tool.name,
                json_type_name(other)
            ));
        }
    };

    let mut argv = Vec::with_capacity(tool.args.len());
    for raw in &tool.args {
        let Some(slot) = argv_slot(raw) else {
            argv.push(raw.clone());
            continue;
        };
        // The configuration arm resolves entirely here and never consults
        // `arguments`. Values were flattened to strings at bring-up, so there
        // is no second scalar-rendering rule to keep in sync.
        let slot = match slot {
            ArgvSlot::Config(key) => {
                let Some(value) = config.get(key) else {
                    return Err(format!(
                        "tool `{}`: configuration slot `{key}` has no value — the argv \
                         template `{raw}` is filled from this plugin's configuration \
                         (`defaults ⊕ user_config`), which currently supplies no \
                         `{key}`. Set it and restart the connector",
                        tool.name
                    ));
                };
                argv.push(value.clone());
                continue;
            }
            ArgvSlot::Argument(name) => name,
        };
        let value = obj.and_then(|m| m.get(slot));
        let rendered = match value {
            Some(Value::String(s)) => s.clone(),
            // Scalars render as their JSON form — `1`, `1.5`, `true`. Going
            // through `to_string` on the `Value` would quote the string case;
            // going through `Display` on the number keeps `1` from becoming
            // `1.0`.
            Some(Value::Number(n)) => n.to_string(),
            Some(Value::Bool(b)) => b.to_string(),
            // An empty argv element is NOT an acceptable rendering of "you
            // forgot an argument": the child would silently receive `""` where
            // it expected a symbol.
            None | Some(Value::Null) => {
                return Err(format!(
                    "tool `{}`: required argument `{slot}` is missing (the argv template \
                     `{raw}` has no value to substitute)",
                    tool.name
                ));
            }
            Some(other) => {
                return Err(format!(
                    "tool `{}`: argument `{slot}` must be a string, number or boolean; \
                     got {} (one `{{{{slot}}}}` element is exactly one argv element, so a \
                     list or an object has no rendering)",
                    tool.name,
                    json_type_name(other)
                ));
            }
        };
        argv.push(rendered);
    }
    Ok(argv)
}

/// Flatten one effective-configuration value to the single string that a child
/// process can carry — as an argv element or as an env value.
///
/// `null` is `None` ("no value"), matching `effective_config`'s reading of a
/// stored `null`. Arrays and objects have no rendering: one slot is one argv
/// element and one env value is one string, so a container would have to be
/// serialized under a convention the child never agreed to. The `config_schema`
/// subset cannot declare either type, so reaching that arm means a row edited
/// outside the API — which is why it is an error rather than a silent skip.
///
/// **An interior NUL is refused by name** (#1284 S3a review P3). JSON can carry
/// a `\u0000` inside a string and the write path stores it, but neither
/// destination can: an argv element and an env value both become a `CString`,
/// and `Command`'s conversion fails on the interior NUL. Without this check the operator's
/// diagnostic is a per-call `spawning /path/to/tool failed: nul byte found in
/// provided data` — an error that names the program and not the configuration
/// key that caused it, on the `cli-query` path at bring-up (or per call, if the
/// value only reaches an argv slot). Refusing here makes it a bring-up failure
/// that names the key. Other control characters are NOT refused: `execve` and
/// argv carry them fine, and a tab or newline inside a configured value is a
/// legitimate (if unusual) thing to want.
pub(super) fn config_scalar(key: &str, v: &Value) -> Result<Option<String>, String> {
    Ok(match v {
        Value::Null => None,
        Value::String(s) if s.contains('\0') => {
            return Err(format!(
                "configuration key `{key}` contains a NUL byte, which no argv \
                 element or environment value can carry (both become a C string \
                 at `execve` time); the connector would fail to spawn with an \
                 error naming the program rather than this key"
            ));
        }
        Value::String(s) => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        Value::Bool(b) => Some(b.to_string()),
        other => {
            return Err(format!(
                "configuration key `{key}` holds {}, which has no single-value \
                 rendering for a child process (`config_schema` can only declare \
                 string, integer, number and boolean)",
                json_type_name(other)
            ));
        }
    })
}

fn json_type_name(v: &Value) -> &'static str {
    match v {
        Value::Null => "null",
        Value::Bool(_) => "a boolean",
        Value::Number(_) => "a number",
        Value::String(_) => "a string",
        Value::Array(_) => "an array",
        Value::Object(_) => "an object",
    }
}

// ---------------------------------------------------------------------------
// Output capping
// ---------------------------------------------------------------------------

/// UTF-8-safe rendering of an already-bounded capture.
///
/// The memory bound is [`read_capped`]'s, not this function's: by the time
/// `bytes` gets here it is at most `cap + 1` long, and `len > cap` is the
/// truncation SIGNAL rather than a measurement. This is why the marker says
/// "truncated at N bytes" and not "N of M": the tail was drained without being
/// counted, so the true total is genuinely unknown here — claiming a total
/// would be a number we made up.
///
/// The result must be valid UTF-8 for a `text` content block, so the window is
/// walked back to a character boundary before `from_utf8_lossy` sees it.
/// Slicing mid-character and letting `from_utf8_lossy` paper over it would turn
/// every truncated multi-byte tail into a U+FFFD — a silent corruption at
/// exactly the boundary a reader is most likely to look at.
///
/// Truncation is always announced. A silently-clipped answer is worse than a
/// short one: an agent cannot tell "that is all the data" from "that is all you
/// were allowed to see".
fn capped_text(bytes: &[u8], cap: usize) -> String {
    if bytes.len() <= cap {
        return String::from_utf8_lossy(bytes).into_owned();
    }
    let mut end = cap;
    // `bytes[end]` is the first EXCLUDED byte; while it is a UTF-8 continuation
    // byte (0b10xxxxxx) the window ends inside a character.
    while end > 0 && (bytes[end] & 0xC0) == 0x80 {
        end -= 1;
    }
    let mut out = String::from_utf8_lossy(&bytes[..end]).into_owned();
    out.push_str(&format!(
        "\n[truncated at {end} bytes: the child produced more than the {cap}-byte cap]"
    ));
    out
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

mod bringup;
pub use bringup::bring_up;

#[cfg(test)]
mod tests;
