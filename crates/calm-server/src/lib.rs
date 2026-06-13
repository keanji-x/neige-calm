//! Calm kernel — minimal container/PTY core. Business semantics (tasks,
//! calendar, plans, git, ...) live in out-of-process plugins reached via MCP.
//!
//! Module map:
//! ```text
//! model         entity types + DTOs (Cove/Wave/Card/Overlay/Terminal/Plugin)
//! error         CalmError + Result alias + IntoResponse
//! event         Event enum + EventBus (broadcast fan-out)
//! db            Repo trait
//!   ├ mod.rs    `Repo` trait + helper free fns
//!   └ sqlite.rs SqlxRepo (production + in-memory dev/test default via
//!               `sqlite::memory:`)
//! routes        HTTP API
//!   ├ coves.rs       (track B)
//!   ├ waves.rs       (track B)
//!   ├ cards.rs       (track B)
//!   ├ overlays.rs    (track B)
//!   ├ plugins.rs     (M2 stub)
//!   └ terminal.rs    (track D, REST half)
//! ws            WebSocket endpoints
//!   ├ events.rs      (track C)
//!   └ terminal.rs    (track D, WS half)
//! plugin_host   M2 placeholder
//! state         AppState (Arc<Repo>, EventBus, DaemonClient, PluginHost)
//! config        Config (CLI / env)
//! ```

pub mod actor;
/// #322 — aspect / join-point framework: OCP-shaped invariant enforcement.
/// See [`aspect`] module docs for the closed-set / open-impl split. Lives at
/// the module-list head so reviewers see the framework boundary up top.
pub mod aspect;
pub mod auth;
pub mod card_kind;
pub mod harness;
use crate::runtime_repo::RunStatus;
use serde::Deserialize;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// #388 Phase 3b — reconcile DB rows that still look live with the
/// process supervisor's PTY registry. Production no longer respawns
/// daemon binaries at boot. If the supervisor does not know a supposedly
/// running terminal, mark the row exited with the stale-row sentinel `-1`
/// and move on.
pub async fn reconcile_supervisor_on_boot(state: &state::AppState) {
    let rows = match state.repo.terminals_running().await {
        Ok(rs) => rs,
        Err(e) => {
            tracing::warn!(
                error = %e,
                "reconcile_supervisor_on_boot: list-running query failed; skipping sweep"
            );
            return;
        }
    };
    let mut running = 0usize;
    let mut stale = 0usize;
    for term in rows {
        match probe_supervisor_for_terminal(state, &term.id).await {
            Ok(true) => running += 1,
            Ok(false) => {
                stale += 1;
                tracing::warn!(
                    terminal_id = %term.id,
                    "terminal row is running in DB but supervisor has no live PTY; marking exited",
                );
                if let Err(e) = state
                    .repo
                    .terminal_set_exit(&term.id, Some(-1), false)
                    .await
                {
                    tracing::warn!(
                        terminal_id = %term.id,
                        error = %e,
                        "failed to mark stale terminal exited during boot reconcile"
                    );
                }
                // Synthetic -1 means the process outcome is unknown at boot, so treat it as Exited.
                if let Err(e) = state
                    .repo
                    .runtime_complete_for_terminal(&term.id, RunStatus::Exited)
                    .await
                {
                    tracing::warn!(
                        terminal_id = %term.id,
                        error = %e,
                        "failed to complete stale terminal runtime during boot reconcile"
                    );
                }
            }
            Err(e) => {
                tracing::warn!(
                    terminal_id = %term.id,
                    error = %e,
                    "supervisor probe failed during boot reconcile; leaving row unchanged"
                );
            }
        }
    }
    tracing::info!(running, stale, "reconcile_supervisor_on_boot: complete",);
}

pub async fn revive_orphans_on_boot(state: &state::AppState) {
    reconcile_supervisor_on_boot(state).await;
}

