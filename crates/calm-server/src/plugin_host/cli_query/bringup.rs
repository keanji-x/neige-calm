//! `cli-query` bring-up: resolve and pin the command, build the child environment, and probe an informational fingerprint. Runs ONCE per enable, bounded by [`super::CLI_QUERY_BRINGUP_BUDGET`].

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use super::super::child_process::{
    ChildFinishError, SpawnTimedOut, finish_within, read_capped, set_process_group_leader,
    spawn_within,
};
use super::super::connector;
use super::super::manifest::{ArgvSlot, CliQueryBlock, argv_slot};
use super::{CliQueryRuntime, PROBE_MAX_STDOUT_BYTES, VERSION_PROBE_BUDGET, config_scalar};
use crate::operation::forge_action_adapter::FORGE_CREDENTIAL_ENV_KEYS;
use serde_json::{Map, Value};

/// Is `key` a forge credential passthrough key — one this connector may never receive from the service environment? The non-credential half (`GH_HOST`, `NO_PROXY`) is deliberately not denied.
fn is_forge_credential_key(key: &str) -> bool {
    FORGE_CREDENTIAL_ENV_KEYS.contains(&key)
}

/// Resolve, pin, environment-build and fingerprint ONE `cli-query` connector. `Err` is an operator-facing reason string; nothing here logs or returns a secret VALUE.
/// The child environment and argv configuration slots are built ONCE per bring-up, so a configuration change reaches a `cli-query` connector only through a restart.
pub async fn bring_up(
    plugin_id: &str,
    block: &CliQueryBlock,
    install_path: &Path,
    effective: &Map<String, Value>,
) -> Result<CliQueryRuntime, String> {
    // `std::env::vars()` PANICS on a non-UTF-8 variable; `vars_os` + skip keeps the boot path on reason strings.
    let service_env: BTreeMap<String, String> = std::env::vars_os()
        .filter_map(|(k, v)| Some((k.into_string().ok()?, v.into_string().ok()?)))
        .collect();
    let service_path = service_env.get("PATH").cloned().unwrap_or_default();
    let path_value = per_connector_path(&service_path, &block.search_path_extra);

    // Resolution is a `stat(2)` per candidate directory: blocking work that would park a worker and outrun `CLI_QUERY_BRINGUP_BUDGET` on a dead mount.
    let program = {
        let command = block.command.clone();
        let extra = block.search_path_extra.clone();
        let service_path = service_path.clone();
        tokio::task::spawn_blocking(move || resolve_command(&command, &extra, &service_path))
            .await
            .map_err(|e| format!("cli_query.command resolution task failed: {e}"))??
    };

    // A wrongly-permissioned or malformed secrets file refuses the enable outright; failing open would hide a world-readable credential from the operator.
    let secrets = connector::read_secrets(install_path)
        .await
        .map_err(|e| format!("secrets.json rejected: {e}"))?
        .unwrap_or_default();
    let secrets_path = install_path.join(connector::SECRETS_FILENAME);

    // Flatten once: both the child environment and the argv slots need the same string for the same key.
    let config = flatten_config(effective)?;
    refuse_unfillable_argv_config_slots(block, &config)?;

    let env = build_child_env(
        block,
        &secrets,
        &service_env,
        &path_value,
        &secrets_path.display().to_string(),
        &config,
    )?;

    // The probe runs with the BASE environment only: its stdout is logged verbatim, so a CLI that echoes its config on `--version` would otherwise put a token in the log.
    // `?` on purpose: a binary that cannot be EXECUTED fails the enable here rather than every call.
    let fingerprint =
        probe_fingerprint(&program, &base_child_env(&service_env, &path_value)).await?;
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
        config,
        timeout: Duration::from_millis(block.timeout_ms()),
        max_output_bytes: block.max_output_bytes(),
    })
}

