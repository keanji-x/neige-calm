//! Boot-time configuration. Read once in `main`, frozen for the process.

use clap::Parser;
use std::path::PathBuf;

#[derive(Parser, Debug, Clone)]
#[command(name = "calm-server", version, about = "neige-calm kernel")]
pub struct Config {
    /// Print KernelCompatibility JSON and exit without opening storage or sockets.
    #[arg(long, default_value_t = false)]
    pub emit_kernel_compatibility_json: bool,

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

    /// Root under which server-managed track workspaces live, one git repository per track at `<root>/<area_id>/<track_id>`; defaults to `$HOME/neige-workspaces`.
    /// Deliberately NOT `CALM_DATA_DIR`: that is resettable runtime state, while a track workspace is a user-visible product that must survive a state reset.
    #[arg(long, env = "CALM_WORKSPACE_ROOT")]
    pub workspace_root: Option<PathBuf>,

    /// Explicit configuration for opted-in single-task isolated Codex execution.
    /// Missing keeps this backend unavailable; existing tasks keep their route.
    #[arg(long)]
    pub isolated_codex_config: Option<PathBuf>,

    /// Unix socket used to ask calm-proc-supervisor to fork session daemons.
    /// Defaults to `<CALM_DATA_DIR>/proc-supervisor.sock`.
    #[arg(long, env = "CALM_PROC_SUPERVISOR_SOCK")]
    pub proc_supervisor_sock: Option<PathBuf>,