pub async fn runtimes_recover_orphans_on_boot(state: &state::AppState) {
    let orphans = match state.repo.runtimes_recover_orphans_on_boot().await {
        Ok(rows) => rows,
        Err(e) => {
            tracing::warn!(
                target: "runtime_orphans::recover_on_boot",
                error = %e,
                "runtime orphan scan failed; skipping",
            );
            return;
        }
    };
    if !orphans.is_empty() {
        tracing::warn!(
            target: "runtime_orphans::recover_on_boot",
            count = orphans.len(),
            "runtime orphans detected on boot; no automatic action - see followup",
        );
        for runtime in &orphans {
            tracing::warn!(
                target: "runtime_orphans::recover_on_boot",
                runtime_id = %runtime.id,
                card_id = %runtime.card_id,
                kind = ?runtime.kind,
                status = ?runtime.status,
                "orphan runtime",
            );
        }
    }
}

pub async fn backfill_worker_sessions_from_runtimes_on_boot(state: &state::AppState) {
    match state.repo.backfill_worker_sessions_from_runtimes().await {
        Ok(count) if count > 0 => {
            tracing::info!(
                target: "worker_sessions::backfill",
                count,
                "backfilled worker_sessions mirrors from runtimes"
            );
        }
        Ok(_) => {}
        Err(e) => {
            tracing::warn!(
                target: "worker_sessions::backfill",
                error = %e,
                "worker_sessions backfill failed; continuing boot",
            );
        }
    }
}

pub async fn recover_operations_on_boot(state: &state::AppState) -> crate::error::Result<()> {
    let plan = state.operation_runtime.recover_on_boot().await?;
    for item in &plan.items {
        tracing::info!(item = ?item, "operation recovery plan item");
    }
    state.operation_runtime.apply_recovery(plan).await
}

/// Issue #644 PR-B — boot scheduler sweep (design §8). MUST run after
/// `recover_operations_on_boot`: operation recovery is synchronous
/// apply-then-drive, so by the time the sweep reads an operation row it
/// reflects the recovered state (a `dispatched` task whose worker op
/// recovery already completed reconciles immediately instead of
/// re-driving). The boot order is asserted in `boot_order_tests`.
///
/// Uses `sweep_boot` (review F7): the reconcile arms stay synchronous
/// in boot order, but pending-arm dispatching is poked onto background
/// tasks so the HTTP server start never waits on full schedule passes.
pub async fn scheduler_sweep_on_boot(state: &state::AppState) {
    state.dispatcher.scheduler().sweep_boot().await;
}

#[derive(Clone, Copy, Debug)]
enum HookReplayProvider {
    Codex,
    Claude,
}

impl HookReplayProvider {
    fn dir_name(self) -> &'static str {
        match self {
            Self::Codex => "codex",
            Self::Claude => "claude",
        }
    }

    fn endpoint(self) -> &'static str {
        match self {
            Self::Codex => "/internal/codex/hook",
            Self::Claude => "/internal/claude/hook",
        }
    }

    fn actor_header(self) -> &'static str {
        match self {
            Self::Codex => "ai:codex",
            Self::Claude => "ai:claude",
        }
    }
}

#[derive(Debug, Deserialize)]
struct HookFallbackRecord {
    card_id: String,
    body: serde_json::Value,
}

pub fn hook_fallback_dir_from_env() -> PathBuf {
    std::env::var_os("NEIGE_HOOK_FALLBACK_DIR")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/tmp/neige-hook-fallback"))
}

pub fn spawn_hook_fallback_replay(base_url: String) -> tokio::task::JoinHandle<()> {
    let root = hook_fallback_dir_from_env();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(50)).await;
        replay_hook_fallback_dir_once(&root, &base_url).await;
    })
}

pub async fn replay_hook_fallback_dir_once(root: &Path, base_url: &str) {
    for provider in [HookReplayProvider::Codex, HookReplayProvider::Claude] {
        replay_hook_fallback_provider(root, base_url, provider).await;
    }
}

