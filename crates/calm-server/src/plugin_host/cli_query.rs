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
//!   ([`FORGE_PASSTHROUGH_ENV_KEYS`]). A query connector is authored by whoever
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

use super::child_process::{KillGroupOnDrop, read_capped, set_process_group_leader};
use super::manifest::{CliQueryBlock, CliQueryTool, argv_slot};
use super::mcp::{CallToolResult, ContentBlock, RpcError};
use crate::operation::forge_action_adapter::FORGE_PASSTHROUGH_ENV_KEYS;

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

/// Is `key` a forge credential passthrough key — i.e. one this connector may
/// never receive from the service environment (design §4 acceptance #4)?
///
/// The single source of truth is
/// [`crate::operation::forge_action_adapter::FORGE_PASSTHROUGH_ENV_KEYS`], the
/// same constant the forge adapter forwards from. Re-typing the list here would
/// drift the moment that one grows — which is precisely how the previous round
/// shipped a `#[cfg(test)]` "witness" with no mechanism behind it.
fn is_forge_passthrough_key(key: &str) -> bool {
    FORGE_PASSTHROUGH_ENV_KEYS.contains(&key)
}

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
        let argv = render_argv(tool, &arguments).map_err(RpcError::invalid_params)?;

        tracing::info!(
            plugin_id = %self.plugin_id,
            tool = %name,
            program = %self.program.display(),
            argc = argv.len(),
            "cli-query connector tools/call"
        );

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
            // makes the child a process-group leader and `KillGroupOnDrop`
            // carries the same guarantee to its descendants.
            .kill_on_drop(true);
        set_process_group_leader(&mut cmd);

        let mut child = cmd.spawn().map_err(|e| {
            RpcError::internal(format!(
                "cli-query `{}`: spawning {} failed: {e}",
                self.plugin_id,
                self.program.display()
            ))
        })?;
        // The child is a session/group leader (`setsid` in `pre_exec`), so its
        // pid is its pgid and one `kill(-pgid)` reaches every descendant.
        let mut group = KillGroupOnDrop::arm(child.id().map(|p| p as i32));
        let mut child_stdout = child.stdout.take().ok_or_else(|| {
            RpcError::internal(format!("cli-query `{}`: stdout not piped", self.plugin_id))
        })?;
        let mut child_stderr = child.stderr.take().ok_or_else(|| {
            RpcError::internal(format!("cli-query `{}`: stderr not piped", self.plugin_id))
        })?;

        let mut out_buf = Vec::new();
        let mut err_buf = Vec::new();
        // Read BOTH pipes concurrently with the wait. Waiting first and reading
        // after would deadlock on any child that fills a pipe buffer (64 KiB on
        // Linux) — which `max_output_bytes` defaults are well within, but a
        // chatty one is not.
        //
        // Each read is capped BEFORE buffering ([`read_capped`]): the cap is on
        // bytes because that is what bounds memory, and a `read_to_end` that
        // truncates afterwards bounds nothing at all.
        let stdout_cap = self.max_output_bytes;
        let outcome = tokio::time::timeout(self.timeout, async {
            let (r_out, r_err, r_status) = tokio::join!(
                read_capped(&mut child_stdout, stdout_cap, &mut out_buf),
                read_capped(&mut child_stderr, CLI_QUERY_MAX_STDERR_BYTES, &mut err_buf),
                child.wait(),
            );
            r_out?;
            r_err?;
            r_status
        })
        .await;

        let status = match outcome {
            Ok(Ok(status)) => status,
            Ok(Err(e)) => {
                // `tokio::join!` completes all three arms, so `wait()` has
                // reaped the leader here too — same recycling hazard as the
                // success path below.
                group.disarm();
                return Err(RpcError::internal(format!(
                    "cli-query `{}`: reading the child failed: {e}",
                    self.plugin_id
                )));
            }
            Err(_elapsed) => {
                // The borrow taken by the future above has ended, so the child
                // is ours again. SIGKILL the whole PROCESS GROUP, not just the
                // direct child: a wrapper script's `foo &` is a grandchild
                // holding the same secret environment, and killing only its
                // parent orphans it onto pid 1. The child has not been reaped
                // yet, so its pid — and therefore the pgid — cannot have been
                // recycled by the time the signal lands.
                group.kill_now();
                let _ = child.kill().await;
                return Err(RpcError::internal(format!(
                    "cli-query `{}`: `{}` exceeded its {} ms budget \
                     (cli_query.timeout_ms) and was killed",
                    self.plugin_id,
                    self.program.display(),
                    self.timeout.as_millis()
                )));
            }
        };

        // The child was reaped by `wait()` above, so its pid — and with it the
        // pgid — is free to be recycled; signalling the group from here on
        // could hit an unrelated process. Disarm. The group kill's job is the
        // paths where the leader is still ours: budget expiry, an I/O error,
        // and a dropped/cancelled `tools_call`.
        group.disarm();

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
// Bring-up
// ---------------------------------------------------------------------------

