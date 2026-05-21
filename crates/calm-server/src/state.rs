//! Shared app state passed to every handler.
//!
//! `Clone` is cheap — everything inside is wrapped in `Arc` or already
//! reference-counted internally.

use crate::config::Config;
use crate::db::{Repo, RouteRepo};
use crate::event::EventBus;
use crate::plugin_host::{PluginHost, PluginRegistry};
use std::path::PathBuf;
use std::sync::Arc;

/// Route-facing handle: the trait object `AppState::repo` exposes. Excludes
/// `RepoSyncDomainRaw` — see `db/mod.rs` module doc for the capability split.
///
/// `Arc<dyn RouteRepo>` is what handlers see; integration tests that need
/// to seed fixtures reach `&dyn Repo` via [`AppState::raw_repo`], which
/// is gated behind the `fixtures` cargo feature (only enabled for the
/// `tests/*.rs` integration crates via the self-loop dev-dep). No
/// production module reaches for `raw_repo` today.
#[derive(Clone)]
pub struct AppState {
    /// Narrow trait object: reads + eventized writes + out-of-domain writes.
    /// Sync-domain raw writes (`cove_create`, `wave_update`, `card_delete`,
    /// `overlay_upsert`, etc.) are unreachable from this handle — handlers
    /// must funnel them through `db::write_with_event_typed`.
    pub repo: Arc<dyn RouteRepo>,
    pub events: EventBus,
    pub daemon: Arc<DaemonClient>,
    pub plugin: Arc<PluginHost>,
    pub codex: Arc<CodexClient>,
    /// Full-capability handle. Held separately from `repo` so the gate at
    /// `AppState::repo` survives even though the underlying concrete impl
    /// is the same `SqlxRepo`. Kept private — callers must go through
    /// [`AppState::raw_repo`] (only visible under `--features fixtures`).
    /// `allow(dead_code)` because in non-`fixtures` builds (the production
    /// binary, the `replay` lib, etc.) nothing reads this field — it's
    /// stored so the `fixtures`-only accessor still has something to hand
    /// out, but the production build keeps the field opaque on purpose.
    #[allow(dead_code)]
    raw: Arc<dyn Repo>,
}

impl AppState {
    /// Bypass the sync-domain gate. **For test-fixture seeding only** —
    /// production code MUST go through `write_with_event_typed` /
    /// `log_pure_event`. Gated behind the `fixtures` cargo feature so
    /// production builds (the binary, `routes/*`, `plugin_host/*`,
    /// `terminal_sweeper`, and the `replay` lib) physically cannot reach
    /// this method — invoking it from a production module fails at
    /// compile time with E0599 (`no method named raw_repo`). Integration
    /// tests pick up the feature automatically via the `[dev-dependencies]`
    /// self-loop in `Cargo.toml`.
    #[cfg(feature = "fixtures")]
    pub fn raw_repo(&self) -> &dyn Repo {
        self.raw.as_ref()
    }

    /// Test / replay-lib hatch: build an `AppState` from already-constructed
    /// pieces, skipping the boot-time plugin registry load + background
    /// task spawn that `new` does. Public so `replay::boot_in_memory` and
    /// integration tests can compose the struct without bypassing the
    /// `raw` field's privacy (which is what guards the capability split
    /// from external `AppState { ... }` literals).
    pub fn from_parts(
        repo: Arc<dyn Repo>,
        events: EventBus,
        daemon: Arc<DaemonClient>,
        plugin: Arc<PluginHost>,
        codex: Arc<CodexClient>,
    ) -> Self {
        let route_repo: Arc<dyn RouteRepo> = repo.clone();
        Self {
            repo: route_repo,
            events,
            daemon,
            plugin,
            codex,
            raw: repo,
        }
    }