async fn replay_hook_fallback_provider(root: &Path, base_url: &str, provider: HookReplayProvider) {
    let dir = root.join(provider.dir_name());
    let mut entries = match tokio::fs::read_dir(&dir).await {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return,
        Err(e) => {
            tracing::warn!(
                target: "hook.fallback.replay",
                provider = provider.dir_name(),
                dir = %dir.display(),
                error = %e,
                "hook fallback scan failed"
            );
            return;
        }
    };

    let mut paths = Vec::new();
    loop {
        let entry = match entries.next_entry().await {
            Ok(Some(entry)) => entry,
            Ok(None) => break,
            Err(e) => {
                tracing::warn!(
                    target: "hook.fallback.replay",
                    provider = provider.dir_name(),
                    dir = %dir.display(),
                    error = %e,
                    "hook fallback dir entry read failed"
                );
                return;
            }
        };
        let path = entry.path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("json") {
            continue;
        }
        paths.push(path);
    }

    paths.sort_by(|a, b| a.file_name().cmp(&b.file_name()));
    for path in paths {
        replay_hook_fallback_file(base_url, provider, &path).await;
    }
}

async fn replay_hook_fallback_file(base_url: &str, provider: HookReplayProvider, path: &Path) {
    let record = match read_hook_fallback_record(path).await {
        Ok(record) => record,
        Err(e) => {
            tracing::warn!(
                target: "hook.fallback.replay",
                provider = provider.dir_name(),
                file = %path.display(),
                error = %e,
                "hook fallback file unreadable; renaming failed"
            );
            rename_hook_fallback_failed(path).await;
            return;
        }
    };
    let body = match serde_json::to_vec(&record.body) {
        Ok(body) => body,
        Err(e) => {
            tracing::warn!(
                target: "hook.fallback.replay",
                provider = provider.dir_name(),
                file = %path.display(),
                error = %e,
                "hook fallback body serialization failed; renaming failed"
            );
            rename_hook_fallback_failed(path).await;
            return;
        }
    };

    for attempt in 1..=2 {
        match post_hook_fallback(base_url, provider, &record.card_id, &body).await {
            Ok(status) if (200..300).contains(&status) => {
                if let Err(e) = tokio::fs::remove_file(path).await {
                    tracing::warn!(
                        target: "hook.fallback.replay",
                        provider = provider.dir_name(),
                        file = %path.display(),
                        error = %e,
                        "hook fallback replay succeeded but delete failed"
                    );
                }
                return;
            }
            Ok(status) => {
                tracing::warn!(
                    target: "hook.fallback.replay",
                    provider = provider.dir_name(),
                    file = %path.display(),
                    attempt,
                    status,
                    "hook fallback replay POST failed"
                );
            }
            Err(e) => {
                tracing::warn!(
                    target: "hook.fallback.replay",
                    provider = provider.dir_name(),
                    file = %path.display(),
                    attempt,
                    error = %e,
                    "hook fallback replay POST error"
                );
            }
        }
    }
    rename_hook_fallback_failed(path).await;
}

async fn read_hook_fallback_record(path: &Path) -> anyhow::Result<HookFallbackRecord> {
    let text = tokio::fs::read_to_string(path).await?;
    Ok(serde_json::from_str(&text)?)
}

async fn rename_hook_fallback_failed(path: &Path) {
    let failed = hook_fallback_failed_path(path);
    if let Err(e) = tokio::fs::rename(path, &failed).await {
        tracing::warn!(
            target: "hook.fallback.replay",
            file = %path.display(),
            failed_file = %failed.display(),
            error = %e,
            "hook fallback failed rename failed"
        );
    }
}

fn hook_fallback_failed_path(path: &Path) -> PathBuf {
    let file_name = path
        .file_name()
        .map(|name| format!("{}.failed", name.to_string_lossy()))
        .unwrap_or_else(|| "hook.json.failed".to_string());
    path.with_file_name(file_name)
}