/// Resolve, pin, environment-build and fingerprint ONE `cli-query` connector.
///
/// `Err` is an operator-facing reason string; the caller renders it as
/// `Unavailable{reason}` (503). Nothing here logs or returns a secret VALUE.
///
/// The caller bounds this whole future with [`CLI_QUERY_BRINGUP_BUDGET`].
pub async fn bring_up(
    plugin_id: &str,
    block: &CliQueryBlock,
    install_path: &Path,
) -> Result<CliQueryRuntime, String> {
    // `std::env::vars()` PANICS on a non-UTF-8 variable. One latin-1 entry in
    // the service environment would turn every `cli-query` enable into a panic
    // on the boot path, where every other failure here is a reason string.
    // `vars_os` + skip is the only shape that keeps that promise; a key we
    // cannot represent is a key no manifest could have named anyway.
    let service_env: BTreeMap<String, String> = std::env::vars_os()
        .filter_map(|(k, v)| Some((k.into_string().ok()?, v.into_string().ok()?)))
        .collect();
    let service_path = service_env.get("PATH").cloned().unwrap_or_default();
    let path_value = per_connector_path(&service_path, &block.search_path_extra);

    // Resolution is a `stat(2)` per candidate directory — BLOCKING work. Run
    // inline it parks a runtime worker, and `tokio::time::timeout` cancels only
    // at await points, so `CLI_QUERY_BRINGUP_BUDGET` could not fire at all:
    // `"search_path_extra": ["/mnt/dead-nfs/bin"]` would hang `AppState::new`.
    // Exactly the defect `connector::read_secrets` already paid for.
    let program = {
        let command = block.command.clone();
        let extra = block.search_path_extra.clone();
        let service_path = service_path.clone();
        tokio::task::spawn_blocking(move || resolve_command(&command, &extra, &service_path))
            .await
            .map_err(|e| format!("cli_query.command resolution task failed: {e}"))??
    };

    // §2.4 — a wrongly-permissioned or malformed secrets file refuses the
    // enable outright, exactly as it does for `mcp-http`. Failing open would
    // mean an operator never learns their credential is world-readable.
    let secrets = super::connector::read_secrets(install_path)
        .await
        .map_err(|e| format!("secrets.json rejected: {e}"))?
        .unwrap_or_default();
    let secrets_path = install_path.join(super::connector::SECRETS_FILENAME);

    let env = build_child_env(
        block,
        &secrets,
        &service_env,
        &path_value,
        &secrets_path.display().to_string(),
    )?;

    // The probe runs with the BASE environment only — no `env_allow`, no
    // `secret_env`. `--version` needs no credentials, and the probe's stdout is
    // logged verbatim as the fingerprint, so a CLI that echoes its config on
    // `--version` would otherwise put a token in the log.
    let fingerprint = probe_fingerprint(&program, &base_child_env(&service_env, &path_value)).await;
    tracing::info!(
        plugin_id = %plugin_id,
        program = %program.display(),
        fingerprint = %fingerprint,
        "cli-query connector command pinned"
    );

    let mut tools = BTreeMap::new();
    for tool in &block.tools {
        tools.insert(tool.name.clone(), tool.clone());
    }

    Ok(CliQueryRuntime {
        plugin_id: plugin_id.to_string(),
        program,
        fingerprint,
        env,
        tools,
        timeout: Duration::from_millis(block.timeout_ms()),
        max_output_bytes: block.max_output_bytes(),
    })
}

/// The PATH the child gets: `search_path_extra` **first**, then the service
/// PATH.
///
/// Extras take precedence deliberately — an operator who points a connector at
/// `/opt/longbridge/bin` means "use that one", and putting them last would let
/// an unrelated same-named binary earlier in the service PATH win silently.
///
/// This value is per-connector and is only ever written into a child's
/// environment. The process-global `PATH` is never mutated: `std::env::set_var`
/// is process-wide and racy, and one connector's extras must not become every
/// other connector's (or the server's) search path.
fn per_connector_path(service_path: &str, extra: &[String]) -> String {
    let mut parts: Vec<&str> = extra
        .iter()
        .map(String::as_str)
        .filter(|s| !s.is_empty())
        .collect();
    parts.extend(service_path.split(':').filter(|s| !s.is_empty()));
    parts.join(":")
}

/// Resolve `command` to an absolute path.
///
/// An absolute path is taken as-is (after an executability check). A bare name
/// is searched in `search_path_extra` first, then the service PATH — the same
/// precedence [`per_connector_path`] gives the child, so "what got resolved"
/// and "what the child would find" cannot disagree.
///
/// **Only ABSOLUTE search entries are considered.** A `PATH` (or
/// `search_path_extra`) entry of `.` or `bin` would join to a RELATIVE
/// "pinned" program whose meaning depends on the server's working directory at
/// exec time — which is the opposite of pinning, and the same reason a relative
/// `command` is refused outright below. Skipped entries are named in the
/// failure reason so an operator is not left wondering why their extra was
/// ignored.
///
/// **The failure reason names the service PATH and every directory searched**
/// (design R5): the case this exists for is a docker preview stack that simply
/// has no such binary, where "command not found" alone tells the operator
/// nothing about where the kernel looked.
///
/// Synchronous on purpose: it is `stat(2)` per candidate, so [`bring_up`] runs
/// it on `spawn_blocking` rather than on a runtime worker.
fn resolve_command(
    command: &str,
    search_path_extra: &[String],
    service_path: &str,
) -> Result<PathBuf, String> {
    let command = command.trim();
    let path = Path::new(command);
    if path.is_absolute() {
        return if is_executable_file(path) {
            Ok(path.to_path_buf())
        } else {
            Err(format!(
                "cli_query.command `{command}` is an absolute path that is not an \
                 executable regular file"
            ))
        };
    }
    if command.contains('/') {
        return Err(format!(
            "cli_query.command `{command}` must be either an absolute path or a bare \
             name resolved against PATH; a relative path is refused because it would \
             depend on the server's working directory"
        ));
    }

    let mut searched: Vec<String> = Vec::new();
    let mut skipped: Vec<String> = Vec::new();
    for dir in search_path_extra
        .iter()
        .map(String::as_str)
        .chain(service_path.split(':'))
        .filter(|s| !s.is_empty())
    {
        if !Path::new(dir).is_absolute() {
            skipped.push(dir.to_string());
            continue;
        }
        let candidate = Path::new(dir).join(command);
        searched.push(candidate.display().to_string());
        if is_executable_file(&candidate) {
            return Ok(candidate);
        }
    }
    Err(format!(
        "cli_query.command `{command}` was not found as an executable file. \
         search_path_extra = {extra:?}; service PATH = `{service_path}`; \
         directories searched, in order: {searched:?}; \
         non-absolute search entries SKIPPED (a relative entry would pin a path \
         whose meaning depends on the server's working directory): {skipped:?}",
        extra = search_path_extra,
    ))
}

#[cfg(unix)]
fn is_executable_file(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    match std::fs::metadata(path) {
        Ok(m) => m.is_file() && m.permissions().mode() & 0o111 != 0,
        Err(_) => false,
    }
}

