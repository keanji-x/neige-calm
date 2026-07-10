//! Plugin host — the kernel's side of the plugin protocol.
//!
//! This module is sliced across the M3 implementation plan:
//!
//!   * **Slice A** — `manifest` (parser + validator), `registry`
//!     (in-memory map of `id → Manifest`), and `PluginHost` (thin container
//!     that owns the registry + a repo handle).
//!   * **Slice B** — `process` + `mcp` + `error`: child supervision,
//!     JSON-RPC framing, real `spawn`/`stop`/`restart` on `PluginHost`,
//!     crash-loop disabling.
//!   * **Slice C (this commit)** — `callbacks` + `perms` + `events`:
//!     real `neige.*` dispatch. Replaces Slice B's MethodNotFound drainer
//!     with a permission-gated router that writes overlays/cards/kv and
//!     bridges the event bus to MCP notifications.
//!   * **Slice H** — `auth`: per-plugin token mint/verify + iframe tokens.
//!
//! See `docs/m3-design.md` §8 for the full slice table.

pub mod auth;
pub mod callbacks;
pub mod error;
pub mod events;
mod glob;
pub mod manifest;
pub mod mcp;
pub mod perms;
pub mod process;
pub mod registry;
pub mod resources;
pub mod version;
pub mod workflow_input;

use std::collections::{BTreeSet, HashMap};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

pub use auth::{PluginToken, hash_token, verify_token};
pub use error::{HostError, McpError, ProcessError};
pub use manifest::Manifest;
pub use mcp::{
    CallToolResult, ContentBlock, InboundNotification, InboundRequest, McpClient, RequestId,
    ResourceContent, ResourceContents, RpcError,
};
pub use process::PluginProcess;
pub use registry::PluginRegistry;
pub use resources::{ResourceError, read_ui_resource};
pub use version::{KERNEL_VERSION, KernelTooOld, check_min_kernel_version};

use tokio::sync::{Mutex, mpsc};

use crate::db::RouteRepo;
use crate::event::{Event, EventBus, EventScope};
use crate::forge_trust::trusted_forge_plugin;
use crate::ids::ActorId;
use crate::state::WriteContext;

use callbacks::{CallbackCtx, SubscriptionRecord};

// ---------------------------------------------------------------------------
// Tunables
// ---------------------------------------------------------------------------

/// SIGTERM → SIGKILL grace. Design doc §2.4 quotes 500 ms / 5 s; we use a
/// single combined window of 2 s. Most well-behaved plugins exit within tens
/// of ms once they see EOF on stdin or a SIGTERM; 2 s gives slow plugins a
/// fair chance without making the supervisor sluggish.
const STOP_GRACE: Duration = Duration::from_secs(2);

/// Crash-loop window per design doc Slice B header: 5 crashes in 5 minutes
/// disables the plugin until an explicit `spawn(id)` call (which in this slice
/// is the REST `/enable` path; for now also reachable via test).
const CRASH_WINDOW: Duration = Duration::from_secs(300);
const CRASH_WINDOW_LIMIT: u32 = 5;

/// Exponential-backoff schedule for respawn: 1, 2, 4, 8, 30, 30, ...
const BACKOFF_SCHEDULE_MS: &[u64] = &[1_000, 2_000, 4_000, 8_000, 30_000];

// ---------------------------------------------------------------------------
// Runtime status
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PluginRuntimeStatus {
    /// Reserved for Slice D's install flow; included here so the state event
    /// vocabulary is closed.
    Installing,
    Spawning,
    Running,
    /// Crash-looped or otherwise unrecoverable. Carries the latest error.
    Crashed {
        reason: String,
    },
    Disabled,
}

impl PluginRuntimeStatus {
    /// Wire string per design doc §7's `plugin.state` event.
    pub fn wire_name(&self) -> &'static str {
        match self {
            Self::Installing => "installing",
            Self::Spawning => "spawning",
            Self::Running => "running",
            Self::Crashed { .. } => "crashed",
            Self::Disabled => "disabled",
        }
    }

    pub fn last_error(&self) -> Option<&str> {
        match self {
            Self::Crashed { reason } => Some(reason.as_str()),
            _ => None,
        }
    }
}

// ---------------------------------------------------------------------------
// Internal running-plugin record
// ---------------------------------------------------------------------------

struct RunningPlugin {
    process: Arc<PluginProcess>,
    mcp: Arc<McpClient>,
    status: PluginRuntimeStatus,
    /// Lets the supervisor task know it should NOT respawn (graceful stop).
    /// Set true by `stop()` before the wait observation.
    stopping: bool,
    /// Cumulative crash count within the current rolling window.
    crashes_in_window: u32,
    window_started: Instant,
    /// Supervisor task handle. Aborted on graceful stop so we don't leak.
    supervisor: Option<tokio::task::JoinHandle<()>>,
    /// Slice C router task that drains inbound MCP requests and dispatches
    /// them to `callbacks::dispatch`. Held so it dies when `RunningPlugin`
    /// is dropped; also explicitly aborted on `stop()`.
    router: tokio::task::JoinHandle<()>,
    /// Per-plugin subscription registry. `neige.event.subscribe` registers
    /// long-lived bridge tasks here; `stop()` aborts them all before the
    /// process is killed so they don't keep the event bus subscribed past
    /// plugin exit.
    subscriptions: Arc<Mutex<Vec<SubscriptionRecord>>>,
}