async fn post_hook_fallback(
    base_url: &str,
    provider: HookReplayProvider,
    card_id: &str,
    body: &[u8],
) -> std::io::Result<u16> {
    use std::io::{Error, ErrorKind};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let uri = base_url
        .parse::<axum::http::Uri>()
        .map_err(|e| Error::new(ErrorKind::InvalidInput, e.to_string()))?;
    if uri.scheme_str() != Some("http") {
        return Err(Error::new(
            ErrorKind::InvalidInput,
            "hook fallback replay only supports http base URLs",
        ));
    }
    let host = uri
        .host()
        .ok_or_else(|| Error::new(ErrorKind::InvalidInput, "base URL missing host"))?;
    let port = uri.port_u16().unwrap_or(80);
    let connect_host = match host {
        "0.0.0.0" => "127.0.0.1",
        "::" => "::1",
        other => other,
    };
    let mut stream = tokio::net::TcpStream::connect((connect_host, port)).await?;
    let path = format!("{}?card_id={}", provider.endpoint(), url_encode(card_id));
    let host_header = if uri.port().is_some() {
        format!("{host}:{port}")
    } else {
        host.to_string()
    };
    let headers = format!(
        "POST {path} HTTP/1.1\r\nHost: {host_header}\r\nContent-Type: application/json\r\nX-Calm-Actor: {actor}\r\nContent-Length: {len}\r\nConnection: close\r\n\r\n",
        actor = provider.actor_header(),
        len = body.len()
    );
    stream.write_all(headers.as_bytes()).await?;
    stream.write_all(body).await?;

    let mut response = Vec::new();
    stream.read_to_end(&mut response).await?;
    parse_http_status(&response)
}

fn parse_http_status(response: &[u8]) -> std::io::Result<u16> {
    let line_end = response
        .windows(2)
        .position(|window| window == b"\r\n")
        .unwrap_or(response.len());
    let status_line = std::str::from_utf8(&response[..line_end])
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e.to_string()))?;
    status_line
        .split_whitespace()
        .nth(1)
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidData, "missing status"))?
        .parse::<u16>()
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e.to_string()))
}

fn url_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char);
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

pub(crate) async fn probe_supervisor_for_terminal(
    state: &state::AppState,
    terminal_id: &str,
) -> anyhow::Result<bool> {
    use calm_session::control::{ControlMsg, ControlReply, ProbeRequest};
    use calm_session::{read_frame, write_frame};
    use tokio::net::UnixStream;

    let sock =
        crate::proc_supervisor::resolve_control_sock(state.daemon.proc_supervisor_sock.as_deref())
            .await
            .map_err(|e| anyhow::anyhow!("{e}"))?;
    let mut stream = UnixStream::connect(&sock)
        .await
        .map_err(|e| anyhow::anyhow!("connect proc supervisor {}: {e}", sock.display()))?;
    write_frame(
        &mut stream,
        &ControlMsg::Probe(ProbeRequest {
            proc_id: format!("term:{terminal_id}"),
        }),
    )
    .await?;
    match read_frame(&mut stream).await? {
        ControlReply::ProbeOk { proc_running, .. } => Ok(proc_running),
        ControlReply::Error { message, .. } => anyhow::bail!("{message}"),
        other => anyhow::bail!("unexpected supervisor probe reply: {other:?}"),
    }
}

pub mod card_fsm;
pub mod card_role_cache;
pub mod codex_appserver;
pub mod config;
pub mod db;
pub mod dispatcher;
pub mod error;
pub mod event;
pub mod event_cursor;
// #679 PR1 — `ids` moved wholesale to calm-types; the re-export keeps every
// `calm_server::ids::…` / `crate::ids::…` path working unchanged.
pub use calm_types::ids;
pub mod mcp_server;
pub mod model;
pub mod openapi;
pub mod operation;
pub mod pending_codex_threads;
pub(crate) mod per_card_lock;
pub mod plugin_host;
pub mod proc_identity;
pub(crate) mod proc_supervisor;
pub mod replay;
pub mod role_gate;
pub mod routes;
pub mod runtime_lookup;
pub mod runtime_repo;
pub(crate) mod runtime_row;
pub mod scheduler;
pub mod shared_codex_appserver;
pub mod shared_codex_home;
pub mod spec_appserver;
pub mod spec_card;
pub mod spec_push;
pub mod state;
pub mod terminal_renderer;
pub mod terminal_sweeper;
pub mod validation;
pub mod wave_cove_cache;
// #679 PR1 — `wave_fs_dto` moved wholesale to calm-types (pure TS DTOs).
pub use calm_types::wave_fs_dto;
pub mod wave_fs_view;
pub mod wave_lifecycle;
pub mod wave_report;
pub mod wave_report_doc;
pub mod wave_vcs;
pub mod worker_sessions_parity_sweep;
pub mod ws;