#[cfg(not(unix))]
fn is_executable_file(path: &Path) -> bool {
    std::fs::metadata(path)
        .map(|m| m.is_file())
        .unwrap_or(false)
}

/// Build the child environment: `env_clear()` plus exactly these keys.
///
/// Order is load-bearing at the ends, not in the middle:
///
/// 1. the base set `{PATH, HOME, LANG}` — `HOME`/`LANG` only when the service
///    has them, since inventing a `HOME` is worse than not having one;
/// 2. `env_allow` keys that exist in the service environment (an absent one is
///    simply not forwarded — the manifest is asking to pass through whatever is
///    there, not to require it);
/// 3. `secret_env` keys, valued from `secrets.json`. A named key with no
///    corresponding secret is a **bring-up failure**: silently omitting it would
///    hand the child a half-authenticated environment and turn a configuration
///    mistake into a per-call auth error nobody can trace;
/// 4. `PATH` re-asserted last, so neither `env_allow` nor `secret_env` can
///    revert the per-connector search path this connector was pinned against.
///
/// No forge credential reaches this map, and step 2 is **fail-closed** about
/// it: a key in [`FORGE_PASSTHROUGH_ENV_KEYS`] is dropped even though the
/// manifest named it. `Manifest::validate` already refuses such a manifest, so
/// nothing should reach here — this filter is the backstop for a manifest that
/// got to the runtime by any other route (a hand-edited DB blob, a future
/// caller that skips validation).
fn build_child_env(
    block: &CliQueryBlock,
    secrets: &BTreeMap<String, String>,
    service_env: &BTreeMap<String, String>,
    path_value: &str,
    secrets_path: &str,
) -> Result<BTreeMap<String, String>, String> {
    let mut env = base_child_env(service_env, path_value);
    for key in &block.env_allow {
        if is_forge_passthrough_key(key) {
            // Not a hard error here on purpose: the loud refusal belongs to
            // manifest parse/validate, which fails install and reload. At
            // runtime the invariant that matters is that the key is ABSENT.
            tracing::warn!(
                key = %key,
                "cli-query: refusing to forward a forge credential key named by env_allow \
                 (this manifest should not have loaded)"
            );
            continue;
        }
        if let Some(v) = service_env.get(key) {
            env.insert(key.clone(), v.clone());
        }
    }
    // `secret_env` is deliberately NOT denylisted. Its values come from this
    // connector's own `secrets.json`, which the operator authored for this
    // connector: naming `GH_TOKEN` there sets it to whatever that file holds,
    // which is not an escalation from the SERVICE identity — and the service
    // identity is the only thing the `env_allow` denylist protects. The
    // asymmetry is intentional, not an oversight.
    for key in &block.secret_env {
        let value = secrets.get(key).ok_or_else(|| {
            // Names the key and the file, never a value.
            format!("cli_query.secret_env names `{key}`, which is absent from {secrets_path}")
        })?;
        env.insert(key.clone(), value.clone());
    }
    env.insert("PATH".to_string(), path_value.to_string());
    Ok(env)
}

/// The base environment every `cli-query` child gets: `PATH` (the pinned
/// per-connector one) plus `HOME`/`LANG` **only when the service has them** —
/// inventing a `HOME` is worse than not having one.
///
/// Split out because the `--version` probe gets this and nothing else.
fn base_child_env(
    service_env: &BTreeMap<String, String>,
    path_value: &str,
) -> BTreeMap<String, String> {
    let mut env = BTreeMap::new();
    env.insert("PATH".to_string(), path_value.to_string());
    for key in ["HOME", "LANG"] {
        if let Some(v) = service_env.get(key) {
            env.insert(key.to_string(), v.clone());
        }
    }
    env
}