/// Refuse the bring-up when a declared `{{config.<key>}}` argv slot has no value in force: an argv element must render to exactly one string, so unlike an absent `config_env` key there is no representable "no value", and the connector would otherwise publish `Running` and answer every call with `invalid_params`.
fn refuse_unfillable_argv_config_slots(
    block: &CliQueryBlock,
    config: &BTreeMap<String, String>,
) -> Result<(), String> {
    for tool in &block.tools {
        for raw in &tool.args {
            let Some(ArgvSlot::Config(key)) = argv_slot(raw) else {
                continue;
            };
            if !config.contains_key(key) {
                return Err(format!(
                    "tool `{}`: the argv template `{raw}` has no value to render — \
                     `{key}` is declared by this manifest's `config_schema` but is \
                     supplied by neither a `default` nor the operator's \
                     configuration. An argv element must be exactly one string, so \
                     this connector would come up and then fail every `{}` call. \
                     Set `{key}` (or give it a `default`, or list it in \
                     `config_schema.required`) and start the plugin again",
                    tool.name, tool.name
                ));
            }
        }
    }
    Ok(())
}

/// The effective configuration, flattened to the strings a child can carry; an absent-as-`null` key simply does not appear.
pub(super) fn flatten_config(
    effective: &Map<String, Value>,
) -> Result<BTreeMap<String, String>, String> {
    let mut out = BTreeMap::new();
    for (key, value) in effective {
        if let Some(s) = config_scalar(key, value)? {
            out.insert(key.clone(), s);
        }
    }
    Ok(out)
}

/// The PATH the child gets: `search_path_extra` first, then the service PATH; the process-global `PATH` is never mutated.
/// Non-absolute entries are dropped exactly as [`resolve_command`] drops them, or the child would get `PATH=".:…"` and resolve `git`/`jq` against the server's working directory with this connector's secrets in its environment.
pub(super) fn per_connector_path(service_path: &str, extra: &[String]) -> String {
    let mut parts: Vec<&str> = extra.iter().map(String::as_str).collect();
    parts.extend(service_path.split(':'));
    parts.retain(|s| !s.is_empty() && Path::new(s).is_absolute());
    parts.join(":")
}