// ---------------------------------------------------------------------------
// PluginHost
// ---------------------------------------------------------------------------

/// Per-plugin runtime view exposed to callers (Slice D's REST handlers).
#[derive(Debug, Clone)]
pub struct PluginHostStatus {
    pub id: String,
    pub status: PluginRuntimeStatus,
    pub pid: Option<u32>,
}

pub struct PluginHost {
    pub registry: Arc<PluginRegistry>,
    /// Narrowed (PR #41) from `Arc<dyn Repo>` to `Arc<dyn RouteRepo>` —
    /// the host only does eventized writes + out-of-domain plugin/token/kv
    /// writes + reads. Raw sync-domain writes (`cove_*`, `wave_*`,
    /// `card_*` direct, `overlay_upsert`) are unreachable so a future
    /// contributor can't quietly bypass the audit log inside the host.
    pub(crate) repo: Arc<dyn RouteRepo>,
    /// Resolved per-plugin mutable-state root from `Config::plugins_data_dir_resolved`.
    pub plugins_data_dir: PathBuf,
    /// Resolved plugin install root from `Config::plugins_dir_resolved` — used
    /// as a fallback when the registry didn't capture an install_path (e.g.
    /// in-memory test seeds).
    pub plugins_dir: PathBuf,
    /// Plugin ids the operator has explicitly disabled via config.
    plugins_disabled: Vec<String>,
    /// Live broadcaster for `Event::PluginState`. Kept as an `Option` so test
    /// shims can leave it `None` and skip emissions.
    events: Option<EventBus>,
    /// Same bus, hoisted into an `Arc` so the Slice C router can hand a
    /// shared handle to each plugin's CallbackCtx. When `events` is `None`
    /// (test shims) we still create a private bus here so dispatch keeps
    /// working — emissions just go nowhere visible.
    events_arc: Arc<EventBus>,
    /// #480 PR2 — write-surface caches shared with REST/worker paths.
    write: WriteContext,
    processes: Mutex<HashMap<String, RunningPlugin>>,
}

#[allow(deprecated)]
impl PluginHost {
    /// Real boot-time constructor. Mirrors Slice A's `new`, but takes the
    /// resolved-paths + event bus + config disable list so we can supervise.
    ///
    /// PR3 (#136): also takes the [`CardRoleCache`] from `AppState` so
    /// the host's `log_pure_event` / dispatch paths use the same map as
    /// the REST surface.
    #[allow(clippy::too_many_arguments)]
    pub fn new_full(
        registry: Arc<PluginRegistry>,
        repo: Arc<dyn RouteRepo>,
        plugins_dir: PathBuf,
        plugins_data_dir: PathBuf,
        plugins_disabled: Vec<String>,
        events: EventBus,
        write: WriteContext,
    ) -> Self {
        let events_arc = Arc::new(events.clone());
        Self {
            registry,
            repo,
            plugins_dir,
            plugins_data_dir,
            plugins_disabled,
            events: Some(events),
            events_arc,
            write,
            processes: Mutex::new(HashMap::new()),
        }
    }

    /// Convenience accessor — most call sites only need the registry handle.
    pub fn registry(&self) -> &Arc<PluginRegistry> {
        &self.registry
    }

    pub fn write(&self) -> &WriteContext {
        &self.write
    }

    /// `Arc<EventBus>` handle for the Slice C router. Always returns a real
    /// bus — see field doc for the no-bus-configured case.
    fn events_arc(&self) -> Arc<EventBus> {
        Arc::clone(&self.events_arc)
    }

    /// Mint + persist a fresh process token for `id`, returning the raw value
    /// the caller can put into the spawn env. The hash lands in
    /// `plugin_tokens`; the raw value is **not** kept anywhere persistent —
    /// after this call the kernel only knows the hash. That means a kernel
    /// restart cannot resurrect the old token, so plugins re-handshake with a
    /// fresh one each kernel boot. **This is intentional**: restart is a
    /// security boundary; if the kernel was compromised between boots we want
    /// every plugin to surface a fresh credential anyway.
    pub async fn ensure_plugin_token(&self, id: &str) -> Result<String, HostError> {
        let raw = PluginToken::generate();
        let hashed = hash_token(raw.as_str());
        self.repo
            .plugin_token_set(id, &hashed, i64::MAX)
            .await
            .map_err(|e| HostError::BadState(format!("plugin_token_set({id}): {e}")))?;
        Ok(raw.into_inner())
    }

