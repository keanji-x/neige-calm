use crate::config::Config;
use crate::shared_codex_home::SharedCodexHome;
use crate::terminal_renderer::TerminalRendererRegistry;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

// ---------------------------------------------------------------------------
// DaemonClient — terminal support paths shared by renderer-backed flows.
// ---------------------------------------------------------------------------

/// Lightweight handle the REST + WS halves both consult. It owns the
/// per-terminal data paths and the optional proc-supervisor socket used by
/// renderer-backed sessions.
pub struct DaemonClient {
    /// Per-terminal sockets live under this directory as `<terminal_id>.sock`.
    /// Created on first use by `routes::terminal::create`. Defaults to
    /// `<config.data_dir>/terminals`.
    pub data_dir: PathBuf,
    /// Control socket for `calm-proc-supervisor`. Production config resolves
    /// this to `<CALM_DATA_DIR>/proc-supervisor.sock`; fixture tests may leave
    /// it unset to use an in-process framed supervisor.
    pub proc_supervisor_sock: Option<PathBuf>,
}

impl DaemonClient {
    /// Real constructor. Pulls terminal data paths from the resolved config.
    pub fn new(cfg: &Config) -> Self {
        let data_dir = cfg.data_dir_resolved().join("terminals");
        Self {
            data_dir,
            proc_supervisor_sock: Some(cfg.proc_supervisor_sock_resolved()),
        }
    }