/// Resolve `command` to an absolute path: absolute is taken as-is (after an executability check); a bare name is searched in `search_path_extra` first, then the service PATH, absolute entries only (a relative pin would depend on the server's cwd at exec time).
/// The failure reason names every directory searched. Synchronous on purpose (`stat(2)` per candidate); run on `spawn_blocking`.
pub(super) fn resolve_command(
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

/// Build the child environment: `env_clear()` plus base `{PATH, HOME, LANG}`, `env_allow` keys present in the service env, `secret_env` keys from `secrets.json` (a missing one is a bring-up failure), `config_env` keys with a value in force, then `PATH` re-asserted last so no source can move the child off the pinned search path.
/// Forge credential keys named by `env_allow` are dropped even though `Manifest::validate` should already have refused the manifest; this filter is the backstop.
pub(super) fn build_child_env(
    block: &CliQueryBlock,
    secrets: &BTreeMap<String, String>,
    service_env: &BTreeMap<String, String>,
    path_value: &str,
    secrets_path: &str,
    config: &BTreeMap<String, String>,
) -> Result<BTreeMap<String, String>, String> {
    let mut env = base_child_env(service_env, path_value);
    for key in &block.env_allow {
        if is_forge_credential_key(key) {
            // Not a hard error: the loud refusal belongs to manifest validate; at runtime the invariant is that the key is ABSENT.
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
    // `secret_env` is deliberately NOT denylisted: its values come from this connector's own `secrets.json`, which is not an escalation from the SERVICE identity the `env_allow` denylist protects.
    for key in &block.secret_env {
        let value = secrets.get(key).ok_or_else(|| {
            // Names the key and the file, never a value.
            format!("cli_query.secret_env names `{key}`, which is absent from {secrets_path}")
        })?;
        env.insert(key.clone(), value.clone());
    }
    for key in &block.config_env {
        if let Some(value) = config.get(key) {
            env.insert(key.clone(), value.clone());
        }
    }
    env.insert("PATH".to_string(), path_value.to_string());
    Ok(env)
}

/// The base environment every `cli-query` child gets: pinned `PATH` plus `HOME`/`LANG` only when the service has them; the `--version` probe gets this and nothing else.
pub(super) fn base_child_env(
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

/// Probe the pinned binary: an informational fingerprint, or a REFUSAL. A failed spawn (`EACCES`, dangling interpreter) is `Err` so the operator learns at enable time; a binary that ran and told us nothing is `Ok` with the size+mtime fallback.
/// `env` must be [`base_child_env`], not the child environment: this probe's stdout is logged verbatim.
pub(super) async fn probe_fingerprint(
    program: &Path,
    env: &BTreeMap<String, String>,
) -> Result<String, String> {
    match run_version_probe(program, env).await {
        Ok(Some(line)) => return Ok(format!("--version: {line}")),
        // Ran, told us nothing useful — fall through to the fallback.
        Ok(None) => {}
        Err(e) if is_permanent_spawn_failure(&e) => {
            return Err(format!(
                "cli_query.command `{}` resolved as executable but could not be \
                 executed: {e}. It would enable and then fail on every call \
                 (a file can carry an execute bit we may not use, or name an \
                 interpreter that does not exist)",
                program.display()
            ));
        }
        // Every OTHER spawn error is about the MACHINE (`EAGAIN`/`ENOMEM`/`EMFILE`/`ETXTBSY`); refusing would permanently mark a good connector `Unavailable` with nothing to retry it, so fall back and enable.
        Err(e) => {
            tracing::warn!(
                program = %program.display(),
                error = %e,
                "cli-query: --version probe could not be spawned; falling back to \
                 the size+mtime fingerprint (transient machine-level failure)"
            );
        }
    }
    // Blocking `stat(2)` on a path that may be a dead mount, so not on a runtime worker.
    let owned = program.to_path_buf();
    let meta = tokio::task::spawn_blocking(move || std::fs::metadata(&owned)).await;
    Ok(match meta {
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
    })
}

/// `Err` = the child could not be started at all. `Ok(None)` = it started and yielded no usable version line. Every phase shares ONE deadline, so the whole probe costs at most [`VERSION_PROBE_BUDGET`].
async fn run_version_probe(
    program: &Path,
    env: &BTreeMap<String, String>,
) -> Result<Option<String>, std::io::Error> {
    let deadline = tokio::time::Instant::now() + VERSION_PROBE_BUDGET;

    let mut cmd = tokio::process::Command::new(program);
    cmd.arg("--version")
        .env_clear()
        .envs(env)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    set_process_group_leader(&mut cmd);

    let mut child = match spawn_within(cmd, deadline).await {
        Ok(Ok(child)) => child,
        Ok(Err(e)) => return Err(e),
        // A spawn that outran the sub-budget is a HANG, not a broken binary: "we learned nothing", not a refusal.
        Err(SpawnTimedOut) => return Ok(None),
    };
    let Some(mut stdout) = child.stdout() else {
        return Ok(None);
    };

    // `.output()` buffers UNBOUNDED; one line is all this reads, so the cap is small.
    let mut buf = Vec::new();
    let finished = finish_within(
        deadline,
        async { read_capped(&mut stdout, PROBE_MAX_STDOUT_BYTES, &mut buf).await },
        child.wait_and_release_group(),
    )
    .await;

    // A hung `--version` must cost the sub-budget and then fall back, never the whole bring-up; returning here drops `child`, which sweeps the group.
    let (status, released_pgid) = match finished {
        Ok(value) => value,
        Err(ChildFinishError::Drain(_) | ChildFinishError::TimedOut) => return Ok(None),
    };
    // The sole sweep once the leader is reaped — see `GroupChild`.
    released_pgid.sweep();
    let Ok(status) = status else { return Ok(None) };
    if !status.success() {
        return Ok(None);
    }
    let text = String::from_utf8_lossy(&buf);
    let Some(line) = text.lines().next().map(str::trim) else {
        return Ok(None);
    };
    if line.is_empty() {
        return Ok(None);
    }
    Ok(Some(line.to_string()))
}

/// Is this spawn failure about the FILE (the connector can never work) or about the machine right now? Only file-shaped ones may refuse an enable.
/// `ENOEXEC` is deliberately absent: libc's `execvp` retries under `/bin/sh`, so a shebang-less file spawns fine and merely exits non-zero.
pub(super) fn is_permanent_spawn_failure(e: &std::io::Error) -> bool {
    matches!(
        e.kind(),
        std::io::ErrorKind::PermissionDenied | std::io::ErrorKind::NotFound
    )
}