/// Informational binary fingerprint, never a bring-up failure.
///
/// First line of `<command> --version` on stdout; on any failure at all
/// (non-zero exit, no output, spawn error, budget expiry) it falls back to the
/// file's size and mtime, which is still enough for an operator to tell two
/// deploys apart.
///
/// `env` must be [`base_child_env`], **not** the child environment: this
/// probe's stdout is logged verbatim at bring-up, and a CLI that echoes its
/// configuration on `--version` would put a `secret_env` value into the log. A
/// `--version` needs no credentials, so it gets none.
///
/// For the record: scrubbing a connector's *tool output* is explicitly OUT OF
/// SCOPE. Design R6 accepted "a connector prints its own secret" as a residual
/// risk and rejected pattern-based redaction as false assurance. This change
/// removes a leak the KERNEL was creating (it chose to run the probe with
/// credentials and to log its stdout); it is not the start of an output
/// scrubber.
async fn probe_fingerprint(program: &Path, env: &BTreeMap<String, String>) -> String {
    let probe = async {
        let mut cmd = tokio::process::Command::new(program);
        cmd.arg("--version")
            .env_clear()
            .envs(env)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true);
        set_process_group_leader(&mut cmd);
        let mut child = cmd.spawn().ok()?;
        let mut group = KillGroupOnDrop::arm(child.id().map(|p| p as i32));
        let mut stdout = child.stdout.take()?;
        // `.output()` buffers UNBOUNDED inside the sub-budget: a chatty
        // `--version` is the same memory amplifier `tools_call` had. One line
        // is all this reads, so the cap is small.
        let mut buf = Vec::new();
        let (read, status) = tokio::join!(
            read_capped(&mut stdout, PROBE_MAX_STDOUT_BYTES, &mut buf),
            child.wait(),
        );
        // Both arms have completed, so the leader is reaped: disarm before the
        // pid can be recycled. A budget expiry never reaches here — the timeout
        // below drops this future, and the guard kills the group on the way out.
        group.disarm();
        read.ok()?;
        if !status.ok()?.success() {
            return None;
        }
        let text = String::from_utf8_lossy(&buf);
        let line = text.lines().next()?.trim();
        if line.is_empty() {
            return None;
        }
        Some(line.to_string())
    };
    if let Ok(Some(line)) = tokio::time::timeout(VERSION_PROBE_BUDGET, probe).await {
        return format!("--version: {line}");
    }
    // Blocking `stat(2)`, and the reason the budget above exists is that the
    // path may be a dead mount — so it does not run on a runtime worker either.
    let program = program.to_path_buf();
    let meta = tokio::task::spawn_blocking(move || std::fs::metadata(&program)).await;
    match meta {
        Ok(Ok(m)) => format!(
            "size={} mtime={:?}",
            m.len(),
            m.modified().ok().map(|t| t
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or_default())
        ),
        Ok(Err(e)) => format!("unavailable ({e})"),
        Err(e) => format!("unavailable (metadata task failed: {e})"),
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
fn render_argv(tool: &CliQueryTool, arguments: &Value) -> Result<Vec<String>, String> {
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn tool(args: &[&str]) -> CliQueryTool {
        serde_json::from_value(json!({
            "name": "quote",
            "input_schema": {
                "type": "object",
                "properties": { "symbol": { "type": "string" }, "n": { "type": "number" } },
            },
            "args": args,
        }))
        .unwrap()
    }

    fn block(v: Value) -> CliQueryBlock {
        serde_json::from_value(v).unwrap()
    }

    // ---- argv templating ----------------------------------------------

    /// A `{{slot}}` element is replaced WHOLESALE, and only when it is the
    /// whole element. `--sym={{symbol}}` stays literal — there is no string
    /// concatenation in this templater, which is what keeps a value from ever
    /// being parsed as anything but one argv element.
    #[test]
    fn a_slot_is_substituted_only_as_a_whole_argv_element() {
        let t = tool(&["quote", "{{symbol}}", "--sym={{symbol}}", "--json"]);
        let argv = render_argv(&t, &json!({ "symbol": "700.HK" })).unwrap();
        assert_eq!(
            argv,
            vec!["quote", "700.HK", "--sym={{symbol}}", "--json"],
            "only the whole-element form substitutes"
        );
    }

    /// The value lands as ONE element even when it contains shell metacharacters
    /// and whitespace — the whole reason there is no `/bin/sh` here.
    #[test]
    fn a_value_with_shell_metacharacters_is_one_literal_argv_element() {
        let t = tool(&["quote", "{{symbol}}"]);
        let argv = render_argv(&t, &json!({ "symbol": "a b; rm -rf / && echo $HOME" })).unwrap();
        assert_eq!(argv.len(), 2);
        assert_eq!(argv[1], "a b; rm -rf / && echo $HOME");
    }

    /// A missing slot is a refusal that NAMES the slot — never an empty argv
    /// element, which the child would read as a real (empty) argument.
    #[test]
    fn a_missing_slot_is_refused_by_name_not_rendered_as_an_empty_element() {
        let t = tool(&["quote", "{{symbol}}"]);
        for arguments in [json!({}), json!({ "symbol": null }), json!(null)] {
            let err = render_argv(&t, &arguments)
                .unwrap_err_or_panic("a missing slot must be refused", &arguments);
            assert!(err.contains("symbol"), "must name the slot: {err}");
        }
    }

    #[test]
    fn non_string_scalars_render_as_their_json_form() {
        let t = tool(&["{{symbol}}"]);
        for (value, expect) in [
            (json!(1), "1"),
            (json!(-3), "-3"),
            (json!(1.5), "1.5"),
            (json!(true), "true"),
            (json!(false), "false"),
        ] {
            let argv = render_argv(&t, &json!({ "symbol": value })).unwrap();
            assert_eq!(argv, vec![expect.to_string()], "for {value}");
        }
    }

    #[test]
    fn arrays_and_objects_are_refused() {
        let t = tool(&["{{symbol}}"]);
        for value in [json!([1, 2]), json!({ "a": 1 })] {
            let err = render_argv(&t, &json!({ "symbol": value.clone() }))
                .expect_err(&format!("{value} must be refused"));
            assert!(err.contains("symbol"), "{err}");
        }
        // …and a non-object `arguments` payload entirely.
        assert!(render_argv(&t, &json!("nope")).is_err());
    }

    /// v0 does not do full JSON-Schema validation; an unknown key is simply
    /// never referenced, so it cannot reach the child.
    #[test]
    fn unknown_argument_keys_are_ignored() {
        let t = tool(&["quote", "{{symbol}}"]);
        let argv = render_argv(&t, &json!({ "symbol": "X", "unused": "Y" })).unwrap();
        assert_eq!(argv, vec!["quote", "X"]);
        assert!(!argv.iter().any(|a| a == "Y"));
    }

    // ---- output capping -------------------------------------------------

    #[test]
    fn output_under_the_cap_is_untouched_and_unmarked() {
        let out = capped_text(b"hello", 32);
        assert_eq!(out, "hello");
        assert!(!out.contains("truncated"));
    }

    #[test]
    fn output_over_the_cap_is_truncated_with_an_explicit_marker() {
        let src = vec![b'x'; 100];
        let out = capped_text(&src, 40);
        assert!(out.starts_with(&"x".repeat(40)), "{out}");
        assert!(
            out.contains("[truncated at 40 bytes"),
            "the cut must be announced: {out}"
        );
        // The marker must NOT claim a total: the tail is drained uncounted, so
        // any "of M" here would be a number nobody measured.
        assert!(!out.contains("of 100"), "{out}");
    }

    /// The cap is a BYTE bound, but the result must be valid UTF-8: cutting at
    /// byte 4 of `"aa中文"` lands inside the first multi-byte character. The
    /// window backs off to the boundary instead of emitting a U+FFFD.
    #[test]
    fn truncation_never_splits_a_multi_byte_character() {
        // b"aa" + 3-byte 中 + 3-byte 文 = 8 bytes.
        let src = "aa\u{4e2d}\u{6587}".as_bytes().to_vec();
        assert_eq!(src.len(), 8);
        for cap in [2, 3, 4, 5, 6, 7] {
            let out = capped_text(&src, cap);
            assert!(
                !out.contains('\u{FFFD}'),
                "cap {cap} produced a replacement character: {out:?}"
            );
            let body = out.split("\n[truncated").next().unwrap();
            assert!(
                "aa\u{4e2d}\u{6587}".starts_with(body),
                "cap {cap}: {body:?} is not a prefix of the source"
            );
            assert!(out.contains("truncated"), "cap {cap}: {out:?}");
        }
        // The 3-byte character is only included once the whole of it fits.
        assert!(capped_text(&src, 4).starts_with("aa\n"));
        assert!(capped_text(&src, 5).starts_with("aa\u{4e2d}"));
    }

    // ---- environment ----------------------------------------------------

    /// A service environment that has EVERY forge passthrough key set — driven
    /// off the production constant, so a key added to
    /// `FORGE_PASSTHROUGH_ENV_KEYS` is automatically in the fixture instead of
    /// silently untested.
    fn service_env() -> BTreeMap<String, String> {
        let mut env: BTreeMap<String, String> = [
            ("PATH", "/usr/bin:/bin"),
            ("HOME", "/home/svc"),
            ("LANG", "C.UTF-8"),
            ("TZ", "UTC"),
            ("NOT_ALLOWED", "nope"),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
        for (i, key) in FORGE_PASSTHROUGH_ENV_KEYS.iter().enumerate() {
            // Distinct values, so a leak "under another name" is detectable.
            env.insert((*key).to_string(), format!("forge-credential-value-{i}"));
        }
        env
    }

    #[test]
    fn child_env_is_the_base_set_plus_allow_plus_secrets() {
        let b = block(json!({
            "command": "longbridge",
            "env_allow": ["TZ", "ABSENT_FROM_SERVICE"],
            "secret_env": ["LB_TOKEN"],
            "tools": [{ "name": "q", "input_schema": {}, "args": [] }],
        }));
        let secrets = [("LB_TOKEN".to_string(), "sk-lb".to_string())]
            .into_iter()
            .collect();
        let env = build_child_env(
            &b,
            &secrets,
            &service_env(),
            "/opt/lb/bin:/usr/bin:/bin",
            "s",
        )
        .unwrap();

        assert_eq!(env.get("PATH").unwrap(), "/opt/lb/bin:/usr/bin:/bin");
        assert_eq!(env.get("HOME").unwrap(), "/home/svc");
        assert_eq!(env.get("LANG").unwrap(), "C.UTF-8");
        assert_eq!(env.get("TZ").unwrap(), "UTC");
        assert_eq!(env.get("LB_TOKEN").unwrap(), "sk-lb");
        // An allowlisted key the service does not have is simply not forwarded.
        assert!(!env.contains_key("ABSENT_FROM_SERVICE"));
        // Nothing outside the enumeration.
        assert!(!env.contains_key("NOT_ALLOWED"));
        let mut keys: Vec<&str> = env.keys().map(String::as_str).collect();
        keys.sort_unstable();
        assert_eq!(keys, vec!["HOME", "LANG", "LB_TOKEN", "PATH", "TZ"]);
    }

    /// Design §4 acceptance #4 — the forge credential passthrough must never
    /// reach a `cli-query` child, **even when the service environment has all
    /// four set**. A connector is not a forge action: it is authored in a
    /// manifest and callable by any agent that can see its tools.
    ///
    /// Asserted twice on purpose: once for the plain manifest, and once for a
    /// manifest that explicitly ASKS for them via `env_allow`/`secret_env` —
    /// because "nobody requested them" is a property of the fixture, while
    /// "requesting them does not get them" is a property of the code.
    #[test]
    fn no_forge_credential_ever_reaches_the_child_env() {
        let svc = service_env();
        assert!(
            !FORGE_PASSTHROUGH_ENV_KEYS.is_empty(),
            "the denylist must not be vacuous"
        );
        for key in FORGE_PASSTHROUGH_ENV_KEYS {
            assert!(svc.contains_key(*key), "fixture must set {key}");
        }

        let plain = block(json!({
            "command": "longbridge",
            "tools": [{ "name": "q", "input_schema": {}, "args": [] }],
        }));
        // …and the manifest that ASKS for all of them. This is the arm the
        // previous round documented and never wrote: "nobody requested them" is
        // a property of the fixture; "requesting them does not get them" is a
        // property of the code.
        let greedy = block(json!({
            "command": "longbridge",
            "env_allow": FORGE_PASSTHROUGH_ENV_KEYS,
            "tools": [{ "name": "q", "input_schema": {}, "args": [] }],
        }));

        for (label, b) in [("plain", &plain), ("env_allow-requests-them", &greedy)] {
            let env = build_child_env(b, &BTreeMap::new(), &svc, "/usr/bin", "s").unwrap();
            for key in FORGE_PASSTHROUGH_ENV_KEYS {
                assert!(
                    !env.contains_key(*key),
                    "[{label}] {key} leaked into a cli-query child environment: {:?}",
                    env.keys().collect::<Vec<_>>()
                );
            }
            // …and none of the VALUES rode along under a different name.
            for value in svc
                .iter()
                .filter(|(k, _)| FORGE_PASSTHROUGH_ENV_KEYS.contains(&k.as_str()))
                .map(|(_, v)| v)
            {
                assert!(
                    !env.values().any(|v| v == value),
                    "[{label}] a forge credential value leaked under another key"
                );
            }
        }
    }

    /// One key per denylist entry, refused at MANIFEST PARSE time — the
    /// earliest and loudest place, so install/reload never produces such a
    /// connector at all. Driven off the production constant, so the quantifier
    /// covers a key added to it tomorrow.
    #[test]
    fn a_manifest_whose_env_allow_names_a_forge_key_is_refused_at_parse_time() {
        use super::super::manifest::Manifest;

        let manifest = |env_allow: Value| {
            json!({
                "manifest_version": 1,
                "kind": "cli-query",
                "id": "lb-query",
                "version": "0.1.0",
                "min_kernel_version": "0.0.1",
                "display_name": "LB",
                "cli_query": {
                    "command": "longbridge",
                    "env_allow": env_allow,
                    "tools": [{ "name": "q", "input_schema": {}, "args": [] }],
                }
            })
            .to_string()
        };

        // The control: a benign allowlist LOADS, so the refusals below are not
        // "cli-query manifests never parse".
        Manifest::parse(&manifest(json!(["TZ"])))
            .expect("a benign env_allow must still load; otherwise this test proves nothing");

        for key in FORGE_PASSTHROUGH_ENV_KEYS {
            let err = Manifest::parse(&manifest(json!(["TZ", key])))
                .err()
                .unwrap_or_else(|| panic!("env_allow naming {key} must be refused"));
            let msg = err.to_string();
            assert!(msg.contains(key), "the refusal must name the key: {msg}");
            assert!(
                msg.contains("env_allow"),
                "the refusal must name the field: {msg}"
            );
        }
    }

    /// `secret_env` is deliberately NOT denylisted: those values come from the
    /// connector's own `secrets.json`, which the operator authored, so there is
    /// no escalation from the SERVICE identity. Locked down so the asymmetry
    /// cannot be "fixed" by accident.
    #[test]
    fn secret_env_may_name_a_forge_key_and_gets_the_operators_own_value() {
        let b = block(json!({
            "command": "longbridge",
            "secret_env": ["GH_TOKEN"],
            "tools": [{ "name": "q", "input_schema": {}, "args": [] }],
        }));
        let secrets = [("GH_TOKEN".to_string(), "operator-authored".to_string())]
            .into_iter()
            .collect();
        let svc = service_env();
        let env = build_child_env(&b, &secrets, &svc, "/usr/bin", "s").unwrap();
        assert_eq!(env.get("GH_TOKEN").unwrap(), "operator-authored");
        assert_ne!(
            env.get("GH_TOKEN").unwrap(),
            svc.get("GH_TOKEN").unwrap(),
            "the SERVICE value must never be the one that lands"
        );
    }

    /// A `secret_env` key with no secret behind it fails bring-up loudly, and
    /// the message names both the key and the file — an env var that is simply
    /// absent turns into a per-call auth failure nobody can trace back here.
    #[test]
    fn a_secret_env_key_with_no_secret_is_a_bring_up_failure() {
        let b = block(json!({
            "command": "longbridge",
            "secret_env": ["LB_TOKEN"],
            "tools": [{ "name": "q", "input_schema": {}, "args": [] }],
        }));
        let err = build_child_env(
            &b,
            &BTreeMap::new(),
            &service_env(),
            "/usr/bin",
            "/plugins/lb/secrets.json",
        )
        .unwrap_err();
        assert!(err.contains("LB_TOKEN"), "{err}");
        assert!(err.contains("/plugins/lb/secrets.json"), "{err}");
    }

    /// Neither `env_allow` nor `secret_env` may revert the per-connector PATH
    /// the command was pinned against.
    #[test]
    fn path_cannot_be_overridden_by_allow_or_secrets() {
        let b = block(json!({
            "command": "longbridge",
            "env_allow": ["PATH"],
            "secret_env": ["PATH"],
            "tools": [{ "name": "q", "input_schema": {}, "args": [] }],
        }));
        let secrets = [("PATH".to_string(), "/evil".to_string())]
            .into_iter()
            .collect();
        let env = build_child_env(&b, &secrets, &service_env(), "/opt/lb/bin", "s").unwrap();
        assert_eq!(env.get("PATH").unwrap(), "/opt/lb/bin");
    }

    // ---- PATH resolution ------------------------------------------------

    #[test]
    fn extras_are_searched_before_the_service_path() {
        let svc = per_connector_path("/usr/bin:/bin", &["/opt/lb/bin".to_string()]);
        assert_eq!(svc, "/opt/lb/bin:/usr/bin:/bin");
    }

    /// Design R5 — a docker preview stack with no such binary must be able to
    /// see WHY from the reason alone.
    #[test]
    fn an_unresolvable_bare_command_names_the_path_and_every_directory_searched() {
        let service_path = "/usr/bin:/bin";
        let err = resolve_command(
            "definitely-not-a-real-binary-1164",
            &["/opt/lb/bin".to_string()],
            service_path,
        )
        .unwrap_err();
        assert!(
            err.contains(service_path),
            "the reason must carry the service PATH: {err}"
        );
        for dir in [
            "/opt/lb/bin/definitely-not-a-real-binary-1164",
            "/usr/bin/definitely-not-a-real-binary-1164",
            "/bin/definitely-not-a-real-binary-1164",
        ] {
            assert!(err.contains(dir), "must list {dir}: {err}");
        }
    }

    #[cfg(unix)]
    #[test]
    fn a_bare_name_resolves_to_an_absolute_path_in_the_extras_first() {
        use std::os::unix::fs::PermissionsExt;
        let tmp_lo = tempfile::tempdir().unwrap();
        let tmp_hi = tempfile::tempdir().unwrap();
        for dir in [tmp_lo.path(), tmp_hi.path()] {
            let p = dir.join("mytool");
            std::fs::write(&p, "#!/bin/sh\n").unwrap();
            std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let resolved = resolve_command(
            "mytool",
            &[tmp_hi.path().display().to_string()],
            &tmp_lo.path().display().to_string(),
        )
        .unwrap();
        assert_eq!(resolved, tmp_hi.path().join("mytool"));
        assert!(resolved.is_absolute());
    }

    #[cfg(unix)]
    #[test]
    fn a_non_executable_file_is_not_a_resolution() {
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("mytool");
        std::fs::write(&p, "not executable").unwrap();
        assert!(resolve_command("mytool", &[], &tmp.path().display().to_string()).is_err());
        assert!(resolve_command(&p.display().to_string(), &[], "").is_err());
    }

    #[test]
    fn a_relative_path_command_is_refused() {
        let err = resolve_command("./bin/tool", &[], "/usr/bin").unwrap_err();
        assert!(err.contains("absolute"), "{err}");
    }

    /// #1164 P3 F6 — a "pinned" path must be absolute, which the SEARCH ENTRIES
    /// decide as much as the command does. A `PATH` of `.` or `bin` would yield
    /// a relative program whose meaning depends on the cwd at exec time.
    ///
    /// Driven with a BARE command name on purpose: every other resolution
    /// fixture passes an absolute path, which returns before this code runs.
    #[cfg(unix)]
    #[test]
    fn a_non_absolute_search_entry_is_skipped_and_the_reason_says_so() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        let sub = tmp.path().join("bin");
        std::fs::create_dir_all(&sub).unwrap();
        let p = sub.join("mytool");
        std::fs::write(&p, "#!/bin/sh\n").unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();

        // The only entries that could resolve `mytool` are relative ones, and
        // they are relative to a cwd we deliberately do not control.
        let err =
            resolve_command("mytool", &[".".to_string(), "bin".to_string()], "bin:.").unwrap_err();
        // The load-bearing half: no relative candidate was ever STAT'd, so no
        // relative candidate could ever have been RETURNED. Asserting only on
        // the word "SKIPPED" passes with the skip deleted — that literal is in
        // the format string unconditionally.
        for relative in ["\"./mytool\"", "\"bin/mytool\""] {
            assert!(
                !err.contains(relative),
                "a relative candidate was searched: {err}"
            );
        }
        // …and the operator is told which entries were dropped, and why.
        for entry in ["\".\"", "\"bin\""] {
            assert!(
                err.contains(entry),
                "the reason must name the skipped entry {entry}: {err}"
            );
        }
        assert!(
            err.contains("working directory"),
            "the reason must say WHY: {err}"
        );

        // …and the same name DOES resolve once the entry is absolute, so the
        // skip is about absoluteness and not about the fixture being broken.
        let ok = resolve_command("mytool", &[sub.display().to_string()], "").unwrap();
        assert_eq!(ok, p);
        assert!(ok.is_absolute());
    }

    // ---- execution ------------------------------------------------------

    #[cfg(unix)]
    fn script(dir: &Path, name: &str, body: &str) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let p = dir.join(name);
        std::fs::write(&p, body).unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
        p
    }

    #[cfg(unix)]
    fn runtime_for(
        program: PathBuf,
        args: &[&str],
        timeout_ms: u64,
        cap: usize,
    ) -> CliQueryRuntime {
        let mut tools = BTreeMap::new();
        tools.insert("quote".to_string(), tool(args));
        CliQueryRuntime {
            plugin_id: "cli-test".to_string(),
            program,
            fingerprint: "test".to_string(),
            env: BTreeMap::new(),
            tools,
            timeout: Duration::from_millis(timeout_ms),
            max_output_bytes: cap,
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_zero_exit_returns_stdout_and_is_error_false() {
        let tmp = tempfile::tempdir().unwrap();
        let p = script(tmp.path(), "ok.sh", "#!/bin/sh\necho \"got:$1\"\n");
        let rt = runtime_for(p, &["{{symbol}}"], 5_000, 4096);
        let res = rt
            .tools_call("quote", json!({ "symbol": "700.HK" }))
            .await
            .unwrap();
        assert_eq!(res.is_error, Some(false));
        assert_eq!(
            res.content[0].text.as_deref(),
            Some("got:700.HK\n"),
            "{:?}",
            res.content
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_non_zero_exit_is_is_error_true_carrying_the_output() {
        let tmp = tempfile::tempdir().unwrap();
        let p = script(
            tmp.path(),
            "bad.sh",
            "#!/bin/sh\necho partial\necho boom >&2\nexit 3\n",
        );
        let rt = runtime_for(p, &[], 5_000, 4096);
        let res = rt.tools_call("quote", json!({})).await.unwrap();
        assert_eq!(res.is_error, Some(true));
        assert_eq!(res.content[0].text.as_deref(), Some("partial\n"));
        let detail = res.content[1].text.clone().unwrap();
        assert!(detail.contains("exit"), "{detail}");
        assert!(detail.contains("boom"), "stderr must be carried: {detail}");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn stdout_over_the_cap_is_truncated_with_the_marker() {
        let tmp = tempfile::tempdir().unwrap();
        let p = script(
            tmp.path(),
            "big.sh",
            "#!/bin/sh\nfor i in 1 2 3 4 5 6 7 8 9 0; do printf 'aaaaaaaaaa'; done\n",
        );
        let rt = runtime_for(p, &[], 5_000, 16);
        let res = rt.tools_call("quote", json!({})).await.unwrap();
        let text = res.content[0].text.clone().unwrap();
        assert!(
            text.contains("[truncated at 16 bytes"),
            "cap must be enforced and announced: {text:?}"
        );
        assert!(text.starts_with(&"a".repeat(16)));
    }

    /// A child whose output dwarfs both the cap AND the 64 KiB pipe buffer must
    /// still return a TRUNCATED ANSWER, not a budget-expiry error: the tail is
    /// drained (and discarded) so the child is never blocked on a full pipe.
    ///
    /// Mutation witness: delete the drain loop in `read_capped` and this goes
    /// red with "exceeded its … budget" instead of a result.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_child_far_over_the_pipe_buffer_still_answers_truncated() {
        let tmp = tempfile::tempdir().unwrap();
        // 2 MiB, far past both the 64-byte cap and the 64 KiB pipe buffer.
        let p = script(
            tmp.path(),
            "flood.sh",
            "#!/bin/sh\nyes aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa | head -c 2097152\n",
        );
        let rt = runtime_for(p, &[], 20_000, 64);
        let res = rt.tools_call("quote", json!({})).await.unwrap();
        assert_eq!(res.is_error, Some(false));
        let text = res.content[0].text.clone().unwrap();
        assert!(text.contains("[truncated at 64 bytes"), "{text:?}");
        // The whole answer is the cap plus one short marker line — nothing
        // close to the 2 MiB the child wrote.
        assert!(text.len() < 512, "materialised {} bytes", text.len());
    }

    /// #1164 P3 F4 — the budget kill must reach the child's DESCENDANTS. A
    /// wrapper that backgrounds work is the normal shape of a query CLI, and
    /// `Child::kill`/`kill_on_drop` reach only the direct child: the reviewer
    /// observed `sleep 30` still alive with PPID 1 after this call returned.
    ///
    /// The assertion is on the grandchild's pid, not on the call's error: the
    /// old test passed with the kill deleted entirely.
    #[cfg(unix)]
    #[tokio::test]
    async fn the_budget_kill_reaches_the_childs_descendants() {
        let tmp = tempfile::tempdir().unwrap();
        let pidfile = tmp.path().join("grandchild.pid");
        let p = script(
            tmp.path(),
            "wrapper.sh",
            "#!/bin/sh\nsleep 30 &\necho $! > \"$1\"\nsleep 30\n",
        );
        let rt = runtime_for(p, &["{{symbol}}"], 300, 4096);
        let err = rt
            .tools_call("quote", json!({ "symbol": pidfile.display().to_string() }))
            .await
            .unwrap_err();
        assert!(err.message.contains("budget"), "{}", err.message);

        let pid: i32 = std::fs::read_to_string(&pidfile)
            .expect("the wrapper must have recorded its background child")
            .trim()
            .parse()
            .unwrap();
        // SAFETY: `kill(pid, 0)` only probes for existence; it delivers no
        // signal and touches no memory.
        let alive = |pid: i32| unsafe { libc::kill(pid, 0) } == 0;
        assert!(pid > 1, "implausible grandchild pid {pid}");

        // The SIGKILL is asynchronous; give the kernel a moment, then insist.
        for _ in 0..50 {
            if !alive(pid) {
                return;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        // Do not leave a 30 s sleep behind if the assertion is about to fail.
        unsafe { libc::kill(pid, libc::SIGKILL) };
        panic!("grandchild {pid} survived the budget kill (it was orphaned onto pid 1)");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_child_that_outlives_its_budget_is_killed_and_named() {
        let tmp = tempfile::tempdir().unwrap();
        let p = script(tmp.path(), "slow.sh", "#!/bin/sh\nsleep 30\n");
        let rt = runtime_for(p, &[], 200, 4096);
        let started = std::time::Instant::now();
        let err = rt.tools_call("quote", json!({})).await.unwrap_err();
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "the budget must actually fire"
        );
        assert!(err.message.contains("200 ms"), "{}", err.message);
        assert!(err.message.contains("budget"), "{}", err.message);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn an_unknown_tool_name_is_refused_before_any_exec() {
        let tmp = tempfile::tempdir().unwrap();
        let p = script(tmp.path(), "ok.sh", "#!/bin/sh\necho hi\n");
        let rt = runtime_for(p, &[], 5_000, 4096);
        let err = rt.tools_call("nope", json!({})).await.unwrap_err();
        assert_eq!(err.code, -32601, "{err:?}");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn bring_up_pins_an_absolute_path_and_records_a_fingerprint() {
        let tmp = tempfile::tempdir().unwrap();
        let p = script(
            tmp.path(),
            "vers.sh",
            "#!/bin/sh\necho 'mytool 1.2.3'\necho 'second line'\n",
        );
        let b = block(json!({
            "command": p.display().to_string(),
            "tools": [{ "name": "q", "input_schema": {}, "args": [] }],
        }));
        let rt = bring_up("cli-test", &b, tmp.path()).await.unwrap();
        assert_eq!(rt.program(), p.as_path());
        assert_eq!(rt.fingerprint(), "--version: mytool 1.2.3");
    }

    /// #1164 P3 F5 — the fingerprint probe runs with the BASE environment only.
    /// Its stdout is logged verbatim, so a CLI that echoes its configuration on
    /// `--version` would otherwise put a `secret_env` value in the log.
    ///
    /// Mutation witness: pass `&env` instead of `&base_child_env(..)` in
    /// `bring_up` and the fingerprint becomes `--version: v1 token=sk-secret`.
    #[cfg(unix)]
    #[tokio::test]
    async fn the_version_probe_never_sees_a_secret_env_value() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        let p = script(
            tmp.path(),
            "echoenv.sh",
            "#!/bin/sh\necho \"v1 token=[$LB_TOKEN]\"\n",
        );
        let secrets = tmp.path().join(super::super::connector::SECRETS_FILENAME);
        std::fs::write(&secrets, r#"{"LB_TOKEN":"sk-secret-value"}"#).unwrap();
        std::fs::set_permissions(&secrets, std::fs::Permissions::from_mode(0o600)).unwrap();

        let b = block(json!({
            "command": p.display().to_string(),
            "secret_env": ["LB_TOKEN"],
            "tools": [{ "name": "q", "input_schema": {}, "args": [] }],
        }));
        let rt = bring_up("cli-test", &b, tmp.path()).await.unwrap();
        assert_eq!(
            rt.fingerprint(),
            "--version: v1 token=[]",
            "the probe must run with the base environment only"
        );
        // …while the CALL environment still has it: the probe is restricted,
        // the connector is not broken.
        assert!(rt.env_keys().contains(&"LB_TOKEN"));
    }

    /// A `--version` that fails must NOT fail bring-up — the fingerprint is
    /// informational.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_failing_version_probe_falls_back_instead_of_failing_bring_up() {
        let tmp = tempfile::tempdir().unwrap();
        let p = script(tmp.path(), "novers.sh", "#!/bin/sh\nexit 1\n");
        let b = block(json!({
            "command": p.display().to_string(),
            "tools": [{ "name": "q", "input_schema": {}, "args": [] }],
        }));
        let rt = bring_up("cli-test", &b, tmp.path()).await.unwrap();
        assert!(
            rt.fingerprint().starts_with("size="),
            "expected the size+mtime fallback, got {}",
            rt.fingerprint()
        );
    }

    /// Small helper so the missing-slot loop can report WHICH input silently
    /// succeeded rather than panicking with no context.
    trait UnwrapErrOrPanic {
        fn unwrap_err_or_panic(self, ctx: &str, input: &Value) -> String;
    }
    impl UnwrapErrOrPanic for Result<Vec<String>, String> {
        fn unwrap_err_or_panic(self, ctx: &str, input: &Value) -> String {
            match self {
                Ok(argv) => panic!("{ctx}: {input} rendered {argv:?}"),
                Err(e) => e,
            }
        }
    }
}