    /// Forced rotation: delete the existing row + restart the plugin so it
    /// picks up the new token on its next spawn. The actual mint happens
    /// inside `spawn` via `ensure_plugin_token`; we just clear the slot here.
    pub async fn rotate_plugin_token(self: &Arc<Self>, id: &str) -> Result<(), HostError> {
        // Clearing the row first means: even if restart fails mid-flight, the
        // next spawn will mint fresh. Old (raw) token in any plugin's hands is
        // already worthless once the process is killed below.
        let _ = self.repo.plugin_token_delete(id).await;
        self.restart(id).await
    }

    /// Auto-spawn every enabled plugin known to the repo. Called from
    /// `AppState::new` after the host is constructed. Per-plugin failures are
    /// logged + swallowed: one broken plugin should not block boot.
    pub async fn autospawn_enabled(self: &Arc<Self>) {
        let rows = match self.repo.plugins_list_all().await {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!(error = %e, "plugin autospawn: list_all failed");
                return;
            }
        };
        for plug in rows {
            if !plug.enabled {
                continue;
            }
            if let Err(e) = self.spawn(&plug.id).await {
                tracing::warn!(plugin_id = %plug.id, error = %e, "plugin autospawn failed");
            }
        }
    }

    /// Spawn a plugin by id. Returns `Ok(())` once `initialize` has handshaken
    /// and the supervisor task is wired. Errors before that point unwind
    /// without leaving a half-running entry.
    pub async fn spawn(self: &Arc<Self>, id: &str) -> Result<(), HostError> {
        // Disabled-by-config short-circuit.
        if self.plugins_disabled.iter().any(|d| d == id) {
            return Err(HostError::Disabled(id.to_string()));
        }

        // Already running?
        {
            let map = self.processes.lock().await;
            if let Some(rp) = map.get(id) {
                // Crashed→spawn is the recovery path; the supervisor cleared
                // its handle, so we treat that as "go ahead".
                if matches!(
                    rp.status,
                    PluginRuntimeStatus::Running | PluginRuntimeStatus::Spawning
                ) {
                    return Err(HostError::AlreadyRunning(id.to_string()));
                }
            }
        }

        let manifest = self
            .registry
            .get(id)
            .ok_or_else(|| HostError::NotFound(id.to_string()))?;

        // Issue #45: refuse to spawn plugins that demand a newer kernel than
        // we are. Parse failures on `min_kernel_version` already get caught
        // by `Manifest::validate` at load time, so the unwrap-via-parse here
        // is purely for re-hydrating the validated string into a `Version`.
        // We do *not* abort the whole autospawn loop on failure — the caller
        // (`autospawn_enabled`) logs and continues, matching the design's
        // "one bad plugin doesn't block boot" policy.
        let required = semver::Version::parse(&manifest.min_kernel_version).map_err(|e| {
            HostError::BadState(format!(
                "plugin `{id}` has an unparseable min_kernel_version `{}` \
                 (should have been rejected at manifest load): {e}",
                manifest.min_kernel_version
            ))
        })?;
        if let Err(err) = check_min_kernel_version(&KERNEL_VERSION, &required) {
            tracing::warn!(
                plugin_id = %id,
                required = %err.required,
                actual = %err.actual,
                "plugin '{id}' requires kernel >= {}, this kernel is {} — refusing to load",
                err.required,
                err.actual,
            );
            return Err(HostError::KernelTooOld(err));
        }

        // #891 slice ④ — registration-time workflow-id uniqueness: refuse to
        // spawn a trusted plugin whose workflow id another RUNNING trusted
        // plugin already registers. Uniqueness is enforced over the same
        // "running ∧ trusted" set every workflow resolver filters on
        // (`resolve_trusted_workflow`, `bound_workflow`, the MCP per-wave
        // tool scope), so a stopped plugin never squats on a workflow id.
        // Ordered before the token mint below so a refusal — like the
        // min-kernel check above — has zero side effects; the autospawn
        // loop's per-plugin tolerance logs and moves on.
        if let Some(conflict) = find_workflow_conflict(
            &manifest,
            self.registry.list(),
            &self.running_plugin_ids().await,
            &trusted_forge_plugin,
        ) {
            tracing::warn!(
                plugin_id = %id,
                error = %conflict,
                "refusing to spawn plugin with a conflicting workflow id"
            );
            return Err(conflict);
        }

        let install_path = self
            .registry
            .install_path(id)
            .unwrap_or_else(|| self.plugins_dir.join(id));

        // Slice H: mint a fresh process token + persist its hash. The raw value
        // returned here is the same value we pass via env and the same value
        // we'll require the plugin to echo back inside `initialize`.
        //
        // Note: every spawn mints fresh. A prior row in `plugin_tokens` is
        // overwritten — the host doesn't try to "recover" the previous raw
        // (which it can't, by design — see `ensure_plugin_token` docs).
        let token = self.ensure_plugin_token(id).await?;

        self.emit_state(id, &PluginRuntimeStatus::Spawning).await;

        // Spawn the process. On failure we propagate without touching the
        // processes map.
        let process = Arc::new(
            PluginProcess::spawn(&manifest, &install_path, &self.plugins_data_dir, &token)
                .map_err(HostError::from)?,
        );

        // Hand stdio over to the MCP client. The supervisor task picks the
        // `Child` up below for `wait()`.
        let (stdin, stdout) = process
            .take_stdio()
            .ok_or_else(|| HostError::Mcp(McpError::TransportClosed("stdio not piped".into())))?;
        let mcp = match McpClient::connect_with_auth(stdout, stdin, Some(token.as_str())).await {
            Ok(c) => c,
            Err(e) => {
                // Failed handshake — try to clean up the child before bailing.
                if let Some(mut child) = process.take_child() {
                    let _ = child.start_kill();
                }
                // Slice H: an auth-mismatch failure is a security event, not
                // a transient crash. We detect via the marker string the
                // McpClient::initialize path emits, surface a Crashed state
                // event with a clear reason, and crucially do **not** install
                // a supervisor task so no respawn fires. The child has been
                // kill_on_drop-flagged so dropping `process` SIGKILLs it.
                if matches!(&e, McpError::Framing(m) if m == "auth mismatch") {
                    let reason = "auth handshake failed";
                    // Drop any stale processes-map entry so list_running /
                    // status don't report a stale Running state.
                    let _ = self.processes.lock().await.remove(id);
                    self.emit_crashed(id, reason).await;
                    return Err(HostError::AuthMismatch(id.to_string()));
                }
                self.emit_crashed(id, &format!("initialize failed: {e}"))
                    .await;
                return Err(HostError::InitializeRejected(e.to_string()));
            }
        };

        // Slice C / M1: install the real `neige.*` router *iff* the plugin
        // declared the experimental `dev.neige/kernel-callbacks` capability in
        // its initialize response. Without taking the inbound channel here,
        // the bounded mpsc would backpressure as soon as a plugin issued any
        // callback — so we always drain, just with different semantics.
        let inbound = match mcp.take_inbound_requests() {
            Some(rx) => rx,
            None => {
                // Re-entrancy guard: somebody else already took it (unexpected
                // in current code paths). Use an empty channel that closes
                // immediately so the router task exits cleanly.
                let (_tx, rx) = mpsc::channel::<InboundRequest>(1);
                rx
            }
        };
        let inbound_notifs = mcp.take_inbound_notifications();
        let subscriptions: Arc<Mutex<Vec<SubscriptionRecord>>> = Arc::new(Mutex::new(Vec::new()));
        let router = if mcp.has_kernel_callbacks_capability(id) {
            spawn_neige_router(
                id.to_string(),
                Arc::clone(&self.repo),
                self.events_arc(),
                Arc::clone(&self.registry),
                Arc::clone(&mcp),
                Arc::clone(&subscriptions),
                inbound,
                inbound_notifs,
                self.write.clone(),
            )
        } else {
            tracing::info!(
                plugin_id = %id,
                "plugin did not declare experimental.dev.neige/kernel-callbacks; \
                 installing MethodNotFound drainer (neige.* calls will fail)"
            );
            spawn_methodnotfound_drainer(id.to_string(), inbound, inbound_notifs)
        };

        // Supervisor task: waits for the child, restarts on unexpected exit.
        let child_handle = process.take_child().ok_or_else(|| {
            HostError::BadState("PluginProcess lost its Child before supervision".into())
        })?;
        let supervisor = {
            let host = Arc::clone(self);
            let plugin_id = id.to_string();
            tokio::spawn(async move {
                host.supervise(plugin_id, child_handle).await;
            })
        };

        // Park the running record. We preserve any pre-existing crash-window
        // counters (carried by `Crashed → Spawning` recovery paths) so the
        // crash-loop disable threshold counts the actual rate, not just the
        // restarts within one spawn lifetime.
        {
            let mut map = self.processes.lock().await;
            let (crashes_in_window, window_started) = match map.get(id) {
                Some(prev) => (prev.crashes_in_window, prev.window_started),
                None => (0, Instant::now()),
            };
            map.insert(
                id.to_string(),
                RunningPlugin {
                    process: process.clone(),
                    mcp: mcp.clone(),
                    status: PluginRuntimeStatus::Running,
                    stopping: false,
                    crashes_in_window,
                    window_started,
                    supervisor: Some(supervisor),
                    router,
                    subscriptions,
                },
            );
        }

        self.emit_state(id, &PluginRuntimeStatus::Running).await;
        tracing::info!(plugin_id = %id, "plugin running");

        Ok(())
    }

    /// Gracefully stop a plugin. Sets `stopping=true` so the supervisor task
    /// won't respawn, sends SIGTERM via PluginProcess::stop, awaits exit.
    pub async fn stop(self: &Arc<Self>, id: &str) -> Result<(), HostError> {
        let (process, supervisor, subs) = {
            let mut map = self.processes.lock().await;
            let rp = map
                .get_mut(id)
                .ok_or_else(|| HostError::NotFound(id.to_string()))?;
            if rp.stopping {
                return Err(HostError::BadState(format!("{id} is already stopping")));
            }
            rp.stopping = true;
            let process = Arc::clone(&rp.process);
            let supervisor = rp.supervisor.take();
            let subs = Arc::clone(&rp.subscriptions);
            // Abort the router so it doesn't race the channel-close on
            // mcp drop. The handle itself stays in the struct until we
            // remove() below; abort() is idempotent and we don't await.
            rp.router.abort();
            (process, supervisor, subs)
        };

        // Abort every active `neige.event.subscribe` bridge task. Holding
        // these past process exit would leak event-bus subscribers.
        {
            let mut s = subs.lock().await;
            for rec in s.drain(..) {
                rec.task.abort();
            }
        }

        // Abort the supervisor *before* we kill the process so it doesn't
        // race us into a respawn attempt.
        if let Some(h) = supervisor {
            h.abort();
        }
        match process.stop(STOP_GRACE).await {
            Ok(_status) => {}
            Err(ProcessError::AlreadyDead) => {
                // Supervisor was already going to react to this. Fine.
            }
            Err(e) => {
                return Err(HostError::Spawn(e));
            }
        }

        {
            let mut map = self.processes.lock().await;
            map.remove(id);
        }

        self.emit_state(id, &PluginRuntimeStatus::Disabled).await;
        Ok(())
    }

    /// Stop then spawn. Returns the spawn error if either half fails.
    pub async fn restart(self: &Arc<Self>, id: &str) -> Result<(), HostError> {
        // Stop is best-effort: if it returns NotFound (e.g. already crashed
        // and cleaned up), we proceed to spawn.
        match self.stop(id).await {
            Ok(()) | Err(HostError::NotFound(_)) => {}
            Err(e) => return Err(e),
        }
        self.spawn(id).await
    }

    /// Snapshot current status for one plugin.
    pub async fn status(&self, id: &str) -> Option<PluginHostStatus> {
        let map = self.processes.lock().await;
        map.get(id).map(|rp| PluginHostStatus {
            id: id.to_string(),
            status: rp.status.clone(),
            pid: rp.process.pid(),
        })
    }

    /// Snapshot the full table — used by the REST `GET /api/plugins` handler
    /// once Slice D wires it.
    pub async fn list_running(&self) -> Vec<PluginHostStatus> {
        let map = self.processes.lock().await;
        map.iter()
            .map(|(id, rp)| PluginHostStatus {
                id: id.clone(),
                status: rp.status.clone(),
                pid: rp.process.pid(),
            })
            .collect()
    }

    /// Snapshot ids that are currently running.
    pub async fn running_plugin_ids(&self) -> BTreeSet<String> {
        let map = self.processes.lock().await;
        map.iter()
            .filter(|(_, rp)| matches!(rp.status, PluginRuntimeStatus::Running))
            .map(|(id, _)| id.clone())
            .collect()
    }

    /// Most-recent stderr lines, oldest → newest. `n` clamps to the ring
    /// capacity inside `PluginProcess`.
    pub async fn stderr_tail(&self, id: &str, n: usize) -> Option<Vec<String>> {
        let map = self.processes.lock().await;
        map.get(id).map(|rp| rp.process.stderr_tail(n))
    }

    /// Borrow the live MCP client. Slice C calls this to issue `tools/list`
    /// or to drive other outbound RPC.
    pub async fn mcp_client(&self, id: &str) -> Option<Arc<McpClient>> {
        let map = self.processes.lock().await;
        map.get(id)
            .filter(|rp| matches!(rp.status, PluginRuntimeStatus::Running))
            .map(|rp| Arc::clone(&rp.mcp))
    }

    /// Dispatch a `neige.*` callback method against the in-kernel handler,
    /// using the same `CallbackCtx` the plugin's inbound MCP router builds.
    ///
    /// M5: this is the host-fan-out the AppBridge `tools/call` route in
    /// `routes::plugins::tool_call` hits when an iframe issues
    /// `app.callServerTool({ name: "neige.overlay.set", ... })`. The route
    /// already enforces the `neige.*` prefix per migration doc §7.6 row 5;
    /// the plugin process is never asked.
    ///
    /// `call_id` is the optional caller-supplied tracing handle from
    /// `ToolCallBody.call_id`. When set, every event the dispatch writes
    /// lands in `events.correlation` as `user_tool_call:<call_id>`. The
    /// plugin's inbound MCP router (which calls `callbacks::dispatch`
    /// directly, not via this method) passes `None` — plugin-initiated
    /// writes don't carry user-facing tracing yet.
    ///
    /// Returns `RpcError::Custom(-32002, ...)` if the plugin isn't currently
    /// running.
    pub async fn dispatch_neige_callback(
        &self,
        plugin_id: &str,
        method: &str,
        params: serde_json::Value,
        call_id: Option<&str>,
    ) -> Result<serde_json::Value, RpcError> {
        let (mcp, subscriptions) = {
            let map = self.processes.lock().await;
            let rp = map
                .get(plugin_id)
                .ok_or_else(|| RpcError::custom(-32002, "plugin not running"))?;
            if !matches!(rp.status, PluginRuntimeStatus::Running) {
                return Err(RpcError::custom(-32002, "plugin not running"));
            }
            (Arc::clone(&rp.mcp), Arc::clone(&rp.subscriptions))
        };

        let ctx = CallbackCtx {
            plugin_id,
            repo: Arc::clone(&self.repo),
            event_bus: self.events_arc(),
            registry: Arc::clone(&self.registry),
            mcp,
            subscriptions,
            call_id,
            write: self.write.clone(),
        };
        callbacks::dispatch(&ctx, method, params).await
    }

    // ----- internals -----

    /// Persist a `plugin.state` event and broadcast it. Goes through
    /// `Repo::log_pure_event` so every fired event lands in the events
    /// table with a real `_id`; the bus broadcast is fired only after
    /// commit succeeds (commit-then-emit invariant).
    async fn emit_state(&self, id: &str, status: &PluginRuntimeStatus) {
        if let Some(bus) = &self.events {
            let event = Event::PluginState {
                id: id.to_string(),
                state: status.wire_name().to_string(),
                last_error: status.last_error().map(String::from),
            };
            // PR2 of #136: `ActorId::Plugin(id)` typed; `EventScope::System`
            // because `Event::PluginState` is a server-lifecycle signal with
            // no entity (cove/wave/card) scope.
            if let Err(e) = self
                .repo
                .log_pure_event(
                    ActorId::Plugin(id.to_string()),
                    EventScope::System,
                    None,
                    bus,
                    self.write.role_cache(),
                    self.write.cove_cache(),
                    event,
                )
                .await
            {
                tracing::warn!(plugin_id = %id, error = %e, "plugin_state event log failed");
            }
        }
    }

    async fn emit_crashed(&self, id: &str, reason: &str) {
        let status = PluginRuntimeStatus::Crashed {
            reason: reason.to_string(),
        };
        self.emit_state(id, &status).await;
    }

    /// Supervisor loop for one plugin: awaits child exit, classifies as
    /// graceful vs crash, applies backoff + crash-loop disabling.
    ///
    /// Running as `Arc<Self>` lets us re-enter `spawn` after a crash. The
    /// return is boxed because `supervise` ↔ `spawn` form a mutual recursion
    /// through `tokio::spawn`; auto-Send inference can't see through that
    /// cycle, so we erase one side via `Pin<Box<dyn Future + Send>>`.
    fn supervise(
        self: Arc<Self>,
        id: String,
        child: tokio::process::Child,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>> {
        Box::pin(self.supervise_inner(id, child))
    }

    async fn supervise_inner(self: Arc<Self>, id: String, mut child: tokio::process::Child) {
        let exit_result = child.wait().await;
        // Was this a graceful stop? Look at the map; if `stopping=true`, yes.
        let stopping = {
            let map = self.processes.lock().await;
            map.get(&id).map(|rp| rp.stopping).unwrap_or(true)
        };

        if stopping {
            tracing::info!(plugin_id = %id, "plugin exited gracefully");
            return;
        }

        let reason = match exit_result {
            Ok(status) => format!("exited with {status}"),
            Err(e) => format!("wait failed: {e}"),
        };
        tracing::warn!(plugin_id = %id, reason = %reason, "plugin exited unexpectedly");

        // Snapshot stderr tail so the crash event carries useful detail.
        let tail = {
            let map = self.processes.lock().await;
            map.get(&id)
                .map(|rp| rp.process.stderr_tail(10).join("\n"))
                .unwrap_or_default()
        };
        let combined_reason = if tail.is_empty() {
            reason
        } else {
            format!("{reason}\nstderr tail:\n{tail}")
        };

        // Crash-window bookkeeping.
        let (attempts, exceeded) = {
            let mut map = self.processes.lock().await;
            let entry = match map.get_mut(&id) {
                Some(e) => e,
                None => {
                    // Was removed by `stop()` — nothing to do.
                    return;
                }
            };
            if entry.window_started.elapsed() > CRASH_WINDOW {
                entry.window_started = Instant::now();
                entry.crashes_in_window = 0;
            }
            entry.crashes_in_window += 1;
            entry.status = PluginRuntimeStatus::Crashed {
                reason: combined_reason.clone(),
            };
            (
                entry.crashes_in_window,
                entry.crashes_in_window >= CRASH_WINDOW_LIMIT,
            )
        };

        self.emit_crashed(&id, &combined_reason).await;

        if exceeded {
            tracing::error!(
                plugin_id = %id,
                attempts,
                "plugin exceeded crash-window limit; not respawning",
            );
            // Leave the Crashed entry in place so `status()` returns it. The
            // supervisor task ends here; an explicit `spawn(id)` revives.
            // We do, however, remove the process arc so its file descriptors
            // (already-closed pipes mostly) get reaped.
            let mut map = self.processes.lock().await;
            if let Some(rp) = map.get_mut(&id) {
                rp.supervisor = None;
            }
            return;
        }

        // Backoff then respawn. Index by (attempts - 1) clamped to the table.
        let idx = (attempts as usize).saturating_sub(1);
        let delay_ms = BACKOFF_SCHEDULE_MS
            .get(idx)
            .copied()
            .unwrap_or(*BACKOFF_SCHEDULE_MS.last().unwrap());
        tracing::info!(
            plugin_id = %id,
            delay_ms,
            attempts,
            "scheduling plugin respawn",
        );
        tokio::time::sleep(Duration::from_millis(delay_ms)).await;

        // Drop the old entry's process/mcp before respawning so the channels
        // close before we open new ones.
        {
            let mut map = self.processes.lock().await;
            map.remove(&id);
        }
        if let Err(e) = self.spawn(&id).await {
            tracing::error!(plugin_id = %id, error = %e, "respawn failed");
            self.emit_crashed(&id, &format!("respawn failed: {e}"))
                .await;
        }
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// #891 slice ④ — pure core of the registration-time workflow-id uniqueness
/// check `PluginHost::spawn` runs. Returns the [`HostError::WorkflowConflict`]
/// for the first workflow id of `manifest` that another **running trusted**
/// candidate manifest already declares; `None` when the spawn may proceed.
///
/// Rules (design §4.4):
/// * only fires when the spawning plugin itself is trusted — untrusted
///   plugins never enter the workflow resolution set, so their (unreachable)
///   duplicate ids are tolerated;
/// * only running ∧ trusted candidates count — a stopped plugin does not
///   hold its workflow ids;
/// * the spawning plugin's own registry entry is skipped (respawn path).
///
/// The trust predicate is injected because the trusted set is
/// env-configured (`NEIGE_TRUSTED_FORGE_PLUGINS`), which keeps this core
/// unit-testable without mutating process env.
fn find_workflow_conflict(
    manifest: &Manifest,
    candidates: impl IntoIterator<Item = Manifest>,
    running_ids: &BTreeSet<String>,
    is_trusted: &dyn Fn(&str) -> bool,
) -> Option<HostError> {
    if !is_trusted(&manifest.id) {
        return None;
    }
    for other in candidates {
        if other.id == manifest.id || !running_ids.contains(&other.id) || !is_trusted(&other.id) {
            continue;
        }
        for workflow in &manifest.workflows {
            if other.workflows.iter().any(|held| held.id == workflow.id) {
                return Some(HostError::WorkflowConflict {
                    plugin_id: manifest.id.clone(),
                    workflow_id: workflow.id.clone(),
                    held_by: other.id.clone(),
                });
            }
        }
    }
    None
}

/// Slice C router: drains the inbound MCP request channel and dispatches each
/// `neige.*` call to `callbacks::dispatch`. Also drains the notification
/// channel — currently log-and-drop, since the design doc reserves
/// `notifications/cancelled` and other side-channels for later use.
///
/// One task per plugin process. Ends when both channels close (plugin exited).
#[allow(clippy::too_many_arguments)]
fn spawn_neige_router(
    plugin_id: String,
    repo: Arc<dyn RouteRepo>,
    event_bus: Arc<EventBus>,
    registry: Arc<PluginRegistry>,
    mcp: Arc<McpClient>,
    subscriptions: Arc<Mutex<Vec<SubscriptionRecord>>>,
    mut inbound: mpsc::Receiver<InboundRequest>,
    inbound_notifs: Option<mpsc::Receiver<InboundNotification>>,
    write: WriteContext,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        // Drain notifications in a separate task — they're lossy by spec and
        // we don't yet act on any specific notification method, but logging
        // is useful for debugging plugin behaviour. We hold the JoinHandle
        // implicitly (it dies when this outer task exits).
        if let Some(mut notif_rx) = inbound_notifs {
            let plugin_id_n = plugin_id.clone();
            tokio::spawn(async move {
                while let Some(notif) = notif_rx.recv().await {
                    tracing::debug!(
                        plugin_id = %plugin_id_n,
                        method = %notif.method,
                        "inbound plugin notification (currently logged + ignored)"
                    );
                }
            });
        }

        while let Some(req) = inbound.recv().await {
            let ctx = CallbackCtx {
                plugin_id: &plugin_id,
                repo: Arc::clone(&repo),
                event_bus: Arc::clone(&event_bus),
                registry: Arc::clone(&registry),
                mcp: Arc::clone(&mcp),
                subscriptions: Arc::clone(&subscriptions),
                // Plugin-initiated inbound requests have no caller-side
                // tracing id (the route layer is where `call_id` enters);
                // resulting event rows get `correlation = NULL`.
                call_id: None,
                write: write.clone(),
            };
            let outcome = callbacks::dispatch(&ctx, &req.method, req.params).await;
            // If the responder is gone (plugin disconnected mid-call), drop
            // silently — the mcp reader already cleans up the wire.
            let _ = req.responder.send(outcome);
        }
        tracing::debug!(plugin_id = %plugin_id, "inbound request channel closed");
    })
}

