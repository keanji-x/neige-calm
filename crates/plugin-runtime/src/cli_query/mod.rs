//! The `kind: cli-query` connector execution runtime: a read-only local query CLI, pinned to one absolute binary at enable time and exec'd directly per `tools/call` with a fixed argv template.
//! No shell, no forge-action adapter, no forge credential passthrough: `env_clear()` plus an explicit enumerated environment, and a `{{slot}}` occupies a WHOLE argv element replaced by exactly one argument.

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

/// Wall-clock bound on ONE `cli-query` bring-up: `AppState::new` awaits the autospawn path inline, so an unbounded bring-up is a boot stall.
/// Not operator-configurable: `cli_query.timeout_ms` is the steady-state budget and may legitimately be long.
pub const CLI_QUERY_BRINGUP_BUDGET: Duration = Duration::from_secs(5);

/// Sub-budget for the `--version` probe, strictly smaller than [`CLI_QUERY_BRINGUP_BUDGET`] so a hung probe fails as "fingerprint unavailable" rather than taking the enable down.
const VERSION_PROBE_BUDGET: Duration = Duration::from_secs(2);

/// stderr capture cap: stderr is diagnostics, so a fixed small window keeps a chatty binary from being a memory amplifier.
pub const CLI_QUERY_MAX_STDERR_BYTES: usize = 4 * 1024;

/// stdout cap for the `--version` probe; only the FIRST LINE is ever used.
const PROBE_MAX_STDOUT_BYTES: usize = 4 * 1024;

/// One enabled `cli-query` connector; every field is resolved once at bring-up so a `tools/call` does no PATH lookup, no secret read and no manifest walking.
pub struct CliQueryRuntime {
    plugin_id: String,
    /// The pinned **absolute** program path; there is no second PATH resolution at call time.
    program: PathBuf,
    /// Informational only; never a bring-up failure.
    fingerprint: String,
    /// The complete child environment, including secret values; this is why [`super::ConnectorClient`]'s `Debug` prints no payload.
    env: BTreeMap<String, String>,
    tools: BTreeMap<String, CliQueryTool>,
    /// The rendered `{{config.<key>}}` slot values, resolved ONCE at bring-up. A separate map from the agent's `arguments` by construction: `tools_call` never merges the two or falls back from one to the other.
    config: BTreeMap<String, String>,
    timeout: Duration,
    max_output_bytes: usize,
}

impl CliQueryRuntime {
    pub fn program(&self) -> &Path {
        &self.program
    }

    /// `<command> --version`'s first line, or a `size=…, mtime=…` fallback.
    pub fn fingerprint(&self) -> &str {
        &self.fingerprint
    }

    /// Child environment KEYS only; the values include secrets.
    pub fn env_keys(&self) -> Vec<&str> {
        self.env.keys().map(String::as_str).collect()
    }

    /// The stdout cap the runtime will actually enforce, so a test can pin that bring-up used `CliQueryBlock`'s clamping getter rather than the raw field.
    pub fn max_output_bytes(&self) -> usize {
        self.max_output_bytes
    }

    pub fn child_path(&self) -> &str {
        self.env.get("PATH").map(String::as_str).unwrap_or_default()
    }

    /// Run one declared tool. `Ok(CallToolResult)` whose `is_error` reports the CHILD's verdict; `Err(RpcError)` only for things that are not a child verdict (unknown tool, malformed arguments, spawn failure, budget expiry).
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

        // ONE deadline for the whole call, spawn included: `fork`+`execve` against a wedged mount can block indefinitely, and a bound that starts only after the child exists is not the one `cli_query.timeout_ms` advertises.
        let deadline = tokio::time::Instant::now() + self.timeout;

        let mut cmd = tokio::process::Command::new(&self.program);
        cmd.args(&argv)
            // `env_clear` FIRST, then only what `build_child_env` enumerated.
            .env_clear()
            .envs(&self.env)
            // A query CLI has no input; an inherited stdin would let a prompting binary block on the server's own stdin.
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            // `kill_on_drop` covers the DIRECT child only; the process-group leader + `GroupChild` carry the teardown to the rest of the group.
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

        // Drain BOTH pipes to EOF, then reap: reading before waiting is the order that cannot deadlock on a full pipe buffer, and each read is capped BEFORE buffering.
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
            // Dropping `child` on the way out sweeps the group BEFORE any reap.
            Err(ChildFinishError::TimedOut) => return Err(budget_expired()),
        };

        // `wait_and_release_group` disarmed `GroupChild`'s own teardown, so this line is the ONLY thing that reaches the descendants; a tool that daemonizes properly (own `fork` + `setsid`) has left the group and survives.
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
            // The failing exit is REPORTED, never retried and never a panic.
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

/// Render `tool.args` against the call's `arguments` object. Keys in `arguments` that match no slot are IGNORED, and only "every slot has exactly one scalar value" is enforced, not the full `input_schema`.
/// Two populations, two maps, no fallback: an [`ArgvSlot::Argument`] is looked up in `arguments` only, an [`ArgvSlot::Config`] in `config` only, so an agent cannot supply a configuration value.
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
        // The configuration arm never consults `arguments`; values were flattened to strings at bring-up.
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
            // Scalars render as their JSON form; `Display` on the number keeps `1` from becoming `1.0`.
            Some(Value::Number(n)) => n.to_string(),
            Some(Value::Bool(b)) => b.to_string(),
            // An empty argv element is NOT an acceptable rendering of a missing argument.
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

/// Flatten one effective-configuration value to the single string a child can carry. `null` is `None`; arrays and objects are an error (only reachable via a row edited outside the API).
/// An interior NUL is refused by name: both destinations become a `CString`, and the per-call `Command` error would name the program, not the key. Other control characters are fine.
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

/// UTF-8-safe rendering of an already-bounded capture. `len > cap` is the truncation SIGNAL, not a measurement: the tail was drained uncounted, so the marker says "truncated at N bytes", never "N of M".
/// The window is walked back to a character boundary so a truncated multi-byte tail does not become a U+FFFD; truncation is always announced.
fn capped_text(bytes: &[u8], cap: usize) -> String {
    if bytes.len() <= cap {
        return String::from_utf8_lossy(bytes).into_owned();
    }
    let mut end = cap;
    // `bytes[end]` is the first EXCLUDED byte; while it is a UTF-8 continuation byte (0b10xxxxxx) the window ends inside a character.
    while end > 0 && (bytes[end] & 0xC0) == 0x80 {
        end -= 1;
    }
    let mut out = String::from_utf8_lossy(&bytes[..end]).into_owned();
    out.push_str(&format!(
        "\n[truncated at {end} bytes: the child produced more than the {cap}-byte cap]"
    ));
    out
}

mod bringup;
pub use bringup::bring_up;

#[cfg(test)]
mod tests;