    /// Placeholder for tests / dev paths that don't have a full `Config`.
    /// Sockets land in a per-uid tempdir.
    pub fn new_stub() -> Self {
        let tmp = std::env::var_os("XDG_RUNTIME_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("/tmp"))
            .join("calm-terminals");
        Self {
            data_dir: tmp,
            proc_supervisor_sock: None,
        }
    }

    /// Socket path for a given terminal id.
    pub fn sock_path(&self, terminal_id: &str) -> PathBuf {
        self.data_dir.join(format!("{terminal_id}.sock"))
    }

    /// PR3a (#293) — per-card directory for a spec card's `codex
    /// app-server` listen socket: `<data_dir>/appserver/<card_id>/`.
    ///
    /// **Must be user-owned**, NOT a bare sticky `/tmp` directory: the
    /// `codex app-server` `chmod 0700`s the socket's *parent* dir at boot
    /// and EPERMs if it can't (spike caveat #2). We hang it off the daemon
    /// data dir's parent (`self.data_dir` is `<data_dir>/terminals`, so
    /// `parent()` is the resolved `data_dir`, which is the user-owned
    /// `$HOME/.local/share/neige-calm` in production and a per-test
    /// tempdir under test). The 0700 chmod lands on this per-card subdir,
    /// **never** the shared `data_dir` itself. Falls back to `self.data_dir`
    /// only in the degenerate case where it has no parent.
    pub fn appserver_sock_dir(&self, card_id: &str) -> PathBuf {
        let base = self.data_dir.parent().unwrap_or(&self.data_dir);
        base.join("appserver").join(card_id)
    }

    /// PR3a (#293) — the `app.sock` path inside [`appserver_sock_dir`].
    /// Passed to `codex app-server --listen unix://<path>` (kernel side)
    /// and `codex resume <tid> --remote unix://<path>` (TUI side).
    pub fn appserver_sock_path(&self, card_id: &str) -> PathBuf {
        self.appserver_sock_dir(card_id).join("app.sock")
    }

    /// Kernel-private transient stdin injection. Routes directly through
    /// the in-process renderer's supervisor writer and waits for the
    /// matching InputAck generated from the supervisor WriteAck.
    pub async fn inject_stdin_renderer(
        &self,
        renderer: &TerminalRendererRegistry,
        terminal_id: &str,
        bytes: &[u8],
        timeout: Duration,
    ) -> anyhow::Result<()> {
        tokio::time::timeout(timeout, async move {
            let entry = renderer
                .get(terminal_id)
                .ok_or_else(|| anyhow::anyhow!("no live renderer for terminal {terminal_id}"))?;
            let (ack_tx, mut ack_rx) = tokio::sync::mpsc::unbounded_channel();
            entry
                .handle
                .supervisor_tx
                .send(crate::terminal_renderer::SupervisorControl::Write(
                    crate::terminal_renderer::PtyWrite {
                        data: bytes.to_vec(),
                        input_seq: 1,
                        ack: Some(ack_tx),
                    },
                ))
                .map_err(|_| anyhow::anyhow!("renderer supervisor writer is closed"))?;
            match ack_rx.recv().await {
                Some(calm_session::DaemonMsg::InputAck { input_seq: 1 }) => Ok(()),
                Some(other) => Err(anyhow::anyhow!(
                    "expected InputAck(1) from renderer, got {other:?}"
                )),
                None => Err(anyhow::anyhow!("renderer ack channel closed")),
            }
        })
        .await
        .map_err(|_| anyhow::anyhow!("inject_stdin to {terminal_id} timed out after {timeout:?}"))?
    }
}

// ---------------------------------------------------------------------------
// CodexClient — owned by Track Codex.
//
// Carries the codex CLI path, the hook bridge path, and the ingest URL.
// The actual spawn lives in `routes::codex_cards::create_codex_card`.
// ---------------------------------------------------------------------------

pub struct CodexClient {
    /// `codex` CLI to spawn. Defaults to `codex` (PATH lookup).
    pub codex_bin: String,
    /// `claude` CLI to spawn for manually-created Claude worker cards.
    /// Defaults to `claude` (PATH lookup).
    pub claude_bin: String,
    /// `neige-codex-bridge` binary path. The actual command codex invokes
    /// is `/usr/local/bin/neige-codex-bridge` (declared in
    /// `docker/codex-requirements.toml` as a policy-managed hook); this
    /// field records the canonical local path so the binary lookup at
    /// `cargo run` / packaging time picks up the workspace build. Resolved
    /// as a sibling of `calm-server` exe, falling back to bare name.
    pub bridge_bin: PathBuf,
    /// Loopback URL the bridge POSTs to (`http://127.0.0.1:<port>`).
    pub ingest_url: String,
    /// Per-card CODEX_HOME parent. Lives under `data_dir/codex-homes/`,
    /// which is `$HOME/.local/share/neige-calm/codex-homes/` by default
    /// — bind-mounted into the container, so it survives `docker compose
    /// down/up` and the codex card's auth.json + state stay alive across
    /// restarts. (The old `/tmp/`-based location was wiped on every
    /// container recreate, leaving the daemon stuck in a respawn loop.)
    pub codex_homes_dir: PathBuf,
    /// Single shared CODEX_HOME for the future shared Codex app-server.
    /// PR1 seeds/configures it only; legacy per-card callers keep using
    /// `codex_homes_dir` until later #410 PRs switch them.
    pub shared_codex_home: Arc<SharedCodexHome>,
    /// Parent directory for generated per-Claude-card `settings.json`
    /// files. This is only a hook settings sidecar, not a Claude home.
    pub claude_settings_dir: PathBuf,
    /// Test-only handle. When `new_stub()` constructs the client it stows
    /// a `tempfile::TempDir` here whose path contains both the legacy
    /// `codex_homes_dir` and PR1's shared `codex-home`.
    /// Holding the handle for the lifetime of the `CodexClient` (which
    /// is itself held inside `Arc<CodexClient>` on `AppState.codex`)
    /// guarantees the per-card `$CODEX_HOME` subdirs created under it
    /// get cleaned up when the test drops its `AppState` — closing the
    /// 134 GB-per-day leak described in issue #267 where the prior
    /// hardcoded `temp_dir().join("neige-codex-homes-stub")` shared one
    /// global dir across every test run.
    ///
    /// Production (`new`) leaves this `None`: `data_dir_resolved()` is a
    /// long-lived path that must survive the server process and the
    /// orchestration layer manages its lifecycle.
    _codex_homes_tempdir: Option<tempfile::TempDir>,
}

impl CodexClient {
    pub fn new(cfg: &Config) -> Self {
        let data_dir = cfg.data_dir_resolved();
        let legacy_homes_parent = data_dir.join("codex-homes");
        Self {
            codex_bin: cfg.codex_bin.clone(),
            claude_bin: cfg.claude_bin.clone(),
            bridge_bin: cfg
                .codex_bridge_bin
                .clone()
                .unwrap_or_else(resolve_codex_bridge_bin),
            ingest_url: cfg.codex_ingest_url_resolved(),
            codex_homes_dir: legacy_homes_parent.clone(),
            shared_codex_home: Arc::new(SharedCodexHome::new(
                data_dir.join("codex-home"),
                legacy_homes_parent,
            )),
            claude_settings_dir: data_dir.join("claude-settings"),
            _codex_homes_tempdir: None,
        }
    }

