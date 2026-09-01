//! `cli-query` bring-up: resolve and pin the command, build the child
//! environment, and probe an informational fingerprint.
//!
//! Runs ONCE per enable, bounded by [`super::CLI_QUERY_BRINGUP_BUDGET`].
//! Everything it can fail on produces an operator-facing reason string.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use super::super::child_process::{
    SpawnTimedOut, read_capped, set_process_group_leader, spawn_within,
};
use super::super::connector;
use super::super::manifest::CliQueryBlock;
use super::{CliQueryRuntime, PROBE_MAX_STDOUT_BYTES, VERSION_PROBE_BUDGET};
use crate::operation::forge_action_adapter::FORGE_CREDENTIAL_ENV_KEYS;

/// Is `key` a forge credential passthrough key — i.e. one this connector may
/// never receive from the service environment (design §4 acceptance #4)?
///
/// The single source of truth is
/// [`crate::operation::forge_action_adapter::FORGE_CREDENTIAL_ENV_KEYS`], the
/// credential half of what the forge adapter forwards. Re-typing the list here
/// would drift the moment that one grows — which is precisely how the previous
/// round shipped a `#[cfg(test)]` "witness" with no mechanism behind it.
///
/// The NON-credential half (`GH_HOST`, `NO_PROXY`, `no_proxy`) is deliberately
/// absent: those grant nothing, and denying them broke a query CLI behind a
/// proxy while `HTTP_PROXY` sailed through — an incoherent policy with a real
/// cost, since every key here can retroactively invalidate an installed
/// manifest at boot (r2 G4).
fn is_forge_credential_key(key: &str) -> bool {
    FORGE_CREDENTIAL_ENV_KEYS.contains(&key)
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
    let secrets = connector::read_secrets(install_path)
        .await
        .map_err(|e| format!("secrets.json rejected: {e}"))?
        .unwrap_or_default();
    let secrets_path = install_path.join(connector::SECRETS_FILENAME);

    let env = build_child_env(
        block,
        &secrets,
        &service_env,
        &path_value,
        &secrets_path.display().to_string(),
    )?;

    // The probe runs with the BASE environment only — no `env_allow`, no
    // `secret_env`. The probe's stdout is logged verbatim as the fingerprint,
    // so a CLI that echoes its config on `--version` would otherwise put a
    // token in the log.
    //
    // `?` on purpose: a binary that resolves but cannot be EXECUTED fails the
    // enable here rather than publishing as `Running` and failing every call.
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
///
/// **Non-absolute entries are dropped, exactly as [`resolve_command`] drops
/// them** (r2 G2). Filtering only during resolution was half a fix: the kernel
/// would correctly refuse to PIN `.`, say so in the reason — and then hand the
/// child `PATH=".:…"` anyway, so a query CLI that shells out to `git` or `jq`
/// resolved it against the server's working directory, with this connector's
/// secrets already in its environment. One filter, both places, or the
/// invariant is only true of the half that is tested.
pub(super) fn per_connector_path(service_path: &str, extra: &[String]) -> String {
    let mut parts: Vec<&str> = extra.iter().map(String::as_str).collect();
    parts.extend(service_path.split(':'));
    parts.retain(|s| !s.is_empty() && Path::new(s).is_absolute());
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
/// it: a key in [`FORGE_CREDENTIAL_ENV_KEYS`] is dropped even though the
/// manifest named it. `Manifest::validate` already refuses such a manifest, so
/// nothing should reach here — this filter is the backstop for a manifest that
/// got to the runtime by any other route (a hand-edited DB blob, a future
/// caller that skips validation).
pub(super) fn build_child_env(
    block: &CliQueryBlock,
    secrets: &BTreeMap<String, String>,
    service_env: &BTreeMap<String, String>,
    path_value: &str,
    secrets_path: &str,
) -> Result<BTreeMap<String, String>, String> {
    let mut env = base_child_env(service_env, path_value);
    for key in &block.env_allow {
        if is_forge_credential_key(key) {
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

/// Probe the pinned binary: an informational fingerprint, or a REFUSAL.
///
/// Two failure modes, deliberately not the same outcome (r2 G5):
///
/// * **The spawn itself failed** — `EACCES` (the execute bit is set, but not
///   for us), `ENOEXEC` (no valid format or shebang), `ENOENT` (a dangling
///   interpreter). Resolution only checks that *some* execute bit is set, so
///   these are exactly the binaries that resolve, enable, publish as `Running`,
///   and then fail every single call. `Err` here, so the operator learns at
///   enable time with the OS error in the reason.
/// * **The binary ran and we simply learned nothing** — non-zero exit, empty
///   output, or a `--version` that hung past [`VERSION_PROBE_BUDGET`]. A CLI is
///   entitled to have no `--version`. `Ok` with the size+mtime fallback, which
///   still lets an operator tell two deploys apart.
///
/// `env` must be [`base_child_env`], **not** the child environment: this
/// probe's stdout is logged verbatim at bring-up, and a CLI that echoes its
/// configuration on `--version` would put a `secret_env` value into the log.
///
/// What that excludes is `env_allow` and `secret_env` — not every path to a
/// secret (r2 G9). `HOME` is still forwarded, because a CLI without one behaves
/// differently enough that the fingerprint would stop describing the real
/// deployment, so a tool that reads `$HOME/.config/<tool>` and echoes it on
/// `--version` can still print its own credential. That residual is the
/// R6-accepted "a connector prints its own secret"; scrubbing connector output
/// is explicitly OUT OF SCOPE, and pattern-based redaction was rejected as
/// false assurance. What changed is that the kernel no longer *hands* the probe
/// the connector's secrets itself.
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
        // Every OTHER spawn error is about the MACHINE, not the binary:
        // `EAGAIN`/`ENOMEM` under `RLIMIT_NPROC` or memory pressure,
        // `EMFILE`/`ENFILE` on descriptor exhaustion, `ETXTBSY` while an
        // upgrade rewrites the file. Bring-up runs inline at boot while every
        // other connector is spawning too, so fork pressure there is expected —
        // and refusing on it would permanently mark a perfectly good connector
        // `Unavailable` with nothing to retry it (r3 H6). Fall back and enable.
        Err(e) => {
            tracing::warn!(
                program = %program.display(),
                error = %e,
                "cli-query: --version probe could not be spawned; falling back to \
                 the size+mtime fingerprint (transient machine-level failure)"
            );
        }
    }
    // Blocking `stat(2)`, and the reason the budget above exists is that the
    // path may be a dead mount — so it does not run on a runtime worker either.
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

/// `Err` = the child could not be started at all. `Ok(None)` = it started and
/// yielded no usable version line (including by outliving the sub-budget).
///
/// Same four-phase lifecycle as `tools_call`, for the same reasons documented
/// there: spawn off the async path, drain to EOF, reap, then sweep the group.
/// Every phase shares ONE deadline, so the whole probe costs at most
/// [`VERSION_PROBE_BUDGET`] — a per-phase grace on top would push the total
/// past [`super::CLI_QUERY_BRINGUP_BUDGET`] and take the enable down, which is
/// the opposite of what the sub-budget exists for (r3 H3).
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
        // A spawn that outran the sub-budget is a HANG, not a broken binary:
        // the file is fine, something underneath it is not answering. That is
        // "we learned nothing", not a refusal — the outer bring-up budget is
        // what decides whether the enable survives.
        Err(SpawnTimedOut) => return Ok(None),
    };
    let Some(mut stdout) = child.stdout() else {
        return Ok(None);
    };

    // `.output()` buffers UNBOUNDED inside the sub-budget: a chatty `--version`
    // is the same memory amplifier `tools_call` had. One line is all this
    // reads, so the cap is small.
    let mut buf = Vec::new();
    let drained = tokio::time::timeout_at(
        deadline,
        read_capped(&mut stdout, PROBE_MAX_STDOUT_BYTES, &mut buf),
    )
    .await;

    // A `--version` that hung is the case `VERSION_PROBE_BUDGET` exists for: it
    // must cost the sub-budget and then fall back, never the whole bring-up.
    // Returning here drops `child`, which sweeps the group before any reap.
    let Ok(Ok(())) = drained else { return Ok(None) };

    let (status, released_pgid) =
        match tokio::time::timeout_at(deadline, child.wait_and_release_group()).await {
            Ok(v) => v,
            Err(_elapsed) => return Ok(None),
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

/// Is this spawn failure about the FILE (so the connector can never work), or
/// about the machine right now (so it may work on the next call)?
///
/// Only the file-shaped ones may refuse an enable. `PermissionDenied` is the
/// execute bit we cannot actually use; `NotFound` — for a path that resolution
/// just stat'd successfully — is a `#!` line naming an interpreter that is not
/// there.
///
/// `ENOEXEC` is deliberately absent, because it never reaches us: Rust's
/// `Command::spawn` goes through `execvp`, and both glibc and musl implement
/// the POSIX `ENOEXEC` retry, silently re-exec'ing the file under `/bin/sh`.
/// A shebang-less text file — and a wrong-architecture ELF — therefore SPAWNS
/// fine and merely exits non-zero, landing in the informational arm. Closing
/// that would mean treating a non-zero `--version` as fatal, which is wrong:
/// a CLI is entitled not to have `--version` at all.
pub(super) fn is_permanent_spawn_failure(e: &std::io::Error) -> bool {
    matches!(
        e.kind(),
        std::io::ErrorKind::PermissionDenied | std::io::ErrorKind::NotFound
    )
}