    /// CORS origin allowed by the API (typically the web-calm dev origin).
    #[arg(
        long,
        env = "CALM_ALLOWED_ORIGIN",
        default_value = "http://localhost:5175"
    )]
    pub allowed_origin: String,

    /// Retired legacy bundle setting, accepted so existing service configs can
    /// upgrade. It logs a warning and never mounts `/calm/`; use `fe_dist`.
    #[arg(long, env = "CALM_WEB_DIST")]
    pub web_dist: Option<PathBuf>,

    /// Optional built next-generation frontend bundle to serve under `/next/`.
    #[arg(long, env = "CALM_FE_DIST")]
    pub fe_dist: Option<PathBuf>,

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

    /// Directory of operator-provided track templates (`*.md` with `+++` TOML front matter), exposed as `site/<stem>`. Read once at boot, fail-closed.
    /// Flag only, no `env =`; under neige-app, pass it through `[child].extra_args`.
    #[arg(long)]
    pub templates_dir: Option<PathBuf>,

    /// Override path for the `codex` CLI binary. Defaults to `codex` (PATH
    /// lookup). The docker compose stack bind-mounts the musl static
    /// binary directly into the container as `/usr/local/bin/codex`.
    #[arg(long, env = "CALM_CODEX_BIN", default_value = "codex")]
    pub codex_bin: String,

    /// Override path for the `claude` CLI binary. Defaults to `claude`
    /// (PATH lookup). Claude cards use the user's subscription login and
    /// do not receive ANTHROPIC_API_KEY from calm.
    #[arg(long, env = "CALM_CLAUDE_BIN", default_value = "claude")]
    pub claude_bin: String,

    /// Override path for the `neige-codex-bridge` binary that codex hooks
    /// shell out to. Defaults to looking next to `calm-server`, then PATH.
    /// See `state::resolve_codex_bridge_bin`.
    #[arg(long, env = "CALM_CODEX_BRIDGE_BIN")]
    pub codex_bridge_bin: Option<PathBuf>,

    /// Override path for the `neige-mcp-stdio-shim` binary that codex MCP clients spawn; defaults to next to `calm-server`, then PATH.
    #[arg(long, env = "CALM_MCP_STDIO_SHIM_BIN")]
    pub mcp_stdio_shim_bin: Option<PathBuf>,

    /// Base URL the codex hook bridge uses to POST back. Defaults to `http://<listen>` with a `0.0.0.0` host rewritten to `127.0.0.1`; override if a proxy fronts calm-server.
    #[arg(long, env = "CALM_CODEX_INGEST_URL")]
    pub codex_ingest_url: Option<String>,

    #[arg(long, env = "CALM_AUTH_USERNAME", default_value = "owner")]
    pub auth_username: Option<String>,

    /// Required when `auth_dev_autologin` is off; boot panics otherwise. Plain string: on a local-only single-user deployment, hashing buys nothing against an attacker who can already read the process env.
    #[arg(long, env = "CALM_AUTH_PASSWORD")]
    pub auth_password: Option<String>,

    /// Promote every request to the owner principal without a login. ALWAYS off by default; production deploys MUST NOT enable this.
    #[arg(long, env = "CALM_DEV_AUTOLOGIN", default_value_t = false)]
    pub auth_dev_autologin: bool,

    /// Opt-in server-side Funnel configuration for mobile QR pairing.
    #[arg(long)]
    pub mobile_access_config: Option<PathBuf>,

    /// App-owned private Tailnet ingress. Mutually exclusive with system Funnel.
    #[arg(long, conflicts_with = "mobile_access_config")]
    pub private_tailnet_config: Option<PathBuf>,

    /// App-owned remote subsystem failed before the kernel started. This
    /// diagnostic does not guess or replace the stored remote-access intent.
    #[arg(long, conflicts_with_all = ["private_tailnet_config", "mobile_access_config"])]
    pub private_tailnet_unavailable: bool,

    /// Initial delay before restarting the shared codex app-server after a crash.
    #[arg(
        long,
        env = "CALM_SHARED_CODEX_APPSERVER_RESTART_INITIAL_DELAY_MS",
        default_value_t = 250
    )]
    pub shared_codex_appserver_restart_initial_delay_ms: u64,

    /// Maximum exponential-backoff delay for shared app-server restarts.
    #[arg(
        long,
        env = "CALM_SHARED_CODEX_APPSERVER_RESTART_MAX_DELAY_MS",
        default_value_t = 10_000
    )]
    pub shared_codex_appserver_restart_max_delay_ms: u64,

    /// Cold-start deadline for a freshly spawned shared codex app-server to bind its socket and answer `initialize`; codex may spend minutes rebuilding its state db before the socket exists.
    /// The default (120) is mirrored by neige-app's `CALM_START_TIMEOUT_DEFAULT_SECS`; keep the two in sync.
    #[arg(
        long,
        env = "CALM_SHARED_CODEX_APPSERVER_START_TIMEOUT_SECS",
        default_value_t = 120
    )]
    pub shared_codex_appserver_start_timeout_secs: u64,

    /// After SIGTERM, how long the supervisor waits for the shared codex app-server's verified exit before the final group SIGKILL; exit-driven, so a cooperative daemon pays only its actual exit time.
    /// Validated 1..=600: 0 would restore an instant-SIGKILL defect that arms codex's backfill lease. The default (60) is mirrored by neige-app's `CALM_STOP_GRACE_DEFAULT_SECS`; keep the two in sync.
    #[arg(
        long,
        env = "CALM_SHARED_CODEX_APPSERVER_STOP_GRACE_SECS",
        default_value_t = 60,
        value_parser = clap::value_parser!(u64).range(1..=600)
    )]
    pub shared_codex_appserver_stop_grace_secs: u64,

    /// Log directory for the shared codex app-server child.
    #[arg(long, env = "CALM_SHARED_CODEX_APPSERVER_LOG_DIR")]
    pub shared_codex_appserver_log_dir: Option<PathBuf>,
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

    /// `$HOME/neige-workspaces` unless overridden. The `HOME`-unset fallback is `current_dir()`, not `.`: materialization requires an absolute path, and a relative root would fail every track create under systemd.
    pub fn workspace_root_resolved(&self) -> PathBuf {
        self.workspace_root.clone().unwrap_or_else(|| {
            std::env::var_os("HOME")
                .map(PathBuf::from)
                .filter(|home| !home.as_os_str().is_empty())
                .unwrap_or_else(|| {
                    std::env::current_dir()
                        .expect("resolving the workspace root needs HOME or a usable cwd")
                })
                .join("neige-workspaces")
        })
    }

    pub fn proc_supervisor_sock_resolved(&self) -> PathBuf {
        self.proc_supervisor_sock
            .clone()
            .unwrap_or_else(|| self.data_dir_resolved().join("proc-supervisor.sock"))
    }

    /// Anchored at `XDG_CONFIG_HOME`: plugin binaries + assets are read-only config, not state.
    pub fn plugins_dir_resolved(&self) -> PathBuf {
        self.plugins_dir.clone().unwrap_or_else(|| {
            let base = std::env::var_os("XDG_CONFIG_HOME")
                .map(PathBuf::from)
                .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
                .unwrap_or_else(|| PathBuf::from("."));
            base.join("neige-calm").join("plugins")
        })
    }

    /// Distinct from `plugins_dir` so uninstall can wipe state without touching the code dir, and vice versa.
    pub fn plugins_data_dir_resolved(&self) -> PathBuf {
        self.plugins_data_dir.clone().unwrap_or_else(|| {
            let base = std::env::var_os("XDG_DATA_HOME")
                .map(PathBuf::from)
                .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share")))
                .unwrap_or_else(|| PathBuf::from("."));
            base.join("neige-calm").join("plugins")
        })
    }

    /// Rewrites a `0.0.0.0` bind to `127.0.0.1` so the child process reaches a routable address.
    pub fn codex_ingest_url_resolved(&self) -> String {
        if let Some(u) = &self.codex_ingest_url {
            return u.clone();
        }
        let listen = self.listen.replacen("0.0.0.0", "127.0.0.1", 1);
        format!("http://{listen}")
    }

    pub fn shared_codex_appserver_log_dir_resolved(&self) -> PathBuf {
        self.shared_codex_appserver_log_dir
            .clone()
            .unwrap_or_else(|| self.data_dir_resolved().join("logs/shared-codex-appserver"))
    }
}