pub async fn boot_harnesses(state: &state::AppState) -> error::Result<usize> {
    let daemon_start = state.shared_codex_appserver.start_or_takeover().await;
    recover_harnesses_after_daemon_boot(state, daemon_start).await
}

pub async fn recover_harnesses_after_daemon_boot(
    state: &state::AppState,
    daemon_start: error::Result<()>,
) -> error::Result<usize> {
    match daemon_start {
        Ok(()) => state.recover_harnesses_on_boot().await,
        Err(e) => {
            tracing::error!(
                error = %e,
                "shared codex app-server start/takeover failed; continuing boot"
            );
            tracing::warn!("skipping spec harness recovery; daemon unavailable");
            Ok(0)
        }
    }
}

#[cfg(test)]
mod boot_order_tests {
    #[test]
    fn main_boot_order_harness_supervisor_runtimes_backfill_operations() {
        let main_rs = include_str!("main.rs");
        let boot_harnesses = main_rs
            .find("boot_harnesses(&state).await")
            .expect("main boot starts daemon and gates spec harness recovery");
        let reconcile = main_rs
            .find("reconcile_supervisor_on_boot(&state).await")
            .expect("main boot calls reconcile_supervisor_on_boot");
        let runtimes = main_rs
            .find("runtimes_recover_orphans_on_boot(&state).await")
            .expect("main boot calls runtimes_recover_orphans_on_boot");
        let backfill = main_rs
            .find("backfill_worker_sessions_from_runtimes_on_boot(&state).await")
            .expect("main boot calls worker_sessions backfill");
        let recover = main_rs
            .find("recover_operations_on_boot(&state).await")
            .expect("main boot calls recover_operations_on_boot");
        assert!(boot_harnesses < reconcile);
        assert!(reconcile < runtimes);
        assert!(runtimes < backfill);
        assert!(backfill < recover);
    }

    #[test]
    fn boot_order_calls_runtime_orphan_recovery_and_backfill_before_operations() {
        let main_rs = include_str!("main.rs");
        let reconcile = main_rs
            .find("reconcile_supervisor_on_boot(&state).await")
            .expect("main boot calls reconcile_supervisor_on_boot");
        let runtimes = main_rs
            .find("runtimes_recover_orphans_on_boot(&state).await")
            .expect("main boot calls runtimes_recover_orphans_on_boot");
        let backfill = main_rs
            .find("backfill_worker_sessions_from_runtimes_on_boot(&state).await")
            .expect("main boot calls worker_sessions backfill");
        let recover = main_rs
            .find("recover_operations_on_boot(&state).await")
            .expect("main boot calls recover_operations_on_boot");
        assert!(reconcile < runtimes);
        assert!(runtimes < backfill);
        assert!(backfill < recover);
    }

    /// Issue #644 PR-B — the scheduler's boot sweep runs AFTER operation
    /// recovery (design §8: harness recovery → supervisor reconcile →
    /// runtime orphans → worker-session backfill → operations → scheduler
    /// sweep). Operation
    /// recovery is synchronous apply-then-drive, so the sweep's
    /// `dispatched` arm reads already-recovered operation rows instead
    /// of racing the recovery's re-drive.
    #[test]
    fn boot_order_scheduler_sweep_after_operation_recovery() {
        let main_rs = include_str!("main.rs");
        let recover = main_rs
            .find("recover_operations_on_boot(&state).await")
            .expect("main boot calls recover_operations_on_boot");
        let sweep = main_rs
            .find("scheduler_sweep_on_boot(&state).await")
            .expect("main boot calls scheduler_sweep_on_boot");
        assert!(recover < sweep);
    }
}
