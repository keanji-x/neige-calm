use crate::config::Config;
use crate::shared_codex_home::SharedCodexHome;
use crate::terminal_renderer::TerminalRendererRegistry;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

/// Terminal data paths and the optional proc-supervisor socket shared by the REST + WS halves.
pub struct DaemonClient {
    /// Per-terminal sockets live under this directory as `<terminal_id>.sock`.
    pub data_dir: PathBuf,
    /// Control socket for `calm-proc-supervisor`; fixture tests may leave it unset to use an
    /// in-process framed supervisor.
    pub proc_supervisor_sock: Option<PathBuf>,
}

impl DaemonClient {
    pub fn new(cfg: &Config) -> Self {
        let data_dir = cfg.data_dir_resolved().join("terminals");
        Self {
            data_dir,
            proc_supervisor_sock: Some(cfg.proc_supervisor_sock_resolved()),
        }
    }

    /// Placeholder for tests / dev paths that don't have a full `Config`.
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

    /// Per-card directory for a planner card's `codex app-server` listen socket. Must be
    /// user-owned, not a sticky `/tmp` dir: the app-server `chmod 0700`s the socket's parent and
    /// EPERMs if it can't.
    pub fn appserver_sock_dir(&self, card_id: &str) -> PathBuf {
        let base = self.data_dir.parent().unwrap_or(&self.data_dir);
        base.join("appserver").join(card_id)
    }

    /// The `app.sock` path inside [`appserver_sock_dir`].
    pub fn appserver_sock_path(&self, card_id: &str) -> PathBuf {
        self.appserver_sock_dir(card_id).join("app.sock")
    }

    /// Kernel-private transient stdin injection through the in-process renderer's supervisor writer.
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
                        authority: crate::terminal_renderer::WriteAuthority::TrustedKernel,
                        data: bytes.to_vec(),
                        input_seq: 1,
                        ack: Some(ack_tx),
                        shape: crate::terminal_renderer::WriteShape::Verbatim,
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

pub struct CodexClient {
    /// `codex` CLI to spawn. Defaults to `codex` (PATH lookup).
    pub codex_bin: String,
    /// `claude` CLI to spawn for manually-created Claude worker cards.
    pub claude_bin: String,
    /// `neige-codex-bridge` binary path, resolved as a sibling of the `calm-server` exe, falling back to the bare name.
    pub bridge_bin: PathBuf,
    /// Loopback URL the bridge POSTs to (`http://127.0.0.1:<port>`).
    pub ingest_url: String,
    /// Per-card CODEX_HOME parent under `data_dir/codex-homes/`; bind-mounted so it survives container recreates.
    pub codex_homes_dir: PathBuf,
    /// Single shared CODEX_HOME for the shared Codex app-server.
    pub shared_codex_home: Arc<SharedCodexHome>,
    /// Parent directory for generated per-Claude-card `settings.json` (a hook settings sidecar, not a Claude home).
    pub claude_settings_dir: PathBuf,
    /// Parent directory for generated Planner terminal hook settings; server-owned.
    pub terminal_hook_settings_dir: PathBuf,
    /// Test-only handle: `new_stub()` stows its `TempDir` here so per-card `$CODEX_HOME`
    /// subdirs are removed when the test drops its `AppState`. `None` in production.
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
            terminal_hook_settings_dir: data_dir.join("terminal-hooks"),
            _codex_homes_tempdir: None,
        }
    }

    /// Test stub — never actually spawns codex. Mints a per-test `TempDir` for the codex homes;
    /// falls back to a shared temp path only if `TempDir::new()` fails.
    pub fn new_stub() -> Self {
        let (temp_root, tmp) = match tempfile::Builder::new()
            .prefix("neige-codex-homes-stub-")
            .tempdir()
        {
            Ok(tmp) => (tmp.path().to_path_buf(), Some(tmp)),
            Err(e) => {
                // `error!` on purpose: this fallback resurrects a shared, never-cleaned temp dir that leaks.
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
            terminal_hook_settings_dir: codex_homes_dir.join("terminal-hooks"),
            codex_homes_dir,
            shared_codex_home,
            _codex_homes_tempdir: tmp,
        }
    }

    /// Shared CODEX_HOME accessor.
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

/// Resolve the path to `neige-mcp-stdio-shim`: explicit override, sibling of running exe,
/// else bare-name PATH lookup.
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

    #[test]
    fn appserver_sock_path_is_under_user_owned_data_dir_per_card() {
        let data_dir = PathBuf::from("/home/u/.local/share/neige-calm");
        let daemon = DaemonClient {
            data_dir: data_dir.join("terminals"),
            proc_supervisor_sock: None,
        };

        let dir = daemon.appserver_sock_dir("card-abc");
        let sock = daemon.appserver_sock_path("card-abc");

        assert_eq!(dir, data_dir.join("appserver").join("card-abc"));
        assert_eq!(sock, dir.join("app.sock"));

        // The 0700 chmod lands on the per-card subdir, never the shared data dir itself.
        assert_ne!(dir, data_dir);
        assert!(sock.starts_with(&data_dir));
        assert!(sock.starts_with(data_dir.join("appserver")));
        assert_ne!(
            daemon.appserver_sock_dir("card-abc"),
            daemon.appserver_sock_dir("card-xyz")
        );
    }
}