/// M1 gate: when a plugin omits the `experimental.dev.neige/kernel-callbacks`
/// capability, the kernel installs this drainer in place of the dispatcher.
/// Every inbound request is answered with `MethodNotFound`, so a plugin that
/// later tries `neige.overlay.set` gets a clean -32601 instead of a hang. This
/// matches Slice B's pre-Slice-C behaviour and keeps the wire sane for plugins
/// that only need outbound `tools/call`.
fn spawn_methodnotfound_drainer(
    plugin_id: String,
    mut inbound: mpsc::Receiver<InboundRequest>,
    inbound_notifs: Option<mpsc::Receiver<InboundNotification>>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        if let Some(mut notif_rx) = inbound_notifs {
            let plugin_id_n = plugin_id.clone();
            tokio::spawn(async move {
                while let Some(notif) = notif_rx.recv().await {
                    tracing::debug!(
                        plugin_id = %plugin_id_n,
                        method = %notif.method,
                        "inbound plugin notification (no-callbacks plugin; logged + ignored)"
                    );
                }
            });
        }
        while let Some(req) = inbound.recv().await {
            let outcome = Err(RpcError::method_not_found(&req.method));
            let _ = req.responder.send(outcome);
        }
        tracing::debug!(plugin_id = %plugin_id, "inbound request channel closed (no-callbacks)");
    })
}