    /// Test stub — never actually spawns codex; tests that touch the
    /// codex routes don't need a real binary on PATH.
    ///
    /// **#267 — per-test temp dir for `codex_homes_dir`.** Earlier
    /// versions hardcoded the path to
    /// `std::env::temp_dir().join("neige-codex-homes-stub")`, a single
    /// global dir every test instance wrote into and nobody cleaned up
    /// — across enough test runs the dir grew to 100+ GB of codex
    /// session state (per-card `logs_*.sqlite*`, `history`, the seeded
    /// `~/.codex` copy). Now each `new_stub()` mints its own
    /// `tempfile::TempDir`, stashed in `_codex_homes_tempdir`, with
    /// `codex-homes/` for legacy per-card homes and `codex-home/` for
    /// PR1's shared home under it. The directory disappears when the
    /// `CodexClient` (and the `Arc` on `AppState.codex`) drops at test
    /// teardown. Falls back to the old shared path only if
    /// `TempDir::new()` fails — vanishingly rare in practice and the
    /// failure case isn't worth losing test coverage.
    pub fn new_stub() -> Self {
        let (temp_root, tmp) = match tempfile::Builder::new()
            .prefix("neige-codex-homes-stub-")
            .tempdir()
        {
            Ok(tmp) => (tmp.path().to_path_buf(), Some(tmp)),
            Err(e) => {
                // #272 (N2) — bumped from `warn!` to `error!`. This
                // fallback resurrects the pre-#267 shared `/tmp/neige-
                // codex-homes-stub` leak path; if it fires in CI the
                // test will silently revive the 134 GB/day leak fixed
                // by PR #271. `error!` is loud enough that triage
                // catches it on first occurrence instead of after the
                // next disk-full incident.
                tracing::error!(
                    error = %e,
                    "failed to create per-test codex_homes tempdir; \
                     falling back to shared `/tmp/neige-codex-homes-stub` \
                     — RESURRECTS THE #267 LEAK PATH (this test run will leak)"
                );
                (std::env::temp_dir().join("neige-codex-homes-stub"), None)
            }
        };
        let codex_homes_dir = temp_root.join("codex-homes");
        let shared_codex_home = Arc::new(SharedCodexHome::new(
            temp_root.join("codex-home"),
            codex_homes_dir.clone(),
        ));
        if let Err(e) = std::fs::create_dir_all(&codex_homes_dir) {
            tracing::error!(
                error = %e,
                path = %codex_homes_dir.display(),
                "failed to create stub codex_homes_dir"
            );
        }
        Self {
            codex_bin: "codex".into(),
            claude_bin: "claude".into(),
            bridge_bin: PathBuf::from("neige-codex-bridge"),
            ingest_url: "http://127.0.0.1:0".into(),
            claude_settings_dir: codex_homes_dir.join("claude-settings"),
            codex_homes_dir,
            shared_codex_home,
            _codex_homes_tempdir: tmp,
        }
    }

    /// Shared CODEX_HOME accessor (`codex_home_dir()` in the #410 PR gates).
    pub fn codex_home_dir(&self) -> &Path {
        self.shared_codex_home.path()
    }
}

fn resolve_codex_bridge_bin() -> PathBuf {
    if let Ok(exe) = std::env::current_exe()
        && let Some(dir) = exe.parent()
    {
        let candidate = dir.join("neige-codex-bridge");
        if candidate.exists() {
            return candidate;
        }
    }
    PathBuf::from("neige-codex-bridge")
}

/// PR7a (#136) — resolve the path to `neige-mcp-stdio-shim`. Same
/// "explicit override, sibling of running exe, else bare-name PATH lookup"
/// pattern as the codex-bridge resolver. The codex daemon will spawn this
/// binary from the path baked into each per-card `$CODEX_HOME/config.toml`'s
/// `[mcp_servers.calm].command` entry.
pub(crate) fn resolve_mcp_stdio_shim_bin(cfg: &Config) -> PathBuf {
    if let Some(path) = &cfg.mcp_stdio_shim_bin {
        return path.clone();
    }
    if let Ok(exe) = std::env::current_exe()
        && let Some(dir) = exe.parent()
    {
        let candidate = dir.join("neige-mcp-stdio-shim");
        if candidate.exists() {
            return candidate;
        }
    }
    PathBuf::from("neige-mcp-stdio-shim")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// PR3a (#293) — the per-card app-server socket must land under the
    /// user-owned data dir (the `app-server` 0700-chmods the socket's
    /// parent, which EPERMs on a shared sticky /tmp), in a per-card subdir
    /// — NOT directly in the shared data dir.
    #[test]
    fn appserver_sock_path_is_under_user_owned_data_dir_per_card() {
        // Mirror production: data_dir = <data_dir>/terminals.
        let data_dir = PathBuf::from("/home/u/.local/share/neige-calm");
        let daemon = DaemonClient {
            data_dir: data_dir.join("terminals"),
            proc_supervisor_sock: None,
        };

        let dir = daemon.appserver_sock_dir("card-abc");
        let sock = daemon.appserver_sock_path("card-abc");

        // Per-card subdir under <data_dir>/appserver/<card_id>/.
        assert_eq!(dir, data_dir.join("appserver").join("card-abc"));
        assert_eq!(sock, dir.join("app.sock"));

        // The 0700 chmod lands on the per-card subdir, never the shared
        // data dir itself.
        assert_ne!(dir, data_dir);
        assert!(sock.starts_with(&data_dir));
        assert!(sock.starts_with(data_dir.join("appserver")));
        // Each card gets its own subdir.
        assert_ne!(
            daemon.appserver_sock_dir("card-abc"),
            daemon.appserver_sock_dir("card-xyz")
        );
    }
}
