//! Shared app state passed to every handler.
//!
//! `Clone` is cheap — everything inside is wrapped in `Arc` or already
//! reference-counted internally.

use crate::card_role_cache::CardRoleCache;
use crate::config::Config;
use crate::db::{Repo, RouteRepo};
use crate::dispatcher::Dispatcher;
use crate::event::EventBus;
use crate::event_cursor::EventCursorCache;
use crate::mcp_server::McpServer;
use crate::plugin_host::{PluginHost, PluginRegistry};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

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
    /// UUID v4 minted once per server-process boot, surfaced on
    /// `/api/version` as `dbInstanceId`. Lets the web client detect when the
    /// underlying sqlite DB has been recreated under it (e.g. `make dev
    /// RESET_DB=1` or a fresh-migrations branch swap) and bust its
    /// IndexedDB-backed React Query cache + WS event cursor before they
    /// paint stale ids that 404 at the route loader.
    ///
    /// Deliberately not persisted to the DB: the whole point is that it
    /// changes whenever the DB *might* have changed underneath us. A new
    /// process = a new instance id, full stop. `Arc<String>` so the value
    /// is cheap to clone across handler dispatches.
    pub db_instance_id: Arc<String>,
    /// PR3 (#136) — `CardId -> CardRole` cache used by `role_gate::enforce_role`
    /// at every audited write entry. Clone-cheap (`Arc<DashMap<…>>` inside).
    /// Production builds seed this from the cards table during
    /// [`AppState::new`]; tests construct an empty cache via
    /// [`AppState::from_parts`] when they don't need role-gating coverage,
    /// or pre-populate it manually otherwise. The cache is also threaded
    /// into every `_tx`-suffixed card helper so the insert/delete path
    /// stays write-through inside the surrounding transaction.
    pub card_role_cache: CardRoleCache,
    /// PR8 (#136) — per-card event cursor cache used by
    /// `calm.wait_for_events` MCP tool and `/internal/codex/pending_events`
    /// HTTP fallback. Boot-fresh (empty) on every `AppState` construction;
    /// not persisted — see `crate::event_cursor` module doc for rationale.
    pub event_cursor_cache: EventCursorCache,
    /// PR5 (#136) — dispatcher worker handle. Subscribes via
    /// [`EventBus::subscribe_filtered`] to `*.job_requested` envelopes
    /// and mints worker-roled cards (+ optionally spawns the codex /
    /// session daemon) for each, gated by a global semaphore (default
    /// 8 permits, override via `NEIGE_DISPATCHER_PERMITS`). Held as
    /// `Arc<Dispatcher>` so tests can probe permit counts via
    /// [`Dispatcher::permits`] / [`Dispatcher::semaphore`]; production
    /// callers don't touch the field after construction. Dropping the
    /// `AppState` doesn't immediately abort the dispatcher task —
    /// closure happens when the event bus's `tx` drops too.
    pub dispatcher: Arc<Dispatcher>,
    /// PR7a (#136) — kernel-as-MCP-server handle. Bound to a Unix domain
    /// socket under `<data_dir>/mcp/kernel.sock`; per-card codex daemons
    /// connect through `neige-mcp-stdio-shim` and authenticate via the
    /// per-card token in `card_mcp_tokens`. The handle's `shim_config`
    /// is read by `spec_card::build_codex_config_toml_with_prompt` so
    /// the per-card `$CODEX_HOME/config.toml` carries a matching
    /// `[mcp_servers.calm]` block.
    ///
    /// `Option` because `from_parts` (replay / unit tests) skips the
    /// listener boot — neither the replay binary nor most integration
    /// tests need a live MCP server. The production `AppState::new`
    /// path always populates this.
    pub mcp_server: Option<Arc<McpServer>>,
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
    ///
    /// PR3 (#136): `card_role_cache` defaults to an empty cache when the
    /// caller passes `None`. Tests that exercise role-gating manually
    /// pre-populate the cache via `CardRoleCache::insert` before calling
    /// this; the replay path uses an empty cache because replay events
    /// are seeded via `log_pure_event` from `ActorId::User` (which the
    /// gate lets through without a cache lookup).
    pub fn from_parts(
        repo: Arc<dyn Repo>,
        events: EventBus,
        daemon: Arc<DaemonClient>,
        plugin: Arc<PluginHost>,
        codex: Arc<CodexClient>,
        card_role_cache: Option<CardRoleCache>,
    ) -> Self {
        let route_repo: Arc<dyn RouteRepo> = repo.clone();
        let card_role_cache = card_role_cache.unwrap_or_default();
        // PR5 (#136): every `AppState` carries a live dispatcher. Test
        // call sites that need to assert on dispatcher behavior reach
        // through `state.dispatcher`; the rest see a passive worker
        // that's silent until something emits a `*.job_requested`
        // event. Permit count honors `NEIGE_DISPATCHER_PERMITS` for
        // the rare test that twiddles the env var; the default 8 is
        // the value tests will see otherwise.
        let dispatcher = Arc::new(Dispatcher::spawn(
            repo.clone(),
            events.clone(),
            card_role_cache.clone(),
            codex.clone(),
            daemon.clone(),
            // `from_parts` is the test / replay hatch — no live MCP
            // server. PR7a.1 (#136 followup) added this slot.
            None,
            Dispatcher::permits_from_env(8),
        ));
        Self {
            repo: route_repo,
            events,
            daemon,
            plugin,
            codex,
            // Fresh UUID per `AppState` — same boot-scoped semantics as
            // `AppState::new`. Each integration test gets its own id,
            // which is the right behavior: two tests sharing one binary
            // are conceptually two server "boots".
            db_instance_id: Arc::new(uuid::Uuid::new_v4().to_string()),
            card_role_cache,
            // PR8 (#136) — empty cursor cache. Integration tests that
            // want to assert "second call's `since` defaults to first
            // call's max id" exercise the cache through the wait/pending
            // handlers; `from_parts` callers that need to pre-seed for
            // a fixture can reach the cache through `state.event_cursor_cache`.
            event_cursor_cache: EventCursorCache::new(),
            dispatcher,
            // `from_parts` is the test / replay-lib hatch — no live MCP
            // server. Production goes through `new` below.
            mcp_server: None,
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

        // PR3 (#136) — boot-time role cache. Seed from the cards table
        // *after* migrations have run (which `SqlxRepo::open` did) and
        // *before* any background task is spawned, so the FSM projector
        // / sweeper / plugin host all see the same cache state the first
        // REST write will. Cache is clone-cheap; we stash one clone on
        // `AppState` and hand the FSM/sweeper their own clones —
        // `Arc<DashMap<…>>` under the hood, so it's the same underlying
        // map.
        let card_role_cache = CardRoleCache::new();
        repo.seed_card_role_cache(&card_role_cache).await?;

        // Per-card FSM (phase 1: codex cards only). Subscribes to the bus
        // and projects `codex.hook` events onto a 6-state FSM, writing
        // `Overlay { kind: "status" }` rows for cards and wave-union rows
        // for waves. See `card_fsm` module docs for the scope rationale.
        crate::card_fsm::spawn(repo.clone(), events.clone(), card_role_cache.clone());

        // Share one `DaemonClient` + `CodexClient` between the
        // dispatcher and the `AppState` fields — both are
        // construction-cheap, but a single instance keeps the
        // resolved-binary state consistent (the codex bin path
        // resolution writes its result into the struct, so two
        // instances could diverge if `current_exe()` shifts between
        // calls, which is a no-op today but unnecessary risk).
        let daemon = Arc::new(DaemonClient::new(cfg));
        let codex = Arc::new(CodexClient::new(cfg));

        // PR7a (#136) — boot the kernel-as-MCP-server. Socket lives at
        // `<data_dir>/mcp/kernel.sock`; `neige-mcp-stdio-shim` is the
        // bridge binary the codex daemon launches per session. We
        // build the tool registry now (PR7a registers the three emit
        // tools; PR7b/PR8 will extend) and let `McpServer::spawn`
        // own the listener task. Boot failure surfaces as a hard
        // anyhow error — no MCP server means spec / worker cards
        // can't emit events, which would silently break the wave
        // FSM. The operator deserves a clear boot-time failure.
        //
        // PR7a.1 (#136 followup) — moved up before `Dispatcher::spawn`
        // so the dispatcher can take an `Arc<McpServer>` at construction
        // time and use it for worker codex daemon spawn (mirrors the
        // spec card path in `routes::waves::create_wave`).
        // PR8 (#136) — event cursor cache. Empty at boot; the wait /
        // pending handlers grow it on demand. Cloned into `AppContext`
        // (via `McpServer::spawn`) and stashed on `AppState` so the
        // HTTP fallback (`/internal/codex/pending_events`) sees the
        // same map.
        let event_cursor_cache = EventCursorCache::new();

        let mcp_socket_path = cfg.data_dir_resolved().join("mcp").join("kernel.sock");
        let mcp_shim_bin = resolve_mcp_stdio_shim_bin();
        let mcp_registry = crate::mcp_server::build_default_registry();
        let mcp_server = crate::mcp_server::McpServer::spawn(
            repo.clone(),
            events.clone(),
            card_role_cache.clone(),
            event_cursor_cache.clone(),
            mcp_socket_path,
            mcp_shim_bin,
            mcp_registry,
        )
        .await?;

        // PR5 (#136) — dispatcher worker. Subscribes to
        // `*.job_requested` envelopes and mints worker-roled cards
        // (Cap: `NEIGE_DISPATCHER_PERMITS` env override, default 8).
        // Spawned here (between role-cache seed and plugin autospawn)
        // so the bus has at least one *.Requested-aware listener
        // before plugins start emitting; the role cache is already
        // seeded so the dispatcher's `card_create_with_id_tx` write-
        // through into the cache sees the seeded state.
        let dispatcher = Arc::new(crate::dispatcher::Dispatcher::spawn(
            repo.clone(),
            events.clone(),
            card_role_cache.clone(),
            codex.clone(),
            daemon.clone(),
            // PR7a.1 — hand the MCP server handle to the dispatcher so
            // worker codex spawns can join the same MCP wire the spec
            // card uses.
            Some(mcp_server.clone()),
            crate::dispatcher::Dispatcher::permits_from_env(8),
        ));

        let plugin = Arc::new(PluginHost::new_full(
            Arc::new(registry),
            repo.clone(),
            plugins_dir,
            plugins_data_dir,
            cfg.plugins_disabled.clone(),
            events.clone(),
            card_role_cache.clone(),
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
            codex,
            // See struct doc for `db_instance_id`: one fresh UUID v4 per
            // process boot. `AppState::new` is called exactly once from
            // `main.rs`, so this is the boot-scoped id the rest of the
            // server hands out via `/api/version`.
            db_instance_id: Arc::new(uuid::Uuid::new_v4().to_string()),
            card_role_cache,
            event_cursor_cache,
            dispatcher,
            mcp_server: Some(mcp_server),
            raw: repo,
        };

        // Orphan-terminal sweeper (Scope C). Ticks every 30s, reaps
        // terminal rows that no card references via `payload.terminal_id`
        // (with a 1-minute grace window for the 3-step create race), and
        // emits `Event::TerminalDeleted` through the same `write_with_event`
        // pipeline every other write uses so the cleanup is audited. See
        // `terminal_sweeper` module docs and `docs/sync-engine-design.md` §10.
        crate::terminal_sweeper::spawn(state.clone());

        // Codex hands-free auto-submit subscriber. Watches the bus for
        // `hook.codex.session_start` and, when the originating card has
        // a non-empty `payload.prompt`, injects `\r` to the codex daemon
        // via `DaemonClient::inject_stdin`. Empty / absent prompt → no-op
        // (the user spawned codex without a hands-free prompt).
        crate::codex_auto_submit::spawn(
            state.repo.clone(),
            state.daemon.clone(),
            state.events.clone(),
        );

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

    /// Kernel-private transient stdin injection. Opens the daemon's unix
    /// socket, frames a [`ClientHello`] asserting
    /// `kernel_originated_input = true` so the daemon's owner-only gate
    /// on [`ClientMsg::Input`] is relaxed for this connection, awaits
    /// `ChildReady` if the snapshot says the child is not yet ready,
    /// sends a single [`ClientMsg::Input`] with `input_seq = 1`, and
    /// blocks until the matching [`DaemonMsg::InputAck`] confirms the
    /// PTY write returned.
    ///
    /// The connection is closed (socket dropped) right after the ack
    /// lands — no further frames are sent. The whole call is bounded by
    /// `timeout` (5s in production); on timeout we log at warn and
    /// return `Err(...)`, leaving the daemon untouched. Caller is
    /// expected to degrade (the auto-submit path keeps the codex TUI
    /// alive — the user can hit Enter manually).
    ///
    /// `terminal_id` is normalized to its hyphenated UUID form before
    /// being placed on the handshake — `card.payload.terminal_id`
    /// stores the simple no-dashes form (see `model::new_id`), and the
    /// daemon validates against `Uuid::Display` (always hyphenated). If
    /// the input isn't a valid UUID we pass it through verbatim and let
    /// the daemon reject as `BadHandshake`.
    ///
    /// **Trust model**: this method is the *only* legitimate producer
    /// of `kernel_originated_input = true`. It is reachable only from
    /// in-process kernel code; the WS bridge in `ws/terminal.rs`
    /// strips the bit unconditionally before forwarding any browser
    /// `ClientHello`. See `ClientCapabilities` doc in `calm-session`
    /// for the full trust model.
    pub async fn inject_stdin(
        &self,
        sock_path: &Path,
        terminal_id: &str,
        bytes: &[u8],
        timeout: Duration,
    ) -> anyhow::Result<()> {
        use calm_session::{
            ClientCapabilities, ClientMsg, DaemonMsg, InitialScrollback, PROTOCOL_VERSION, PtySize,
            RenderEncoding, Role, read_frame, write_frame,
        };
        use tokio::net::UnixStream;

        let normalized = match uuid::Uuid::parse_str(terminal_id) {
            Ok(u) => u.to_string(),
            Err(_) => terminal_id.to_string(),
        };

        tokio::time::timeout(timeout, async move {
            let mut stream = UnixStream::connect(sock_path).await.map_err(|e| {
                anyhow::anyhow!(
                    "connect daemon socket {}: {e}",
                    sock_path.display()
                )
            })?;
            let (mut rd, mut wr) = stream.split();

            // ClientHello — Observer role (we are not the user, we are the
            // kernel relaying input). `kernel_originated_input = true`
            // unlocks the daemon's owner-only gate on `Input`. ResizeCommit
            // and Kill remain owner-only even with this bit set — see
            // `ClientCapabilities` doc.
            let hello = ClientMsg::ClientHello {
                protocol_version: PROTOCOL_VERSION,
                terminal_id: normalized,
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
            write_frame(&mut wr, &hello).await?;

            // Read ServerHello and learn whether the child is already
            // ready. If not, wait for the one-shot ChildReady broadcast
            // before injecting bytes — otherwise the input lands while
            // codex is still loading its splash and gets eaten by the
            // composer-init flush.
            let server_hello: DaemonMsg = read_frame(&mut rd).await?;
            let child_ready = match server_hello {
                DaemonMsg::ServerHello {
                    is_child_ready, ..
                } => is_child_ready,
                DaemonMsg::ProtocolError {
                    code,
                    message,
                    expected_version,
                } => {
                    return Err(anyhow::anyhow!(
                        "daemon ProtocolError {code:?}: {message} (expected_version={expected_version:?})"
                    ));
                }
                other => {
                    return Err(anyhow::anyhow!(
                        "expected ServerHello as first frame, got {other:?}"
                    ));
                }
            };

            if !child_ready {
                // Drain frames until ChildReady fires. RenderPatch /
                // RenderSnapshot frames can interleave — skip them.
                loop {
                    let msg: DaemonMsg = read_frame(&mut rd).await?;
                    match msg {
                        DaemonMsg::ChildReady { .. } => break,
                        DaemonMsg::TerminalExited { code, .. } => {
                            return Err(anyhow::anyhow!(
                                "daemon TerminalExited (code={code:?}) before ChildReady"
                            ));
                        }
                        DaemonMsg::ProtocolError {
                            code, message, ..
                        } => {
                            return Err(anyhow::anyhow!(
                                "daemon ProtocolError {code:?}: {message}"
                            ));
                        }
                        _ => continue,
                    }
                }
            }

            // Send the Input. `input_seq = 1` is monotonic for this
            // ephemeral connection (which only ever sends one Input).
            write_frame(
                &mut wr,
                &ClientMsg::Input {
                    data: bytes.to_vec(),
                    input_seq: 1,
                },
            )
            .await?;

            // Await the matching InputAck. The daemon emits acks in
            // PTY-write completion order; since we only sent one Input,
            // the next ack with `input_seq == 1` is ours. Skip any
            // intervening RenderPatch / RenderSnapshot / etc.
            loop {
                let msg: DaemonMsg = read_frame(&mut rd).await?;
                match msg {
                    DaemonMsg::InputAck { input_seq: 1 } => break,
                    DaemonMsg::TerminalExited { code, .. } => {
                        return Err(anyhow::anyhow!(
                            "daemon TerminalExited (code={code:?}) before InputAck"
                        ));
                    }
                    DaemonMsg::ProtocolError {
                        code, message, ..
                    } => {
                        return Err(anyhow::anyhow!(
                            "daemon ProtocolError {code:?}: {message}"
                        ));
                    }
                    _ => continue,
                }
            }

            Ok::<(), anyhow::Error>(())
        })
        .await
        .map_err(|_| {
            anyhow::anyhow!(
                "inject_stdin to {} timed out after {:?}",
                sock_path.display(),
                timeout
            )
        })?
    }
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
// The actual spawn lives in `routes::codex_cards::create_codex_card`.
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

/// PR7a (#136) — resolve the path to `neige-mcp-stdio-shim`. Same
/// "sibling of running exe, else bare-name PATH lookup" pattern as the
/// codex-bridge resolver. The codex daemon will spawn this binary
/// from the path baked into each per-card `$CODEX_HOME/config.toml`'s
/// `[mcp_servers.calm].command` entry.
fn resolve_mcp_stdio_shim_bin() -> PathBuf {
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