#[cfg(test)]
mod workflow_conflict_tests {
    use super::*;

    fn manifest_with_workflow(id: &str, workflow_id: &str) -> Manifest {
        let json = serde_json::json!({
            "manifest_version": 1,
            "id": id,
            "version": "0.1.0",
            "min_kernel_version": "0.0.1",
            "display_name": "Workflow Conflict Stub",
            "entrypoint": { "command": "bin/stub" },
            "workflows": [
                {
                    "id": workflow_id,
                    "plan_template": [],
                    "gates": [],
                    "spec_instructions": "",
                    "card_kinds": []
                }
            ],
            "permissions": {}
        });
        Manifest::parse(&json.to_string()).expect("manifest parses")
    }

    fn running(ids: &[&str]) -> BTreeSet<String> {
        ids.iter().map(|id| id.to_string()).collect()
    }

    #[test]
    fn duplicate_workflow_on_running_trusted_plugin_conflicts() {
        let incoming = manifest_with_workflow("dev.second", "issue-development");
        let holder = manifest_with_workflow("dev.first", "issue-development");
        let trusted = |_: &str| true;
        let conflict =
            find_workflow_conflict(&incoming, [holder], &running(&["dev.first"]), &trusted)
                .expect("duplicate workflow id must conflict");
        match conflict {
            HostError::WorkflowConflict {
                plugin_id,
                workflow_id,
                held_by,
            } => {
                assert_eq!(plugin_id, "dev.second");
                assert_eq!(workflow_id, "issue-development");
                assert_eq!(held_by, "dev.first");
            }
            other => panic!("expected WorkflowConflict, got {other:?}"),
        }
    }