    /// Real boot-time constructor. Loads the plugin manifest registry from
    /// `cfg.plugins_dir`, creating the directory if it doesn't exist (fresh
    /// install path), wires up `DaemonClient` + `EventBus` + `PluginHost`,
    /// and auto-spawns every enabled plugin via `PluginHost::autospawn_enabled`.
    ///
    /// If the registry load returns an error (e.g. duplicate plugin id) we
    /// surface it: that's a hard misconfiguration the operator needs to fix.
    /// Per-plugin parse failures (and per-plugin spawn failures) are already
    /// downgraded to `tracing::warn!` so one broken plugin can't block boot.
    pub async fn new(cfg: &Config, repo: Arc<dyn Repo>) -> anyhow::Result<Self> {
        let plugins_dir = cfg.plugins_dir_resolved();
        if !plugins_dir.exists() {
            // Fresh-install path: a missing dir is normal on first boot. We
            // create it so that subsequent installs (Slice D) have a target.
            tracing::info!(
                plugins_dir = %plugins_dir.display(),
                "creating plugins dir"
            );
            std::fs::create_dir_all(&plugins_dir)?;
        }
        let (registry, report) = PluginRegistry::load_from_dir(&plugins_dir)?;
        tracing::info!(
            loaded = report.loaded.len(),
            skipped = report.skipped.len(),
            "plugin registry loaded"
        );

        // Same treatment for the data dir — Slice B/C will write into per-plugin
        // subdirs of this, so make sure the root exists at boot.
        let plugins_data_dir = cfg.plugins_data_dir_resolved();
        if !plugins_data_dir.exists() {
            tracing::info!(
                plugins_data_dir = %plugins_data_dir.display(),
                "creating plugins data dir"
            );
            std::fs::create_dir_all(&plugins_data_dir)?;
        }

        let events = EventBus::new();

        // Per-card FSM (phase 1: codex cards only). Subscribes to the bus
        // and projects `codex.hook` events onto a 6-state FSM, writing
        // `Overlay { kind: "status" }` rows for cards and wave-union rows
        // for waves. See `card_fsm` module docs for the scope rationale.
        crate::card_fsm::spawn(repo.clone(), events.clone());

        // Hands-free codex auto-submit. Subscribes to the same bus and,
        // when a `hook.codex.session_start` event fires for a card whose
        // `payload.auto_submit == true`, injects a `\r` into the daemon
        // socket ~600 ms later so codex's composer pre-fill submits
        // itself. Separate from `card_fsm` because it's a side-effecting
        // subscriber, not a projector. See `codex_auto_submit` module
        // docs for the trust model and v2-protocol interaction.
        let daemon = Arc::new(DaemonClient::new(cfg));
        crate::codex_auto_submit::spawn(repo.clone(), daemon.clone(), events.clone());

        let plugin = Arc::new(PluginHost::new_full(
            Arc::new(registry),
            repo.clone(),
            plugins_dir,
            plugins_data_dir,
            cfg.plugins_disabled.clone(),
            events.clone(),
        ));

        // Auto-spawn every enabled plugin row. Per-plugin errors are logged
        // inside `autospawn_enabled`; we never let one broken plugin block
        // the rest of the boot path.
        plugin.autospawn_enabled().await;

        // Upcast the full `Arc<dyn Repo>` to the narrow `Arc<dyn RouteRepo>`
        // exposed via `AppState::repo`. Stable trait-object upcasting (Rust
        // 1.86+) gives us this for free because `Repo: RouteRepo`.
        let route_repo: Arc<dyn RouteRepo> = repo.clone();
        let state = Self {
            repo: route_repo,
            events,
            daemon,
            plugin,
            codex: Arc::new(CodexClient::new(cfg)),
            raw: repo,
        };

        // Orphan-terminal sweeper (Scope C). Ticks every 30s, reaps
        // terminal rows that no card references via `payload.terminal_id`
        // (with a 1-minute grace window for the 3-step create race), and
        // emits `Event::TerminalDeleted` through the same `write_with_event`
        // pipeline every other write uses so the cleanup is audited. See
        // `terminal_sweeper` module docs and `docs/sync-engine-design.md` §10.
        crate::terminal_sweeper::spawn(state.clone());

        Ok(state)
    }
}

// ---------------------------------------------------------------------------
// DaemonClient — owned by Track D.
//
// Wraps the connection / spawning logic for `calm-session-daemon` so REST
// + WS terminal handlers can talk to PTYs without leaking the framed-binary
// protocol details into the rest of the codebase.
// ---------------------------------------------------------------------------

