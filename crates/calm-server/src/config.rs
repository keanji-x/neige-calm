//! Boot-time configuration. Read once in `main`, frozen for the process.

use clap::Parser;
use std::path::PathBuf;

#[derive(Parser, Debug, Clone)]
#[command(name = "calm-server", about = "neige-calm kernel")]
pub struct Config {
    /// HTTP listen address.
    #[arg(long, env = "CALM_LISTEN", default_value = "127.0.0.1:4040")]
    pub listen: String,

    /// Storage URL. `sqlite://path/to/file.db?mode=rwc` or `mock` for an
    /// in-memory `SqlxRepo` (`sqlite::memory:`, handy for dev/tests; not
    /// durable).
    #[arg(long, env = "CALM_DB_URL", default_value = "mock")]
    pub db_url: String,

    /// Root directory for runtime state (PTY sockets, daemon scratch).
    /// Defaults to `<XDG_DATA_HOME>/neige-calm` or `~/.local/share/neige-calm`.
    #[arg(long, env = "CALM_DATA_DIR")]
    pub data_dir: Option<PathBuf>,

    /// CORS origin allowed by the API (typically the web-calm dev origin).
    #[arg(
        long,
        env = "CALM_ALLOWED_ORIGIN",
        default_value = "http://localhost:5175"
    )]
    pub allowed_origin: String,

    /// Plugin install root (read-only code). Defaults to
    /// `<XDG_CONFIG_HOME>/neige-calm/plugins` or `~/.config/neige-calm/plugins`.
    #[arg(long, env = "CALM_PLUGINS_DIR")]
    pub plugins_dir: Option<PathBuf>,

    /// Plugin mutable-state root (kv stores, logs). Defaults to
    /// `<XDG_DATA_HOME>/neige-calm/plugins` or `~/.local/share/neige-calm/plugins`.
    #[arg(long, env = "CALM_PLUGINS_DATA_DIR")]
    pub plugins_data_dir: Option<PathBuf>,

    /// Plugin ids to skip enabling at boot. Comma-separated on the CLI / env.
    /// Useful for debugging a misbehaving plugin without touching its row.
    #[arg(long, env = "CALM_PLUGINS_DISABLED", value_delimiter = ',', num_args = 0..)]
    pub plugins_disabled: Vec<String>,

    /// Override path for the `codex` CLI binary. Defaults to `codex` (PATH
    /// lookup). The docker compose stack bind-mounts the musl static
    /// binary directly into the container as `/usr/local/bin/codex`.
    #[arg(long, env = "CALM_CODEX_BIN", default_value = "codex")]
    pub codex_bin: String,

    /// Override path for the `neige-codex-bridge` binary that codex hooks
    /// shell out to. Defaults to looking next to `calm-server`, then PATH.
    /// See `state::resolve_codex_bridge_bin`.
    #[arg(long, env = "CALM_CODEX_BRIDGE_BIN")]
    pub codex_bridge_bin: Option<PathBuf>,

    /// Base URL the codex hook bridge uses to POST back to calm-server.
    /// Defaults to `http://<listen>` — when `listen` binds `0.0.0.0`, we
    /// rewrite the host to `127.0.0.1` for the loopback POST. Override if
    /// you front calm-server with a proxy.
    #[arg(long, env = "CALM_CODEX_INGEST_URL")]
    pub codex_ingest_url: Option<String>,
}

impl Config {
    pub fn data_dir_resolved(&self) -> PathBuf {
        self.data_dir.clone().unwrap_or_else(|| {
            let base = std::env::var_os("XDG_DATA_HOME")
                .map(PathBuf::from)
                .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share")))
                .unwrap_or_else(|| PathBuf::from("."));
            base.join("neige-calm")
        })
    }

    /// Where plugin install dirs live. Mirrors `data_dir_resolved`'s XDG
    /// fallback chain but anchored at `XDG_CONFIG_HOME` since plugin binaries
    /// + assets are read-only config, not state.
    pub fn plugins_dir_resolved(&self) -> PathBuf {
        self.plugins_dir.clone().unwrap_or_else(|| {
            let base = std::env::var_os("XDG_CONFIG_HOME")
                .map(PathBuf::from)
                .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
                .unwrap_or_else(|| PathBuf::from("."));
            base.join("neige-calm").join("plugins")
        })
    }

    /// Where per-plugin mutable state lives. Distinct from `plugins_dir` so
    /// uninstall can wipe state without touching the code dir, and vice versa.
    pub fn plugins_data_dir_resolved(&self) -> PathBuf {
        self.plugins_data_dir.clone().unwrap_or_else(|| {
            let base = std::env::var_os("XDG_DATA_HOME")
                .map(PathBuf::from)
                .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share")))
                .unwrap_or_else(|| PathBuf::from("."));
            base.join("neige-calm").join("plugins")
        })
    }

    /// Base URL bridges use to POST hook events back to the loopback
    /// ingest endpoint. Rewrites a `0.0.0.0` bind to `127.0.0.1` so the
    /// child process actually reaches a routable address.
    pub fn codex_ingest_url_resolved(&self) -> String {
        if let Some(u) = &self.codex_ingest_url {
            return u.clone();
        }
        let listen = self.listen.replacen("0.0.0.0", "127.0.0.1", 1);
        format!("http://{listen}")
    }
}
