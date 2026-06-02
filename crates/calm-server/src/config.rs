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

    /// Optional built web bundle to serve under `/calm/`.
    /// Docker dev usually lets nginx serve this. Host prod sets this to
    /// `web/dist` so a single local calm-server process serves both SPA and API.
    #[arg(long, env = "CALM_WEB_DIST")]
    pub web_dist: Option<PathBuf>,

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

    /// Override path for the `neige-mcp-stdio-shim` binary that codex MCP
    /// clients spawn from each card's generated config.toml. Defaults to
    /// looking next to `calm-server`, then PATH. Host prod sets this to a
    /// stable ~/.local/bin symlink so old docker paths do not leak into
    /// local codex homes.
    #[arg(long, env = "CALM_MCP_STDIO_SHIM_BIN")]
    pub mcp_stdio_shim_bin: Option<PathBuf>,

    /// Base URL the codex hook bridge uses to POST back to calm-server.
    /// Defaults to `http://<listen>` — when `listen` binds `0.0.0.0`, we
    /// rewrite the host to `127.0.0.1` for the loopback POST. Override if
    /// you front calm-server with a proxy.
    #[arg(long, env = "CALM_CODEX_INGEST_URL")]
    pub codex_ingest_url: Option<String>,

    // ---- auth (issue #189) -------------------------------------------------
    //
    // Single-user owner auth. The kernel runs in one of two modes:
    //
    //   * Production: `auth_dev_autologin = false`. `auth_password` is
    //     REQUIRED — boot panics in `auth::AuthConfig::from_config` if
    //     it's unset. The configured username/password is the only way
    //     to obtain a session cookie. `auth_username` defaults to
    //     `"owner"` if unset; downstream display name follows.
    //
    //   * Dev: `auth_dev_autologin = true`. The middleware promotes
    //     every request to the owner principal without a cookie, and
    //     `whoami` returns the owner shape unconditionally. Skips the
    //     credential check; useful for `make dev` loops and e2e
    //     fixtures where typing a password every reload is pure
    //     friction. Production deploys MUST NOT enable this.
    /// Configured owner username for `POST /api/auth/login`. Single-user
    /// model — there's only ever one valid username. Defaults to `owner`.
    #[arg(long, env = "CALM_AUTH_USERNAME", default_value = "owner")]
    pub auth_username: Option<String>,

    /// Configured owner password for `POST /api/auth/login`. Required when
    /// `auth_dev_autologin` is off; boot panics otherwise. Plain string
    /// today (no hashing) because this is the single-user owner model on a
    /// local-only deployment — adding bcrypt/argon2 buys nothing when the
    /// only attacker who can reach the process can already read its env.
    #[arg(long, env = "CALM_AUTH_PASSWORD")]
    pub auth_password: Option<String>,

    /// Skip the cookie/login flow and promote every request to the owner
    /// principal. ALWAYS off by default. Used by `make dev` loops; explicit
    /// env/config opt-in only. Production deploys MUST NOT enable this.
    #[arg(long, env = "CALM_DEV_AUTOLOGIN", default_value_t = false)]
    pub auth_dev_autologin: bool,

    /// PR4 (#410) — boot one shared `codex app-server` for future card routes.
    /// Rollback switch: when false, no shared daemon is started or taken over.
    #[arg(
        long,
        env = "CALM_SHARED_CODEX_APPSERVER_ENABLED",
        default_value_t = true
    )]
    pub shared_codex_appserver_enabled: bool,

    /// Route prompt-bearing user codex cards through the shared codex
    /// app-server (PR4) when `true`.
    /// `shared_codex_prompt_cards_enabled` default false keeps PR3c opt-in
    /// until ops confirms shared-daemon stability and addresses these followups:
    /// - settings.http_proxy / https_proxy hot-reload (R7): currently
    ///   the daemon reads settings at spawn time; changes require daemon
    ///   restart. Legacy per-card path reads settings per spawn.
    /// - any further Channel B review findings as we accumulate production
    ///   telemetry on shared daemon.
    #[arg(
        long,
        env = "CALM_SHARED_CODEX_PROMPT_CARDS_ENABLED",
        default_value_t = false
    )]
    pub shared_codex_prompt_cards_enabled: bool,

    /// PR6 -> PR3c decoupling gate. Default false keeps empty user codex
    /// cards on the legacy per-card CODEX_HOME path until shared-daemon
    /// prompt identity is confirmed stable by operators.
    #[arg(
        long,
        env = "CALM_SHARED_CODEX_EMPTY_CARDS_ENABLED",
        default_value_t = false
    )]
    pub shared_codex_empty_cards_enabled: bool,

    /// Route spec cards created by `POST /api/waves` through the shared
    /// codex app-server when `true`. Default false preserves the legacy
    /// per-wave app-server path until PR7b is explicitly enabled.
    #[arg(
        long,
        env = "CALM_SHARED_CODEX_SPEC_CARDS_ENABLED",
        default_value_t = false
    )]
    pub shared_codex_spec_cards_enabled: bool,

    /// Route dispatcher-spawned worker codex cards through the shared
    /// codex app-server when `true`. Default false preserves the legacy
    /// per-card daemon path until PR7b-worker is explicitly enabled.
    #[arg(
        long,
        env = "CALM_SHARED_CODEX_WORKER_CARDS_ENABLED",
        default_value_t = false
    )]
    pub shared_codex_worker_cards_enabled: bool,

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

    pub fn proc_supervisor_sock_resolved(&self) -> PathBuf {
        self.proc_supervisor_sock
            .clone()
            .unwrap_or_else(|| self.data_dir_resolved().join("proc-supervisor.sock"))
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

    pub fn shared_codex_appserver_log_dir_resolved(&self) -> PathBuf {
        self.shared_codex_appserver_log_dir
            .clone()
            .unwrap_or_else(|| self.data_dir_resolved().join("logs/shared-codex-appserver"))
    }
}