/// Lightweight handle the REST + WS halves both consult. The handle is
/// "lightweight" because the daemon is its own long-lived process — we don't
/// pool stream connections through here; instead WS handlers connect on
/// demand using the stored socket path. All `DaemonClient` needs to do is
/// (a) know where to put per-terminal sockets and (b) know which binary to
/// spawn.
pub struct DaemonClient {
    /// Per-terminal sockets live under this directory as `<terminal_id>.sock`.
    /// Created on first use by `routes::terminal::create`. Defaults to
    /// `<config.data_dir>/terminals`.
    pub data_dir: PathBuf,
    /// Path to the `calm-session-daemon` binary. Resolved at startup to be
    /// a sibling of the running `calm-server` exe (so `cargo run` /
    /// `target/release` layouts work without an install step); falls back to
    /// `calm-session-daemon` and lets `$PATH` lookup happen at spawn.
    pub session_daemon_bin: PathBuf,
}

impl DaemonClient {
    /// Real constructor. Pulls `data_dir` from the resolved config and
    /// locates the daemon binary next to the current executable.
    pub fn new(cfg: &Config) -> Self {
        let data_dir = cfg.data_dir_resolved().join("terminals");
        Self {
            data_dir,
            session_daemon_bin: resolve_session_daemon_bin(),
        }
    }

    /// Placeholder for tests / dev paths that don't have a full `Config`.
    /// Sockets land in a per-uid tempdir; binary lookup falls back to `$PATH`.
    pub fn new_stub() -> Self {
        let tmp = std::env::var_os("XDG_RUNTIME_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("/tmp"))
            .join("calm-terminals");
        Self {
            data_dir: tmp,
            session_daemon_bin: resolve_session_daemon_bin(),
        }
    }

    /// Socket path for a given terminal id.
    pub fn sock_path(&self, terminal_id: &str) -> PathBuf {
        self.data_dir.join(format!("{terminal_id}.sock"))
    }

    /// Inject raw bytes into a live terminal's PTY stdin over its daemon
    /// socket, as if a keyboard had typed them.
    ///
    /// This is the kernel's privileged write path — used by
    /// `codex_auto_submit` to submit a hands-free agent's pre-filled
    /// prompt with a `\r`. The trust contract is enforced by the
    /// daemon's protocol layer via
    /// [`calm_session::ClientCapabilities::kernel_originated_input`]:
    ///
    ///   1. We open the per-terminal Unix socket (kernel-private — never
    ///      crosses a network boundary, so the capability is honest).
    ///   2. Frame a [`ClientMsg::ClientHello`] with `capabilities.
    ///      kernel_originated_input = true` and `role_hint = Observer`
    ///      (we are NOT trying to steal the browser's Owner role —
    ///      coexisting is the whole point).
    ///   3. Frame a [`ClientMsg::Input`] with the requested bytes.
    ///   4. Drop the connection. The daemon disposes of the per-client
    ///      `TerminalSessionState` and the browser's Owner connection
    ///      (still attached separately) is undisturbed.
    ///
    /// The daemon's owner-only gate on `Input` accepts our write because
    /// of the `kernel_originated_input` capability (see
    /// `crates/calm-session/src/terminal_session.rs` `on_client_frame`).
    /// The WS bridge strips this flag on every browser-originated
    /// `ClientHello`, so the trust model is intact: only callers reaching
    /// the daemon directly over its private Unix socket can set it.
    ///
    /// `terminal_id` is the same id `card.payload.terminal_id` stamps
    /// (and the same `term.id` the daemon was spawned with). It may
    /// arrive in either the simple no-dashes `model::new_id` form or
    /// the hyphenated `Uuid::Display` form; we normalize to hyphenated
    /// internally because the daemon compares `ClientHello.terminal_id`
    /// byte-for-byte against its own `cli.id.to_string()` (always
    /// hyphenated). This mirrors the normalization the WS bridge does
    /// on every browser-originated `ClientHello` (see
    /// `crates/calm-server/src/ws/terminal.rs` §CORRECTNESS) so the
    /// kernel-side path has the same handshake-compat guarantee.
    pub async fn inject_stdin(
        &self,
        sock_path: &std::path::Path,
        terminal_id: &str,
        bytes: &[u8],
    ) -> std::io::Result<()> {
        use calm_session::{
            ClientCapabilities, ClientMsg, InitialScrollback, PROTOCOL_VERSION, PtySize,
            RenderEncoding, Role, write_frame,
        };
        use tokio::net::UnixStream;

        // Normalize to hyphenated form for the handshake (see method
        // doc). If it doesn't parse as a Uuid we forward verbatim and
        // let the daemon reject with `BadHandshake` — that's the
        // correct fail-loud behavior for a malformed id.
        let handshake_terminal_id = uuid::Uuid::parse_str(terminal_id)
            .map(|u| u.to_string())
            .unwrap_or_else(|_| terminal_id.to_string());

        let mut stream = UnixStream::connect(sock_path).await?;

        // ClientHello: `Observer` hint + `kernel_originated_input: true`.
        // The Observer hint matters because if the browser dropped its
        // own connection in the meantime we'd otherwise grab Owner — we
        // want to be passive in every case so we never need to remember
        // to release it.
        let hello = ClientMsg::ClientHello {
            protocol_version: PROTOCOL_VERSION,
            terminal_id: handshake_terminal_id,
            client_id: uuid::Uuid::new_v4(),
            desired_size: PtySize {
                cols: 80,
                rows: 24,
                pixel_width: None,
                pixel_height: None,
            },
            cell_size: None,
            initial_scrollback: InitialScrollback::None,
            resume_from: None,
            role_hint: Some(Role::Observer),
            capabilities: ClientCapabilities {
                render_encodings: vec![RenderEncoding::Vt],
                supports_scrollback: false,
                supports_sixel: false,
                supports_images: false,
                kernel_originated_input: true,
            },
        };
        write_frame(&mut stream, &hello).await.map_err(io_other)?;

        // Input: the bytes we want the daemon to forward to PTY stdin.
        let input = ClientMsg::Input(bytes.to_vec());
        write_frame(&mut stream, &input).await.map_err(io_other)?;

        // Hold the connection open briefly so the daemon's read loop
        // can drain both frames before our drop reaches the kernel as
        // EOF. Without this gap, fast machines see the EOF before the
        // ResizePty/Input effects finish applying; the Input frame is
        // queued in the kernel's socket buffer but the daemon's
        // `read_frame` returns `Err(io)` on the EOF mid-frame and the
        // loop breaks without delivering the bytes.
        //
        // 50 ms is well below any human-perceptible budget and
        // comfortably above the daemon's per-frame parse + effect
        // application time (sub-millisecond on dev hardware).
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        // Drop the stream — the daemon's read loop sees EOF cleanly.
        drop(stream);
        Ok(())
    }
}

/// Promote a `FrameError` (or any error type whose `Display` makes sense
/// in a transport-error log line) into an `io::Error`. We give up the
/// typed error here — `inject_stdin`'s caller is `codex_auto_submit`,
/// which only logs at `warn` and continues, so a single error enum is
/// enough.
fn io_other<E: std::fmt::Display>(e: E) -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::Other, e.to_string())
}