    #[test]
    fn stopped_holder_does_not_squat_on_workflow_id() {
        let incoming = manifest_with_workflow("dev.second", "issue-development");
        let holder = manifest_with_workflow("dev.first", "issue-development");
        let trusted = |_: &str| true;
        assert!(
            find_workflow_conflict(&incoming, [holder], &running(&[]), &trusted).is_none(),
            "a stopped plugin must not hold the workflow id"
        );
    }

    #[test]
    fn untrusted_duplicates_are_tolerated() {
        let incoming = manifest_with_workflow("dev.second", "issue-development");
        let holder = manifest_with_workflow("dev.first", "issue-development");
        let running_ids = running(&["dev.first"]);

        // Untrusted spawner: never enters the resolution set — no conflict.
        let only_first_trusted = |id: &str| id == "dev.first";
        assert!(
            find_workflow_conflict(
                &incoming,
                [holder.clone()],
                &running_ids,
                &only_first_trusted
            )
            .is_none()
        );

        // Untrusted holder: its workflows are unresolvable — no conflict.
        let only_second_trusted = |id: &str| id == "dev.second";
        assert!(
            find_workflow_conflict(&incoming, [holder], &running_ids, &only_second_trusted)
                .is_none()
        );
    }

    #[test]
    fn respawn_skips_own_registry_entry_and_distinct_ids_pass() {
        let incoming = manifest_with_workflow("dev.first", "issue-development");
        let own_entry = manifest_with_workflow("dev.first", "issue-development");
        let trusted = |_: &str| true;
        assert!(
            find_workflow_conflict(&incoming, [own_entry], &running(&["dev.first"]), &trusted)
                .is_none(),
            "respawn must not conflict with the plugin's own registry entry"
        );

        let other = manifest_with_workflow("dev.other", "different-workflow");
        assert!(
            find_workflow_conflict(&incoming, [other], &running(&["dev.other"]), &trusted)
                .is_none(),
            "distinct workflow ids must not conflict"
        );
    }
}