/// Prefer a sibling of the running executable (works for `cargo run` and
/// release layouts). Fall back to the bare name so PATH lookup happens at
/// spawn time if the sibling isn't there.
fn resolve_session_daemon_bin() -> PathBuf {
    if let Ok(exe) = std::env::current_exe()
        && let Some(dir) = exe.parent()
    {
        let candidate = dir.join("calm-session-daemon");
        if candidate.exists() {
            return candidate;
        }
    }
    PathBuf::from("calm-session-daemon")
}

// ---------------------------------------------------------------------------
// CodexClient — owned by Track Codex.
//
// Carries the codex CLI path, the hook bridge path, and the ingest URL.
// The actual spawn lives in `routes::codex::spawn_codex_for`.
// ---------------------------------------------------------------------------

pub struct CodexClient {
    /// `codex` CLI to spawn. Defaults to `codex` (PATH lookup).
    pub codex_bin: String,
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
}

impl CodexClient {
    pub fn new(cfg: &Config) -> Self {
        Self {
            codex_bin: cfg.codex_bin.clone(),
            bridge_bin: cfg
                .codex_bridge_bin
                .clone()
                .unwrap_or_else(resolve_codex_bridge_bin),
            ingest_url: cfg.codex_ingest_url_resolved(),
            codex_homes_dir: cfg.data_dir_resolved().join("codex-homes"),
        }
    }

    /// Test stub — never actually spawns codex; tests that touch the
    /// codex routes don't need a real binary on PATH.
    pub fn new_stub() -> Self {
        Self {
            codex_bin: "codex".into(),
            bridge_bin: PathBuf::from("neige-codex-bridge"),
            ingest_url: "http://127.0.0.1:0".into(),
            codex_homes_dir: std::env::temp_dir().join("neige-codex-homes-stub"),
        }
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
