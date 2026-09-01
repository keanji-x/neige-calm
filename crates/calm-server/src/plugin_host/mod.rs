//! Plugin host — the kernel's side of the plugin protocol.
//!
//! It owns manifests, process supervision, MCP transport, callbacks,
//! permissions, resources, events, and plugin authentication.

pub mod auth;
pub mod callbacks;
pub mod connector;
pub mod error;
pub mod events;
mod glob;
pub mod http_mcp;
pub mod lifecycle;
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
pub use connector::{ConnectorClient, SecretsError, read_secrets};
pub use error::{HostError, McpError, ProcessError};
pub use http_mcp::{HttpCredential, HttpMcpClient};
pub use manifest::{ConnectorKind, Manifest};
pub use mcp::{
    CallToolResult, ContentBlock, InboundNotification, InboundRequest, McpClient, RequestId,
    ResourceContent, ResourceContents, RpcError,
};
pub use process::PluginProcess;
pub use registry::{PluginRegistry, PluginRegistryBuilder};
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
    /// #1164 §2.2 / §2.3 — a **connector** could not be brought up: the remote
    /// MCP host timed out or errored, the CLI could not be resolved on PATH,
    /// or `secrets.json` was refused. Distinct from [`Self::Crashed`] because
    /// nothing crashed and there is no supervisor: connectors have no child
    /// process, so there is no automatic retry. Recovery is an operator
    /// re-enable.
    Unavailable {
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
            Self::Unavailable { .. } => "unavailable",
            Self::Disabled => "disabled",
        }
    }

    pub fn last_error(&self) -> Option<&str> {
        match self {
            Self::Crashed { reason } | Self::Unavailable { reason } => Some(reason.as_str()),
            _ => None,
        }
    }
}

// ---------------------------------------------------------------------------
// Internal running-plugin record
// ---------------------------------------------------------------------------

struct RunningPlugin {
    /// #1164 §2.5 — `None` for connectors: `mcp-http` and `cli-query` have no
    /// kernel-supervised child. Every reader (`status`, `list_running`,
    /// `stderr_tail`, the crash tail) now goes through `as_ref()`.
    process: Option<Arc<PluginProcess>>,
    /// Field name deliberately kept as `mcp`; the TYPE widened from
    /// `Arc<McpClient>` to the [`ConnectorClient`] union (§2.5 / D8), and then
    /// to `Option<…>` so a **failed connector** can hold a live table entry.
    /// `None` means exactly one thing: this entry exists to make a terminal
    /// [`PluginRuntimeStatus::Unavailable`] observable via `status` / `list` /
    /// `GET /api/plugins/{id}`; there is nothing to call.
    mcp: Option<ConnectorClient>,
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
    /// #1164 §2.5 — `None` for connectors: the kernel builds no inbound
    /// `neige.*` router for them (and `dispatch_neige_callback` refuses any
    /// non-`Stdio` client), so there is nothing to drain or abort.
    router: Option<tokio::task::JoinHandle<()>>,
    /// Per-plugin subscription registry. `neige.event.subscribe` registers
    /// long-lived bridge tasks here; `stop()` aborts them all before the
    /// process is killed so they don't keep the event bus subscribed past
    /// plugin exit.
    subscriptions: Arc<Mutex<Vec<SubscriptionRecord>>>,
}

// ---------------------------------------------------------------------------
// Process table (live processes + spawn admission reservations)
// ---------------------------------------------------------------------------

/// Everything `PluginHost` knows about plugin runtime state, guarded by ONE
/// mutex so admission decisions are atomic.
///
/// #891 review fix (spawn TOCTOU): the workflow-uniqueness check and the
/// "already running" check used to read a Running-only snapshot with no lock
/// held across the spawn, while the `live` insert happened only after process
/// exec + MCP handshake. Two concurrent spawns of trusted plugins declaring
/// the same workflow id could both pass, yielding duplicate running owners
/// and a nondeterministic `plugin_scope_for_wave` winner. Concurrent callers
/// are real: HTTP enable/reload routes plus the crash-supervisor respawn.
///
/// `spawning` is the admission set: an id is inserted here — under the same
/// lock where the conflict check reads state — the moment its spawn is
/// admitted, and counts as a workflow-id holder for every later admission
/// until it is either swapped for a `live` entry (success, same lock) or
/// released (any failure between admission and the swap). The existing
/// [`PluginRuntimeStatus::Spawning`] state is reused to report reserved ids
/// via `status`/`list_running`, so no new status vocabulary is introduced.
/// The table is guarded by a **`std::sync::Mutex`**, not a tokio one: every
/// critical section is short, synchronous, and never held across an `.await`
/// (verified per site), which is exactly the case where tokio's own docs
/// recommend the std mutex. The synchronous lock is what makes
/// [`AdmissionGuard`]'s `Drop` able to release a reservation on task
/// abort/panic without needing an async context.
#[derive(Default)]
struct ProcessTable {
    live: HashMap<String, RunningPlugin>,
    spawning: BTreeSet<String>,
}

/// RAII admission reservation (#891 r2 review fix). Constructed under the
/// admission lock the moment `spawn` inserts its `spawning` reservation, and
/// held across the whole `spawn_admitted` future. Release paths:
///
/// * **success** — the atomic reservation→live swap removes the reservation
///   itself and calls [`AdmissionGuard::disarm`] under the same lock, so a
///   later `Drop` is a no-op (and can never release a *newer* reservation
///   for the same id);
/// * **everything else** — `Err` returns, the calling task being
///   aborted/dropped mid-`.await`, and panic unwinds all run `Drop`, which
///   synchronously re-locks the table and removes the reservation. Without
///   this, a cancelled spawn would leave the id `Spawning` forever: same-id
///   spawns would get `AlreadyRunning` and the reserved workflow ids would
///   squat indefinitely.
///
/// Deadlock safety: `Drop` only locks when still armed, and the only place a
/// guard is consumed while the table lock is held is `disarm` — which flips
/// the flag without locking, so its own `Drop` skips the lock. No code path
/// drops an *armed* guard while holding the table lock.
struct AdmissionGuard {
    host: Arc<PluginHost>,
    id: String,
    armed: bool,
}

impl AdmissionGuard {
    fn new(host: Arc<PluginHost>, id: String) -> Self {
        Self {
            host,
            id,
            armed: true,
        }
    }

    /// Consume the guard without releasing the reservation — called ONLY
    /// inside the success path's reservation→live swap (which removes the
    /// reservation itself, under the same lock).
    fn disarm(mut self) {
        self.armed = false;
    }
}

impl Drop for AdmissionGuard {
    fn drop(&mut self) {
        if self.armed {
            self.host.lock_table().spawning.remove(&self.id);
        }
    }
}

impl ProcessTable {
    /// Ids that hold their manifests' workflow ids for admission purposes:
    /// live plugins that are actually `Running`, plus admission-reserved
    /// (`Spawning`) ids. Crashed/stopping entries do not squat on ids —
    /// same policy as [`PluginHost::running_plugin_ids`].
    fn workflow_holder_ids(&self) -> BTreeSet<String> {
        let mut ids: BTreeSet<String> = self
            .live
            .iter()
            .filter(|(_, rp)| matches!(rp.status, PluginRuntimeStatus::Running))
            .map(|(id, _)| id.clone())
            .collect();
        ids.extend(self.spawning.iter().cloned());
        ids
    }
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

/// #1196 S0a — the crash-window / respawn-backoff knobs, lifted out of module
/// constants into an injectable value. `Default` reproduces today's constants
/// byte-for-byte, so a host built without [`PluginHost::with_backoff_schedule`]
/// behaves exactly as before.
#[derive(Debug, Clone)]
pub struct BackoffConfig {
    /// Respawn delays indexed by `attempts - 1`, clamped to the last entry.
    pub schedule_ms: Vec<u64>,
    /// Sliding window over which crashes are counted.
    pub crash_window: Duration,
    /// Crashes within `crash_window` that stop the respawn loop.
    pub crash_window_limit: u32,
}

impl Default for BackoffConfig {
    fn default() -> Self {
        Self {
            schedule_ms: BACKOFF_SCHEDULE_MS.to_vec(),
            crash_window: CRASH_WINDOW,
            crash_window_limit: CRASH_WINDOW_LIMIT,
        }
    }
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
    /// See [`ProcessTable`] for why this is a std (not tokio) mutex.
    processes: std::sync::Mutex<ProcessTable>,
    /// #1164 §2.7(1) — recorded order of the two connector-spawn steps whose
    /// RELATIVE ORDER is the invariant. See [`Self::connector_spawn_order`].
    spawn_order: std::sync::Mutex<HashMap<String, ConnectorSpawnOrder>>,
    /// Per-plugin-id serialization of "decide what the state is" with "emit
    /// it". **Every** emission takes it, because [`Self::emit_state`] is the
    /// only way to emit and [`Self::emit_state`] is the thing that acquires it.
    ///
    /// The process table alone cannot do this. A read of `live` is a SNAPSHOT:
    /// [`Self::reaffirm_running`] takes the table lock, sees `Running`, drops
    /// the lock, and only then awaits an emission. In that window a concurrent
    /// [`Self::stop`] can remove the entry and emit `Disabled` — leaving the
    /// persisted and broadcast state falsely `Running` for a connector that is
    /// gone. Holding a *sync* mutex across those awaits is not an option
    /// (emission writes to the repo), hence async ones.
    ///
    /// **Round-5: the lock moved from the two call sites into the emitter.**
    /// It used to be taken by `reaffirm_running` and `stop` only, which left
    /// three emitters outside it — `spawn_mcp_http`'s trailing `Running`, the
    /// app-plugin spawn's `Running`, and `publish_unavailable`'s `Unavailable`.
    /// Any of those could start, `stop` could then remove the entry and persist
    /// `Disabled` under the lock, and the older unserialized emission would
    /// commit last: byte-for-byte the bug `reaffirm_running` was added to fix,
    /// reachable from three other doors. A rule that every future emitter must
    /// remember to obey is not an invariant; owning the lock inside the emitter
    /// is. The two decide-and-emit paths take the guard themselves and hand it
    /// down ([`Self::emit_state_under`]), which is also why the guard is a
    /// value and not an implicit re-entrant lock.
    ///
    /// Keyed per id: two different plugins have nothing to serialize, and one
    /// global lock would put every plugin's boot emission behind one repo write.
    /// The map only ever grows by installed-plugin count.
    ///
    /// **Lock order is one-way and unchanged: `state_emit` → `processes`.**
    /// Nothing takes the (sync) process-table mutex and then awaits an
    /// emission — every call site drops its `MutexGuard` before awaiting, which
    /// the compiler enforces for the `Send` futures in this module — so the two
    /// locks cannot form a cycle.
    state_emit: std::sync::Mutex<HashMap<String, Arc<StateEmitCell>>>,
    /// Instrumentation for the invariant above: the high-water mark of
    /// emissions inside the critical section **for a single id** at once. `1`
    /// is the invariant; two different plugins emitting at once is fine and
    /// deliberately does not register here.
    ///
    /// Production cost is two atomics per emission. The point is that
    /// `two_emitters_for_one_connector_never_interleave` can observe the real
    /// production emitter rather than a fixture that re-implements it: remove
    /// the lock from `emit_state` and the mark reaches 2, failing that test.
    state_emit_peak: std::sync::atomic::AtomicUsize,
    /// #1196 S0a — crash-loop / respawn-backoff tunables, defaulted to the
    /// [`CRASH_WINDOW`] / [`CRASH_WINDOW_LIMIT`] / [`BACKOFF_SCHEDULE_MS`]
    /// constants and overridable via [`Self::with_backoff_schedule`].
    ///
    /// They are fields rather than constants because the crash-window test
    /// that #1196 S1 owes (acceptance 13) lives in `tests/` — an external
    /// crate, for which the lib's `#[cfg(test)]` is invisible. A builder was
    /// chosen over extra `new_full` parameters deliberately: `new_full` has
    /// ~106 call sites in this crate and none of them should have to move.
    backoff: BackoffConfig,
}

/// Per-id emission lock plus its concurrency probe.
struct StateEmitCell {
    lock: Arc<Mutex<()>>,
    inflight: std::sync::atomic::AtomicUsize,
}

/// Proof that the caller holds the per-id emission lock for `id`.
///
/// Only [`PluginHost::lock_state`] constructs one, so a function that takes one
/// cannot be called without the lock — which is what makes "every emission is
/// serialized per id" a property of the types rather than of reviewer memory.
pub struct StateEmitGuard {
    id: String,
    cell: Arc<StateEmitCell>,
    _held: tokio::sync::OwnedMutexGuard<()>,
}

/// Decrements a cell's in-flight count however the emission ends (including a
/// cancelled future — the caller's outer `timeout` can drop us mid-write).
struct InflightGuard(Arc<StateEmitCell>);

impl Drop for InflightGuard {
    fn drop(&mut self) {
        self.0
            .inflight
            .fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
    }
}

/// Round trips `connect_mcp_http` makes: `initialize` then `tools/list`. The
/// outer bring-up timeout is this multiple of the per-request budget, because
/// `mcp_http.bringup_timeout_ms` configures ONE request, not the whole spawn.
const MCP_HTTP_ROUND_TRIPS: u32 = 2;

/// Headroom on top of `MCP_HTTP_ROUND_TRIPS × request_timeout_ms` for the work
/// that sits outside ureq's own clock — chiefly `spawn_blocking` queue delay on
/// a busy boot, plus tokio scheduling jitter around the two `.await`s.
///
/// Deliberately a FIXED amount rather than a multiplier: the thing it pays for
/// does not scale with how long the operator is willing to wait for one
/// request. Keeping it small also keeps the bound meaningful for the short
/// timeouts tests configure.
const CONNECTOR_BRINGUP_SLACK: Duration = Duration::from_millis(500);

/// Total wall-clock the *connector* portion of [`PluginHost::autospawn_enabled`]
/// may consume, across ALL connectors.
///
/// The per-connector bound in `spawn_mcp_http` caps ONE bring-up; autospawn
/// iterates serially and `AppState::new` awaits it inline, so without a bound
/// spanning the loop, N unreachable connectors still stall boot by N × that cap.
/// This is the only construct that makes boot latency independent of how many
/// dead connectors are installed. Connectors that do not get their turn inside
/// the budget land `Unavailable` with a reason that says so — they are not
/// silently skipped, and they are not detached from boot either: acceptance §4
/// #7 requires materialization to have happened before the boot audit loop
/// reads `exposes_tools`, so bring-up must remain inline.
///
/// **Floor is enforced by construction, not by choosing a big number.** A
/// constant floor alone guaranteed that a connector whose own cap exceeded it
/// was cut off by the LOOP budget at boot — blaming "earlier connectors" that
/// need not exist — while `POST /enable`, which has no loop budget, brought the
/// same connector up against its own cap. Boot and enable disagreed about the
/// same manifest. [`PluginHost::autospawn_enabled_within`] therefore widens this
/// value to [`connector_bringup_budget`]'s largest value over the enabled
/// connectors (plus [`CONNECTOR_BRINGUP_SLACK`], so the per-connector bound is
/// always the one that fires first).
///
/// **And that widening is itself bounded, which is what makes boot latency an
/// invariant rather than a hope.** It was not always: while one manifest field
/// governed both bring-up and `tools/call`, `"request_timeout_ms": 600000`
/// widened this budget to 20.5 minutes — the server did not serve for that
/// long, at the operator's unwitting discretion. The bring-up budget now has
/// its own field with a ceiling validated at manifest parse time
/// ([`manifest::MCP_HTTP_MAX_BRINGUP_TIMEOUT_MS`]), so
/// [`MAX_CONNECTOR_BRINGUP_BUDGET`] caps `widest` for EVERY manifest that can
/// load, and the widened loop budget can never exceed
/// `max(30 s, MAX_CONNECTOR_BRINGUP_BUDGET + slack)`.
pub const CONNECTOR_AUTOSPAWN_BUDGET: Duration = Duration::from_secs(30);

/// The loop budget [`PluginHost::autospawn_enabled_within`] actually adopts,
/// given the one it was handed and the widest per-connector bring-up cap among
/// the connectors it is about to iterate.
///
/// A supplied budget is a *floor*, never a ceiling: see
/// [`CONNECTOR_AUTOSPAWN_BUDGET`] for why the widening exists. This is a
/// function rather than an inline `max` in the loop because the widening is half
/// of the boot bound and a test that wants to state the bound must be able to
/// compute it from the same expression production evaluates — restating
/// `max(supplied, widest + slack)` in a test is a second arithmetic, and a
/// second arithmetic is exactly how `a_slow_event_store_cannot_hold_boot_past_
/// the_phase_ceiling` came to assert a 1.5 s ceiling against a loop that was
/// really running to 1.9 s.
///
/// `const` so that [`MAX_CONNECTOR_AUTOSPAWN_WALL`] can be *this* function
/// applied to the widest loadable inputs rather than a second copy of the same
/// `max` inlined in a const block — which is what it used to be, leaving the
/// helper unpinned by that constant's test.
pub const fn widened_connector_budget(supplied: Duration, widest_bringup: Duration) -> Duration {
    let widened = Duration::from_millis(
        widest_bringup.as_millis() as u64 + CONNECTOR_BRINGUP_SLACK.as_millis() as u64,
    );
    // `Ord::max` is not const; compare the raw nanos and return the *original*
    // `supplied` so no precision is lost on the floor side.
    if widened.as_nanos() > supplied.as_nanos() {
        widened
    } else {
        supplied
    }
}

/// Wall-clock allowed for everything the connector phase does **besides**
/// bringing connectors up: the terminal `Unavailable` emission for connectors
/// that never got their turn, and the reconcile emission for one that came up
/// just as the budget ran out. All of those are persisted+broadcast events, so
/// their cost is a slow event store's cost, not this process's.
///
/// It is a budget for the whole tail rather than a per-connector allowance on
/// purpose: a per-connector one is `N ×` again, which is the shape this whole
/// bound exists to remove.
const CONNECTOR_RECONCILE_BUDGET: Duration = Duration::from_millis(500);

/// **The** wall-clock ceiling on the connector phase of boot, given the loop
/// budget it runs with — spawn, reconcile and every emission inside it.
///
/// One expression, so the number that is *documented* and the number that is
/// *enforced* cannot drift: [`MAX_CONNECTOR_AUTOSPAWN_WALL`] is this function
/// applied to the widest budget any loadable manifest can produce, and
/// [`PluginHost::autospawn_enabled_within`] fences its loop with this function
/// applied to the budget it actually got. Rounds 1-4 each stated a ceiling in
/// prose (30 s, then 30.5 s) while the code computed a different one, because
/// the prose was a separate arithmetic.
pub const fn connector_phase_ceiling(loop_budget: Duration) -> Duration {
    Duration::from_millis(
        loop_budget.as_millis() as u64 + CONNECTOR_RECONCILE_BUDGET.as_millis() as u64,
    )
}

/// The largest wall-clock the connector phase of boot can consume, for **any**
/// set of manifests that load.
///
/// `autospawn_enabled` starts from [`CONNECTOR_AUTOSPAWN_BUDGET`] and widens it
/// to the widest per-connector cap plus slack (see there); that widening is
/// capped by [`MAX_CONNECTOR_BRINGUP_BUDGET`], which manifest-parse-time
/// validation makes structural. This is the composition of the two, and it is
/// what "boot latency is an invariant" means numerically. With today's
/// constants: `(2 × 15 s + 500 ms) + 500 ms` of widened loop budget, `+ 500 ms`
/// of reconcile tail = **31.5 s** — computed here, never retyped, and asserted
/// against the real loop by `the_connector_phase_ceiling_is_the_documented_one`.
pub const MAX_CONNECTOR_AUTOSPAWN_WALL: Duration = connector_phase_ceiling(
    widened_connector_budget(CONNECTOR_AUTOSPAWN_BUDGET, MAX_CONNECTOR_BRINGUP_BUDGET),
);

/// The largest value [`connector_bringup_budget`] can return for any manifest
/// that passes `Manifest::validate`.
///
/// This is the constant that makes the boot bound structural. Asserted against
/// the real function in the manifest-driven test suite; if the formula or the
/// ceiling moves without this following, that test fails.
pub const MAX_CONNECTOR_BRINGUP_BUDGET: Duration = Duration::from_millis(
    manifest::MCP_HTTP_MAX_BRINGUP_TIMEOUT_MS * MCP_HTTP_ROUND_TRIPS as u64
        + CONNECTOR_BRINGUP_SLACK.as_millis() as u64,
);

/// The wall-clock cap on ONE connector's bring-up — the single source of the
/// formula.
///
/// `spawn_mcp_http` bounds an individual spawn with it and
/// `autospawn_enabled_within` widens the loop budget by it; if the two computed
/// it separately they could drift apart, which is exactly the boot-vs-enable
/// disagreement described on [`CONNECTOR_AUTOSPAWN_BUDGET`].
///
/// A connector kind with no network bring-up (there is none in this slice —
/// `cli-query` has no enable path yet) costs only the slack.
///
/// Reads `bringup_timeout_ms`, **never** `request_timeout_ms`: the latter is
/// the (uncapped) `tools/call` budget and has no business on the boot path.
/// Because the former is capped at manifest parse time, the result is bounded
/// by [`MAX_CONNECTOR_BRINGUP_BUDGET`] for every manifest that can load.
pub fn connector_bringup_budget(manifest: &Manifest) -> Duration {
    match manifest.mcp_http.as_ref() {
        Some(block) => {
            Duration::from_millis(block.bringup_timeout_ms()) * MCP_HTTP_ROUND_TRIPS
                + CONNECTOR_BRINGUP_SLACK
        }
        None => CONNECTOR_BRINGUP_SLACK,
    }
}

/// Process-global monotonic tick behind [`ConnectorSpawnOrder`]. A single
/// counter (rather than per-host) keeps the comparison meaningful even when a
/// test drives two hosts over the same plugin dir.
static SPAWN_ORDER_TICK: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

enum SpawnOrderStep {
    Materialized,
    LiveInserted,
}

/// When each half of the §2.7(1) pair happened, on the process-global tick.
///
/// This exists because the ordering is otherwise **unobservable**: the two
/// production steps are adjacent synchronous blocks with no `.await` between
/// them, so no concurrent observer — however fast it samples — can ever be
/// scheduled in the gap. A sampling test therefore passes just as green when
/// the two blocks are swapped, which makes it not a test of the ordering at
/// all. The recorded ticks are a structural witness instead: swap the blocks
/// and the ticks swap with them.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ConnectorSpawnOrder {
    /// Tick at which the tool catalog reached the registry.
    pub materialized_at: Option<u64>,
    /// Tick at which the id became visible as `Running`.
    pub live_inserted_at: Option<u64>,
}

impl ConnectorSpawnOrder {
    /// `true` iff materialization is recorded strictly before the live insert.
    /// Missing either half is NOT ok — an unrecorded step means the spawn did
    /// not reach that point.
    pub fn materialized_before_live_insert(&self) -> bool {
        match (self.materialized_at, self.live_inserted_at) {
            (Some(m), Some(l)) => m < l,
            _ => false,
        }
    }
}

/// The network half of [`PluginHost::spawn_mcp_http`], factored out so ONE
/// `tokio::time::timeout` can bound all of it. Returns the operator-facing
/// reason string on failure (already scrubbed of the API key by
/// [`HttpMcpClient`]).
async fn connect_mcp_http(
    id: &str,
    block: &manifest::McpHttpBlock,
    install_path: PathBuf,
) -> Result<(Arc<HttpMcpClient>, Vec<serde_json::Value>), String> {
    // §2.4 — a wrongly-permissioned secrets file refuses the enable outright.
    // Failing open here would mean an operator never learns their API key is
    // world-readable.
    let secrets = connector::read_secrets(&install_path)
        .await
        .map_err(|e| format!("secrets.json rejected: {e}"))?
        .unwrap_or_default();

    // THE point at which a secret becomes an HTTP credential — and therefore
    // the point at which the HTTP-redaction constraints apply. They are carried
    // by `HttpCredential`, the type `HttpMcpClient::new` takes, so this is not
    // "remember to validate here": there is no way to build the client without
    // one. `read_secrets` deliberately does not apply them, since a `cli-query`
    // secret is on no redaction path.
    let api_key = match block.api_key_secret.as_deref() {
        Some(name) => {
            let raw = secrets.get(name).cloned().ok_or_else(|| {
                format!(
                    "mcp_http.api_key_secret names `{name}`, which is absent from {}",
                    install_path.join(connector::SECRETS_FILENAME).display()
                )
            })?;
            // The reason names the rule, never the value: it is persisted and
            // broadcast as `PluginState.last_error`.
            Some(HttpCredential::parse(&raw).map_err(|why| {
                format!(
                    "the credential in {} named by mcp_http.api_key_secret (`{name}`) {why}",
                    install_path.join(connector::SECRETS_FILENAME).display()
                )
            })?)
        }
        None => None,
    };

    let client = Arc::new(HttpMcpClient::new(id, block, api_key.as_ref()));

    // Best-effort: the probed server class answers `tools/list` with no
    // handshake at all, so an `initialize` failure is informational. It shares
    // the caller's single budget rather than owning one of its own.
    if let Err(e) = client.initialize().await {
        tracing::info!(
            plugin_id = %id,
            target = %client.log_target(),
            error = %e,
            "mcp-http connector did not answer initialize; continuing to tools/list"
        );
    }

    let upstream = client
        .tools_list()
        .await
        .map_err(|e| format!("tools/list against {} failed: {e}", client.log_target()))?;
    Ok((client, upstream))
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
            processes: std::sync::Mutex::new(ProcessTable::default()),
            spawn_order: std::sync::Mutex::new(HashMap::new()),
            state_emit: std::sync::Mutex::new(HashMap::new()),
            state_emit_peak: std::sync::atomic::AtomicUsize::new(0),
            backoff: BackoffConfig::default(),
        }
    }

    /// #1196 S0a — post-construction override of the crash-window / respawn
    /// backoff tunables. Deliberately a builder rather than `new_full`
    /// parameters: `new_full` has ~106 call sites in this crate, and none of
    /// them need to know about this seam.
    ///
    /// `schedule_ms` must be non-empty — the respawn path indexes it and falls
    /// back to its last element.
    #[must_use]
    pub fn with_backoff_schedule(
        mut self,
        schedule_ms: Vec<u64>,
        crash_window: Duration,
        crash_window_limit: u32,
    ) -> Self {
        assert!(
            !schedule_ms.is_empty(),
            "backoff schedule must have at least one entry"
        );
        self.backoff = BackoffConfig {
            schedule_ms,
            crash_window,
            crash_window_limit,
        };
        self
    }

    /// Convenience accessor — most call sites only need the registry handle.
    pub fn registry(&self) -> &Arc<PluginRegistry> {
        &self.registry
    }

    /// #1196 S0a — **runtime** registry write, routed through the host.
    ///
    /// [`PluginRegistry::insert`] is `pub(in crate::plugin_host)`, so *every*
    /// write that happens after a host exists comes through here: the three
    /// lifecycle routes (`install` / `uninstall` / `reload`) as well as
    /// integration tests that mutate a running registry. S1 gives this method a
    /// `&LifecycleGuard` parameter — which is precisely why the routes must
    /// already be on it — today it is a pure pass-through and changes no
    /// behavior.
    pub fn registry_insert(&self, manifest: manifest::Manifest, install_path: Option<PathBuf>) {
        self.registry.insert(manifest, install_path);
    }

    /// #1196 S0a — **runtime** registry removal. See [`Self::registry_insert`].
    pub fn registry_remove(&self, id: &str) -> Option<manifest::Manifest> {
        self.registry.remove(id)
    }

    /// Lock the process table. Poison recovery via `into_inner`: the guarded
    /// critical sections are short and allocation-only, so a panic mid-hold
    /// leaves the table structurally sound; recovering (instead of
    /// propagating) matters because [`AdmissionGuard::drop`] must be able to
    /// release a reservation during a panic unwind without double-panicking.
    fn lock_table(&self) -> std::sync::MutexGuard<'_, ProcessTable> {
        self.processes
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
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
        // #1164 §2.5 — refuse for connectors BEFORE the delete and BEFORE the
        // restart. Connectors never had a token minted (the kind branch in
        // `spawn_admitted` precedes `ensure_plugin_token`), so "rotating" one
        // would be a no-op delete followed by a very real stop+respawn of a
        // healthy connector. The ordering is the whole point of this guard.
        //
        // **Fail closed when the kind is unknown.** An earlier shape made the
        // guard conditional on `registry.get(id)` returning `Some`, so an
        // absent registry entry fell straight through to the delete + restart —
        // i.e. the one case where we cannot prove the plugin is an `app` was
        // also the case that got the side effects. There is no legitimate
        // rotation for an id the registry does not know: `spawn` itself starts
        // with the same lookup and returns `NotFound`, so a rotation could not
        // have restarted anything either way.
        let Some(manifest) = self.registry.get(id) else {
            return Err(HostError::NotFound(id.to_string()));
        };
        if !manifest.kind.is_app() {
            return Err(HostError::UnsupportedForKind {
                plugin_id: id.to_string(),
                kind: manifest.kind.wire_name(),
                operation: "token rotation (connectors are never issued a plugin token)",
            });
        }
        // Clearing the row first means: even if restart fails mid-flight, the
        // next spawn will mint fresh. Old (raw) token in any plugin's hands is
        // already worthless once the process is killed below.
        let _ = self.repo.plugin_token_delete(id).await;
        self.restart(id).await
    }

    /// Auto-spawn every enabled plugin known to the repo. Called from
    /// `AppState::new` after the host is constructed. Per-plugin failures are
    /// logged + swallowed: one broken plugin should not block boot.
    ///
    /// **Connector bring-up stays inline and stays serial**, but the connector
    /// portion as a whole is bounded by
    /// `connector_phase_ceiling(CONNECTOR_AUTOSPAWN_BUDGET)` — spawn,
    /// reconciliation and every persisted emission, not just the spawn step.
    /// [`MAX_CONNECTOR_AUTOSPAWN_WALL`] is what that can be at its widest. Inline
    /// is not an accident: acceptance §4 #7 requires a connector's tools to be
    /// materialized before the boot audit loop in `AppState::new` reads
    /// `exposes_tools`, so detaching bring-up into a background task would make
    /// that read race the materialization and silently lose the connector's
    /// `PluginToolRegistered` events. Bounding the loop is what makes boot
    /// latency independent of the number of unreachable connectors; the
    /// per-connector timeout alone only made it N × one connector's cap.
    pub async fn autospawn_enabled(self: &Arc<Self>) {
        self.autospawn_enabled_within(CONNECTOR_AUTOSPAWN_BUDGET)
            .await;
    }

    /// [`Self::autospawn_enabled`] with the connector budget supplied.
    ///
    /// Exists so a test can drive the REAL loop against a budget small enough
    /// to observe it firing; production always goes through
    /// `autospawn_enabled`, which supplies [`CONNECTOR_AUTOSPAWN_BUDGET`].
    /// Bounding a 30 s budget from the outside would otherwise mean a 30 s
    /// test, and a test that only asserts "under 30 s" cannot tell the loop
    /// bound from the per-connector one.
    pub async fn autospawn_enabled_within(self: &Arc<Self>, connector_budget: Duration) {
        let rows = match self.repo.plugins_list_all().await {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!(error = %e, "plugin autospawn: list_all failed");
                return;
            }
        };
        // The budget must never be smaller than a single connector's own cap,
        // or a lone connector with a large `bringup_timeout_ms` is cut off by
        // the LOOP bound at boot and comes up fine through `POST /enable` —
        // two different answers for one manifest, with a reason that blames
        // earlier connectors that need not exist. See
        // [`CONNECTOR_AUTOSPAWN_BUDGET`]. The extra slack keeps the
        // per-connector bound the one that fires first, so the operator-facing
        // reason names the connector's own timeout rather than the budget.
        //
        // This widening is bounded: `connector_bringup_budget` cannot exceed
        // `MAX_CONNECTOR_BRINGUP_BUDGET`, because the field it reads has a
        // ceiling validated at manifest parse time. That is what stops an
        // operator-supplied number from turning boot into a 20-minute stall.
        let widest = rows
            .iter()
            .filter(|p| p.enabled)
            .filter_map(|p| self.registry.get(&p.id))
            .filter(|m| !m.kind.is_app())
            .map(|m| connector_bringup_budget(&m))
            .max()
            .unwrap_or_default();
        let connector_budget = widened_connector_budget(connector_budget, widest);

        // CONNECTOR-only elapsed time. An `Instant` taken before the loop also
        // charges every `app` plugin's spawn to this budget, so a slow local
        // child ahead of a connector in `plugins_list_all()` order silently
        // consumed it — and the refusal then said the budget "was spent by
        // earlier connectors", which was false.
        let ceiling = connector_phase_ceiling(connector_budget);
        let mut connector_elapsed = Duration::ZERO;
        for plug in rows {
            if !plug.enabled {
                continue;
            }
            // `app` plugins spawn a local child and are not network-bound, so
            // they are outside this budget — it exists for the remote half.
            let is_connector = self
                .registry
                .get(&plug.id)
                .is_some_and(|m| !m.kind.is_app());
            if !is_connector {
                if let Err(e) = self.spawn(&plug.id).await {
                    tracing::warn!(plugin_id = %plug.id, error = %e, "plugin autospawn failed");
                }
                continue;
            }

            // ---- THE bound. ------------------------------------------------
            // One fence over the WHOLE per-connector iteration — bring-up,
            // reconciliation, and every persisted emission either of them
            // performs — rather than over the `spawn` sub-step alone.
            //
            // Bounding only `spawn` left two unbounded awaits after it
            // (`publish_unavailable`, `reaffirm_running`) plus an unbounded
            // emission on the budget-exhausted path, each of which writes an
            // event. A slow event store therefore still held boot for as long
            // as it liked, N times over — the bound existed but was not TOTAL.
            //
            // Because the fence wraps the iteration body rather than a named
            // step inside it, an await added to that body later is covered
            // without anyone remembering to cover it.
            let started = Instant::now();
            let spawn_deadline = started + connector_budget.saturating_sub(connector_elapsed);
            let phase_deadline = started + ceiling.saturating_sub(connector_elapsed);
            let fenced = tokio::time::timeout_at(
                tokio::time::Instant::from_std(phase_deadline),
                self.autospawn_one_connector(&plug.id, spawn_deadline, connector_budget),
            )
            .await;
            // Every connector iteration is charged, and only connector
            // iterations are: an `app` plugin's slow local child ahead of a
            // connector must not consume the remote half's budget.
            connector_elapsed += started.elapsed();
            if fenced.is_err() {
                // The whole connector phase is out of wall clock. Still leave a
                // terminal, observable entry — but with NO await, because an
                // await here is the very thing that ran us out. The table write
                // is a `HashMap::insert` under a sync lock; the event that would
                // normally accompany it is what we are giving up.
                let reason = format!(
                    "connector `{}` was cut off at boot: the connector phase's {} ms \
                     wall-clock ceiling was reached (a slow or unreachable connector, \
                     or a slow event store). Re-enable it once that is fixed.",
                    plug.id,
                    ceiling.as_millis()
                );
                tracing::warn!(plugin_id = %plug.id, "{reason}");
                self.mark_unavailable(&plug.id, None, reason);
            }
        }
    }

    /// One connector's boot iteration: bring it up within `spawn_deadline`, then
    /// reconcile whatever that produced. Every await in here — including the
    /// terminal emissions — is inside the caller's phase fence.
    async fn autospawn_one_connector(
        self: &Arc<Self>,
        id: &str,
        spawn_deadline: Instant,
        connector_budget: Duration,
    ) {
        let remaining = spawn_deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            let reason = format!(
                "connector `{id}` was not brought up at boot: the {} ms budget for \
                 starting all connectors was already spent by earlier ones. \
                 Re-enable it once the slow/unreachable connectors are fixed.",
                connector_budget.as_millis()
            );
            tracing::warn!(plugin_id = %id, "{reason}");
            // Terminal + observable, exactly like every other connector
            // bring-up failure: `status`/`list`/detail must not report this
            // id as if it had never been enabled.
            // Same reconciliation as the timeout arm below: never regress
            // an id that is already live and `Running`.
            let _ = self.publish_unavailable(id, None, reason).await;
            return;
        }
        let outcome = tokio::time::timeout_at(
            tokio::time::Instant::from_std(spawn_deadline),
            self.spawn(id),
        )
        .await;
        match outcome {
            Ok(Ok(())) => {}
            Ok(Err(e)) => {
                tracing::warn!(plugin_id = %id, error = %e, "plugin autospawn failed");
            }
            Err(_elapsed) => {
                // The dropped `spawn` future released its admission
                // reservation via `AdmissionGuard::drop`, but nothing
                // emitted a terminal state — do it here so the event log
                // does not sit at `spawning` forever.
                //
                // **This arm must RECONCILE, not assume failure.** The
                // timeout wraps the whole spawn, including
                // `spawn_mcp_http`'s live-table insert; elapsing after that
                // insert but before the trailing `emit_state(Running)`
                // completes would otherwise replace a genuinely healthy
                // connector (client live, tools materialized) with
                // `Unavailable`, dropping it out of `running_plugin_ids`
                // and making every materialized tool invisible.
                // `publish_unavailable` refuses to regress a `Running`
                // entry and says so by returning `false`.
                let reason = format!(
                    "connector `{id}` did not finish starting within the remaining \
                     {} ms of the {} ms boot budget for all connectors",
                    remaining.as_millis(),
                    connector_budget.as_millis()
                );
                if self.publish_unavailable(id, None, reason.clone()).await {
                    tracing::warn!(plugin_id = %id, "{reason}");
                } else {
                    tracing::info!(
                        plugin_id = %id,
                        "connector boot budget elapsed after it had already come up; \
                         keeping it Running"
                    );
                    // The dropped future never reached its own
                    // `emit_state(Running)`, so the event log would
                    // otherwise sit at `spawning` for a live connector.
                    //
                    // NOT `emit_state(Running)` directly: `publish_unavailable`
                    // returning `false` is a SNAPSHOT taken under the table
                    // lock and then released. `reaffirm_running` re-decides
                    // and emits as one serialized step, so a `stop()` that
                    // lands in the window cannot be overwritten by this
                    // stale `Running`.
                    self.reaffirm_running(id).await;
                }
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

        // #891 slice ④ (+ review fix) — atomic admission. Under ONE lock on
        // the process table we (a) refuse a spawn that's already running or
        // already admitted, (b) run the registration-time workflow-id
        // uniqueness check against running ∧ admitted holders, and (c) on
        // success reserve the id in the admission set. This closes the
        // check-to-insert TOCTOU: a concurrent spawn (HTTP enable/reload,
        // crash-supervisor respawn) observes either our reservation or our
        // live entry, never the in-between. Uniqueness is enforced over the
        // same "running ∧ trusted" set every workflow resolver filters on
        // (`resolve_trusted_workflow`, `bound_workflow`, the MCP per-wave
        // tool scope) — plus admission reservations — so a stopped plugin
        // never squats on a workflow id but a mid-spawn one already holds it.
        // Ordered before the token mint so a refusal — like the min-kernel
        // check above — has zero side effects on plugin state; the autospawn
        // loop's per-plugin tolerance logs and moves on.
        let admission = {
            let mut table = self.lock_table();
            if table.spawning.contains(id) {
                return Err(HostError::AlreadyRunning(id.to_string()));
            }
            if let Some(rp) = table.live.get(id)
                && matches!(
                    rp.status,
                    PluginRuntimeStatus::Running | PluginRuntimeStatus::Spawning
                )
            {
                // Crashed→spawn is the recovery path; the supervisor cleared
                // its handle, so we treat that as "go ahead".
                return Err(HostError::AlreadyRunning(id.to_string()));
            }
            match find_workflow_conflict(
                &manifest,
                self.registry.list(),
                &table.workflow_holder_ids(),
                &trusted_forge_plugin,
            ) {
                Some(conflict) => Err(conflict),
                None => {
                    table.spawning.insert(id.to_string());
                    // r2 review fix: the reservation's lifetime is owned by
                    // this RAII guard, not by manual bookkeeping — an `Err`
                    // return, a task abort/drop at ANY `.await` inside
                    // `spawn_admitted`, or a panic unwind all release it via
                    // `Drop`; only the success path's atomic swap disarms.
                    Ok(AdmissionGuard::new(Arc::clone(self), id.to_string()))
                }
            }
        };
        let guard = match admission {
            Ok(guard) => guard,
            Err(conflict) => {
                tracing::warn!(
                    plugin_id = %id,
                    error = %conflict,
                    "refusing to spawn plugin with a conflicting workflow id"
                );
                // #891 review fix (design §4.4 "该插件进 Failed"): surface the
                // refusal as a failed `PluginState` event so operators see WHY
                // the plugin isn't running instead of it silently looking
                // stopped. Boot-loop tolerance is unchanged: autospawn logs and
                // continues; the enable route maps this to a structured 409.
                self.emit_crashed(id, &conflict.to_string()).await;
                return Err(conflict);
            }
        };

        self.spawn_admitted(id, &manifest, guard).await
    }

    /// Everything downstream of a successful admission reservation: token
    /// mint, process exec, MCP handshake, router + supervisor wiring, and
    /// the final swap of the reservation for the live `Running` entry (one
    /// lock). Owns the [`AdmissionGuard`]: every failure exit — `Err`
    /// return, task abort/drop at any `.await`, panic — drops the guard,
    /// which releases the reservation; the success swap disarms it.
    async fn spawn_admitted(
        self: &Arc<Self>,
        id: &str,
        manifest: &Manifest,
        guard: AdmissionGuard,
    ) -> Result<(), HostError> {
        let install_path = self
            .registry
            .install_path(id)
            .unwrap_or_else(|| self.plugins_dir.join(id));

        // #1164 §1.4 + §2.6 — branch by kind BEFORE `ensure_plugin_token()`.
        // The ordering is the point: minting a token for a remote HTTP server
        // or a query CLI would write a `plugin_tokens` row for a credential
        // nobody will ever present, and `rotate-token` would then look like a
        // meaningful operation on it.
        match manifest.kind {
            ConnectorKind::App => {}
            ConnectorKind::McpHttp => {
                return self
                    .spawn_mcp_http(id, manifest, &install_path, guard)
                    .await;
            }
            ConnectorKind::CliQuery => {
                // Parse + install are in place (see `Manifest::validate`);
                // resolving/pinning the binary and executing it is the next
                // slice. Reported through the SAME channel as every other
                // connector bring-up failure — `ConnectorUnavailable`, hence a
                // 503 — rather than `BadState`, which `spawn_error_to_calm`
                // has no arm for and would render as a kernel-fault 500 for a
                // plain "not implemented yet".
                return self
                    .connector_unavailable(
                        id,
                        guard,
                        format!(
                            "cli-query connector `{id}` cannot be enabled yet: \
                             the execution runtime is not implemented in this slice (#1164 P3)"
                        ),
                    )
                    .await;
            }
        }

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
        // live map (the caller releases the admission reservation).
        let process = Arc::new(
            PluginProcess::spawn(manifest, &install_path, &self.plugins_data_dir, &token)
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
                    // Drop any stale live entry so list_running / status
                    // don't report a stale Running state. (The admission
                    // reservation is released by the guard's Drop on this
                    // Err return.)
                    let _ = self.lock_table().live.remove(id);
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

        // Park the running record, atomically swapping the admission
        // reservation for the live entry so no interleaving ever sees
        // "neither reserved nor running". The guard is disarmed under the
        // SAME lock: after this block a same-id respawn may legitimately
        // create a new reservation, and a still-armed guard dropped later
        // would wrongly release it. We preserve any pre-existing
        // crash-window counters (carried by `Crashed → Spawning` recovery
        // paths) so the crash-loop disable threshold counts the actual rate,
        // not just the restarts within one spawn lifetime.
        {
            let mut table = self.lock_table();
            let (crashes_in_window, window_started) = match table.live.get(id) {
                Some(prev) => (prev.crashes_in_window, prev.window_started),
                None => (0, Instant::now()),
            };
            table.spawning.remove(id);
            guard.disarm();
            table.live.insert(
                id.to_string(),
                RunningPlugin {
                    process: Some(process.clone()),
                    // App plugins are ALWAYS `Stdio`. `mcp_client()`'s
                    // narrowed `(Running, Stdio)` match (§2.6 / D12) is only
                    // safe because of this — if a future arm parks a
                    // non-`Stdio` client for an `app`, every forge / card /
                    // callback path would start reporting "not running".
                    mcp: Some(ConnectorClient::Stdio(mcp.clone())),
                    status: PluginRuntimeStatus::Running,
                    stopping: false,
                    crashes_in_window,
                    window_started,
                    supervisor: Some(supervisor),
                    router: Some(router),
                    subscriptions,
                },
            );
        }

        self.emit_state(id, &PluginRuntimeStatus::Running).await;
        tracing::info!(plugin_id = %id, "plugin running");

        Ok(())
    }

    /// `kind: mcp-http` spawn arm (§2.2).
    ///
    /// No token, no process, no router, no supervisor. What it DOES do:
    /// read `secrets.json`, build the client, run a best-effort `initialize`,
    /// fetch + filter `tools/list`, **materialize the tool catalog into the
    /// registry**, and only then publish the live `Running` entry.
    ///
    /// The materialize-before-publish order is §2.7(1) and is load-bearing:
    /// `running_plugin_ids` gates tool discovery AND the boot audit loop in
    /// `AppState::new`, both of which read `manifest.exposes_tools`. Publish
    /// Running first and there is a window where the id is visible with an
    /// empty tool list — silently, and until the next restart.
    async fn spawn_mcp_http(
        self: &Arc<Self>,
        id: &str,
        manifest: &Manifest,
        install_path: &std::path::Path,
        guard: AdmissionGuard,
    ) -> Result<(), HostError> {
        let block = manifest.mcp_http.as_ref().ok_or_else(|| {
            HostError::BadState(format!(
                "plugin `{id}` is kind mcp-http but has no mcp_http block"
            ))
        })?;

        // §2.7(1) — this spawn's ordering witness starts empty. Without the
        // reset, a spawn that materialized and then failed before the live
        // insert leaves a half-filled entry behind, and the NEXT spawn's fresh
        // `materialized_at` would be read alongside the previous attempt's
        // stale `live_inserted_at` — a pair that never happened, and one that
        // can compare "correct" by accident.
        self.spawn_order
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(id);

        self.emit_state(id, &PluginRuntimeStatus::Spawning).await;

        // ONE outer wall-clock bound over the WHOLE bring-up (§2.2).
        //
        // `mcp_http.bringup_timeout_ms` is a PER-REQUEST budget, and
        // `connect_mcp_http` makes two round trips (`initialize`, then
        // `tools/list`). Setting the outer bound to exactly one request's worth
        // therefore condemned a healthy-but-slow upstream — or one that merely
        // stalls on `initialize`, which is explicitly best-effort — to
        // `Unavailable`. The outer bound is a MULTIPLE of the per-request one
        // plus slack for what sits outside ureq's own clock (DNS, TLS, and
        // `spawn_blocking` queue delay), so it stays a real cap on a black-holed
        // host without redefining what the operator configured.
        //
        // `request_timeout_ms` is deliberately NOT consulted here: it is the
        // `tools/call` budget, it may be minutes long, and this expression is
        // awaited inline by `AppState::new`.
        let per_request = Duration::from_millis(block.bringup_timeout_ms());
        // One formula, one place: `autospawn_enabled_within` widens the loop
        // budget by this same value, and the two drifting apart is exactly the
        // boot-vs-enable disagreement documented on CONNECTOR_AUTOSPAWN_BUDGET.
        let budget = connector_bringup_budget(manifest);
        let outcome = tokio::time::timeout(
            budget,
            connect_mcp_http(id, block, install_path.to_path_buf()),
        )
        .await;

        let (client, upstream) = match outcome {
            Err(_elapsed) => {
                return self
                    .connector_unavailable(
                        id,
                        guard,
                        format!(
                            "connector bring-up timed out after {} ms \
                             ({} × mcp_http.bringup_timeout_ms = {} ms, plus {} ms slack)",
                            budget.as_millis(),
                            MCP_HTTP_ROUND_TRIPS,
                            per_request.as_millis(),
                            CONNECTOR_BRINGUP_SLACK.as_millis(),
                        ),
                    )
                    .await;
            }
            Ok(Err(reason)) => return self.connector_unavailable(id, guard, reason).await,
            Ok(Ok(v)) => v,
        };

        let tools = connector::materialize_http_tools(id, block, &upstream);
        let tool_count = tools.len();

        // ---- §2.7(1): materialization, then the live insert. -------------
        // The ORDER of the next two blocks is the invariant. Each stamps the
        // process-global monotonic tick as its LAST action, so the recorded
        // pair is a structural witness of the order the source is written in:
        // swapping the two blocks swaps the two ticks, and
        // `connector_spawn_order()` reports it. See the acceptance test.

        // §2.7(2)(3) — field-level mutation, and a NO-OP if the id vanished
        // from the registry while we were on the wire (uninstall races an
        // in-flight spawn; see R12). Losing the race means we abandon the
        // spawn rather than resurrect an uninstalled connector.
        if !self.registry.set_exposes_tools(id, tools) {
            let reason = format!(
                "plugin `{id}` left the registry while its connector was starting \
                 (uninstalled or reloaded mid-spawn); abandoning spawn"
            );
            tracing::warn!(plugin_id = %id, "{reason}");
            drop(guard);
            // Every other failure exit emits a terminal state; without this the
            // event log would sit at `spawning` forever for an id that will
            // never come up. No live entry is inserted on purpose: the id was
            // uninstalled, and a runtime row would be exactly the resurrection
            // §2.7(3) exists to prevent.
            self.emit_state(id, &PluginRuntimeStatus::Unavailable { reason })
                .await;
            return Err(HostError::NotFound(id.to_string()));
        }
        self.stamp_spawn_order(id, SpawnOrderStep::Materialized);

        {
            let mut table = self.lock_table();
            let (crashes_in_window, window_started) = match table.live.get(id) {
                Some(prev) => (prev.crashes_in_window, prev.window_started),
                None => (0, Instant::now()),
            };
            table.spawning.remove(id);
            guard.disarm();
            table.live.insert(
                id.to_string(),
                RunningPlugin {
                    process: None,
                    mcp: Some(ConnectorClient::Http(Arc::clone(&client))),
                    status: PluginRuntimeStatus::Running,
                    stopping: false,
                    crashes_in_window,
                    window_started,
                    supervisor: None,
                    router: None,
                    subscriptions: Arc::new(Mutex::new(Vec::new())),
                },
            );
            drop(table);
            self.stamp_spawn_order(id, SpawnOrderStep::LiveInserted);
        }

        self.emit_state(id, &PluginRuntimeStatus::Running).await;
        tracing::info!(
            plugin_id = %id,
            target = %client.log_target(),
            tool_count,
            "mcp-http connector running"
        );
        Ok(())
    }

    /// Record one half of the §2.7(1) ordering pair. Two atomic operations and
    /// a small map write per connector spawn — cheap enough to keep in the
    /// production path, which is the point: an ordering probe that only exists
    /// under `cfg(test)` cannot witness the production ordering.
    fn stamp_spawn_order(&self, id: &str, step: SpawnOrderStep) {
        let tick = SPAWN_ORDER_TICK.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let mut map = self
            .spawn_order
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let entry = map.entry(id.to_string()).or_default();
        match step {
            SpawnOrderStep::Materialized => entry.materialized_at = Some(tick),
            SpawnOrderStep::LiveInserted => entry.live_inserted_at = Some(tick),
        }
    }

    /// The recorded §2.7(1) ordering for `id`'s most recent connector spawn.
    ///
    /// `materialized_at < live_inserted_at` is the invariant: the tool catalog
    /// must be in the registry before the id can be observed Running, because
    /// `running_plugin_ids` gates both tool discovery and the boot audit loop,
    /// and both then read `manifest.exposes_tools`.
    pub fn connector_spawn_order(&self, id: &str) -> Option<ConnectorSpawnOrder> {
        self.spawn_order
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(id)
            .copied()
    }

    /// Shared connector failure exit: swap the admission reservation for a
    /// live `Unavailable` entry, emit [`PluginRuntimeStatus::Unavailable`],
    /// return a typed error.
    ///
    /// The live entry is what makes a failed connector **observable**: without
    /// it `status()` returns `None` and `GET /api/plugins/{id}` reports the
    /// connector as if it had never been enabled, with no `last_error` — an
    /// operator's only signal would be the HTTP response to the `enable` call
    /// they may not have made (boot autospawn).
    ///
    /// Connectors have no supervisor, so this is terminal until an operator
    /// re-enables (§2.2) — and an `Unavailable` entry does not shadow that
    /// re-enable: the admission check only refuses `Running`/`Spawning`.
    /// Crucially it does NOT block boot: `autospawn_enabled` logs per-plugin
    /// errors and moves on.
    async fn connector_unavailable(
        self: &Arc<Self>,
        id: &str,
        guard: AdmissionGuard,
        reason: String,
    ) -> Result<(), HostError> {
        // Discarded on purpose: this path runs strictly before the live insert,
        // so the "already Running" arm is unreachable here (see the callee's
        // doc). The error below is the caller's answer either way.
        let _ = self
            .publish_unavailable(id, Some(guard), reason.clone())
            .await;
        Err(HostError::ConnectorUnavailable {
            plugin_id: id.to_string(),
            reason,
        })
    }

    /// The state half of [`Self::connector_unavailable`], usable without an
    /// admission guard.
    ///
    /// `guard: None` is for the caller that never held one: the boot-budget
    /// arm in [`Self::autospawn_enabled`], whose `spawn` future was dropped —
    /// which already released the reservation through `AdmissionGuard::drop`.
    /// It still owes the operator a terminal, observable state.
    ///
    /// **It will not regress a live `Running` entry, and returns `false` when
    /// it declines.** The insert used to be unconditional, which made the
    /// state machine able to run backwards: `autospawn_enabled_within` wraps
    /// its `tokio::time::timeout` around the WHOLE spawn, including
    /// `spawn_mcp_http`'s live insert, so a budget that elapsed after that
    /// insert (during the trailing `emit_state(Running).await`) landed here
    /// and overwrote a connector that was genuinely up — `HttpMcpClient` live,
    /// tools already in the registry — with `Unavailable`. The id then dropped
    /// out of `running_plugin_ids`, every materialized tool went invisible,
    /// the `ConnectorClient::Http` was dropped, and a false failure was
    /// broadcast and persisted. Only the caller can know whether the outcome
    /// it observed is authoritative, so the check lives here, once, rather
    /// than at each call site.
    ///
    /// No other caller loses anything: every `connector_unavailable` exit runs
    /// strictly BEFORE the live insert, so a `Running` entry at that point is
    /// impossible — and if one somehow existed it would belong to a different,
    /// successful bring-up that this failure has no claim to erase.
    async fn publish_unavailable(
        self: &Arc<Self>,
        id: &str,
        guard: Option<AdmissionGuard>,
        reason: String,
    ) -> bool {
        if !self.mark_unavailable(id, guard, reason.clone()) {
            return false;
        }
        self.emit_state(id, &PluginRuntimeStatus::Unavailable { reason })
            .await;
        true
    }

    /// The **synchronous** half of [`Self::publish_unavailable`]: release any
    /// admission reservation and publish the terminal live entry, under one
    /// table lock and with no await anywhere.
    ///
    /// Split out because "the operator can see that this connector failed" and
    /// "the event log records why" have different cost profiles: the first is a
    /// `HashMap::insert`, the second is a repo write that a slow event store can
    /// stall arbitrarily. The boot fence gives up the second when it is out of
    /// wall clock, and must never give up the first — a connector reported by
    /// `GET /api/plugins/{id}` as if it had never been enabled is the failure
    /// mode the `Unavailable` entry exists to prevent.
    ///
    /// Returns `false` without touching anything when the id is live and
    /// `Running` — see [`Self::publish_unavailable`] for why that check lives
    /// here, once, rather than at each call site.
    fn mark_unavailable(&self, id: &str, guard: Option<AdmissionGuard>, reason: String) -> bool {
        {
            // One lock: release the reservation and publish the terminal entry
            // together, so no observer sees the id as neither reserved nor
            // failed.
            let mut table = self.lock_table();
            let (crashes_in_window, window_started) = match table.live.get(id) {
                Some(prev) => (prev.crashes_in_window, prev.window_started),
                None => (0, Instant::now()),
            };
            table.spawning.remove(id);
            if let Some(guard) = guard {
                guard.disarm();
            }
            if matches!(
                table.live.get(id).map(|rp| &rp.status),
                Some(PluginRuntimeStatus::Running)
            ) {
                return false;
            }
            table.live.insert(
                id.to_string(),
                RunningPlugin {
                    process: None,
                    // Nothing to call — see the field doc.
                    mcp: None,
                    status: PluginRuntimeStatus::Unavailable {
                        reason: reason.clone(),
                    },
                    stopping: false,
                    crashes_in_window,
                    window_started,
                    supervisor: None,
                    router: None,
                    subscriptions: Arc::new(Mutex::new(Vec::new())),
                },
            );
        }
        tracing::warn!(plugin_id = %id, reason = %reason, "connector unavailable");
        true
    }

    /// Re-emit `Running` for a connector whose own `emit_state(Running)` never
    /// ran, but **only if it is still running when the emission happens**.
    /// Returns whether it emitted.
    ///
    /// This exists because "is it Running?" and "say it is Running" cannot be
    /// two independent steps. `publish_unavailable` answering `false` proves
    /// only that the entry was `Running` at the instant it held the table lock;
    /// by the time the caller awaits an emission, a concurrent [`Self::stop`]
    /// may have removed the entry and emitted `Disabled`, and this emission
    /// would then be the LAST word — persisting and broadcasting `Running` for
    /// a connector that no longer exists, with no client and no tools. That is
    /// the exact mirror of the F0 regression, so it gets the same treatment
    /// from the other side.
    ///
    /// Two things make the decision and the emission agree:
    ///
    /// * the per-id [`Self::state_emit`] guard is held across both, and `stop`'s own
    ///   remove-then-emit tail takes the same lock — so the two orderings are
    ///   "stop finished, we see no entry and stay quiet" or "we emitted first,
    ///   and stop's `Disabled` lands after us". Either way the last word is
    ///   correct;
    /// * `stopping` is checked as well as `Running`. `stop` sets that flag under
    ///   the table lock at its very start, so a stop that is merely IN FLIGHT
    ///   (still awaiting subscriptions/supervisor teardown, not yet at its
    ///   locked tail) also suppresses this emission rather than racing it.
    ///
    /// **Test coverage, stated honestly.** The re-check is driven by
    /// `the_boot_budget_reconcile_does_not_resurrect_a_stopped_connector` and
    /// is mutation-verified (delete it and that test fails). The `state_emit`
    /// serialization is driven by
    /// `two_emitters_for_one_connector_never_interleave`, which parks a real
    /// spawn's `Running` emission inside the critical section and runs a real
    /// `stop` against it (removing the lock from `emit_state` fails it). The
    /// `stopping` arm is not separately test-driven: it covers a window an
    /// external caller cannot open deterministically, so the only test
    /// available would be a racy one, which proves nothing when green.
    /// `pub` so the acceptance test can drive THIS function rather than a
    /// fixture that re-implements its check: the defect is precisely that the
    /// decision and the emission were two steps, and only the real one can
    /// witness that they have been joined.
    pub async fn reaffirm_running(self: &Arc<Self>, id: &str) -> bool {
        let serialized = self.lock_state(id).await;
        {
            let table = self.lock_table();
            match table.live.get(id) {
                Some(rp) if matches!(rp.status, PluginRuntimeStatus::Running) && !rp.stopping => {}
                _ => return false,
            }
        }
        self.emit_state_under(&serialized, &PluginRuntimeStatus::Running)
            .await;
        true
    }

    /// Gracefully stop a plugin. Sets `stopping=true` so the supervisor task
    /// won't respawn, sends SIGTERM via PluginProcess::stop, awaits exit.
    ///
    /// **Stopping a never-successfully-enabled connector is `Ok`, and it clears
    /// the `Unavailable` entry.** This is a deliberate decision, not a
    /// side effect of the `Unavailable`-entry change:
    ///
    /// * Before that change, a failed connector left no live entry at all, so
    ///   `stop` answered `NotFound` — i.e. "disable a connector that failed to
    ///   start" was a 404. That is the wrong answer to a request the operator
    ///   is entitled to make, and it is the one they make precisely BECAUSE it
    ///   failed.
    /// * Clearing `last_error` costs nothing durable: the reason was recorded
    ///   as a persisted+broadcast `Event::PluginState { unavailable, .. }` when
    ///   the bring-up failed, and it stays in the event log. The live entry is
    ///   a *current-state* view, and the current state after an explicit
    ///   disable is `disabled` — keeping a stale failure reason on a plugin the
    ///   operator has since turned off would be the misleading option.
    pub async fn stop(self: &Arc<Self>, id: &str) -> Result<(), HostError> {
        let (process, supervisor, subs) = {
            let mut table = self.lock_table();
            let rp = table
                .live
                .get_mut(id)
                .ok_or_else(|| HostError::NotFound(id.to_string()))?;
            if rp.stopping {
                return Err(HostError::BadState(format!("{id} is already stopping")));
            }
            rp.stopping = true;
            let process = rp.process.clone();
            let supervisor = rp.supervisor.take();
            let subs = Arc::clone(&rp.subscriptions);
            // Abort the router so it doesn't race the channel-close on
            // mcp drop. The handle itself stays in the struct until we
            // remove() below; abort() is idempotent and we don't await.
            // Connectors have no router (§2.5).
            if let Some(router) = rp.router.as_ref() {
                router.abort();
            }
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
        // §2.5 — kind-aware: a connector has no child to signal, so stopping
        // it is exactly "drop the client and forget the live entry". Dropping
        // the last `ConnectorClient` clone closes the HTTP client / releases
        // the CLI runtime; nothing else is owed.
        if let Some(process) = process {
            match process.stop(STOP_GRACE).await {
                Ok(_status) => {}
                Err(ProcessError::AlreadyDead) => {
                    // Supervisor was already going to react to this. Fine.
                }
                Err(e) => {
                    return Err(HostError::Spawn(e));
                }
            }
        }

        // Removing the entry and announcing `Disabled` is ONE serialized step.
        // Without the lock these are two, and `reaffirm_running` can slip a
        // stale `Running` between them — see its doc. The lock is taken here,
        // after the (possibly slow) child teardown above, so it never spans a
        // SIGTERM grace period.
        {
            let serialized = self.lock_state(id).await;
            {
                let mut table = self.lock_table();
                table.live.remove(id);
            }
            self.emit_state_under(&serialized, &PluginRuntimeStatus::Disabled)
                .await;
        }
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

    /// Snapshot current status for one plugin. An admission-reserved id
    /// (mid-spawn, no live entry yet) reports as `Spawning` with no pid.
    pub async fn status(&self, id: &str) -> Option<PluginHostStatus> {
        let table = self.lock_table();
        if table.spawning.contains(id) {
            return Some(PluginHostStatus {
                id: id.to_string(),
                status: PluginRuntimeStatus::Spawning,
                pid: None,
            });
        }
        table.live.get(id).map(|rp| PluginHostStatus {
            id: id.to_string(),
            status: rp.status.clone(),
            pid: rp.process.as_ref().and_then(|p| p.pid()),
        })
    }

    /// Snapshot the full table — used by the REST `GET /api/plugins` handler
    /// once Slice D wires it. Admission-reserved ids report as `Spawning`
    /// (they shadow any stale crashed live entry, matching `status`).
    pub async fn list_running(&self) -> Vec<PluginHostStatus> {
        let table = self.lock_table();
        let mut out: Vec<PluginHostStatus> = table
            .spawning
            .iter()
            .map(|id| PluginHostStatus {
                id: id.clone(),
                status: PluginRuntimeStatus::Spawning,
                pid: None,
            })
            .collect();
        out.extend(
            table
                .live
                .iter()
                .filter(|(id, _)| !table.spawning.contains(*id))
                .map(|(id, rp)| PluginHostStatus {
                    id: id.clone(),
                    status: rp.status.clone(),
                    pid: rp.process.as_ref().and_then(|p| p.pid()),
                }),
        );
        out
    }

    /// Snapshot ids that are currently running. Admission-reserved
    /// (`Spawning`) ids are deliberately NOT included: tool visibility and
    /// dispatch must not expose a plugin before its handshake completed.
    pub async fn running_plugin_ids(&self) -> BTreeSet<String> {
        let table = self.lock_table();
        table
            .live
            .iter()
            .filter(|(_, rp)| matches!(rp.status, PluginRuntimeStatus::Running))
            .map(|(id, _)| id.clone())
            .collect()
    }

    /// Most-recent stderr lines, oldest → newest. `n` clamps to the ring
    /// capacity inside `PluginProcess`.
    /// #1164 §2.5 — a connector has no child, hence no stderr ring: it reports
    /// an empty tail rather than `None`, so `GET /api/plugins/{id}/log`
    /// (which already defaults `None` to `[]`) needs no change either way.
    pub async fn stderr_tail(&self, id: &str, n: usize) -> Option<Vec<String>> {
        let table = self.lock_table();
        table.live.get(id).map(|rp| {
            rp.process
                .as_ref()
                .map(|p| p.stderr_tail(n))
                .unwrap_or_default()
        })
    }

    /// Borrow the live **stdio** MCP client.
    ///
    /// #1164 §2.6 / D12 narrowed this from "running" to
    /// `(Running, ConnectorClient::Stdio)`. Callers that structurally require
    /// a process-backed `app` plugin — forge-action dispatch, card creation
    /// via tool call, `neige.*` callback fan-out — keep using this and get a
    /// `None` for connectors, which is the truthful answer. Ordinary agent
    /// tool dispatch moved to [`Self::connector_client`].
    ///
    /// This cannot make an `app` plugin look "not running": the app spawn arm
    /// always parks a `Stdio` client.
    pub async fn mcp_client(&self, id: &str) -> Option<Arc<McpClient>> {
        let table = self.lock_table();
        table
            .live
            .get(id)
            .filter(|rp| matches!(rp.status, PluginRuntimeStatus::Running))
            .and_then(|rp| rp.mcp.as_ref()?.as_stdio().cloned())
    }

    /// Borrow the live client of a running plugin **whatever its kind**
    /// (#1164 §2.6). This is the accessor ordinary `plugin.<id>_<tool>`
    /// dispatch uses.
    ///
    /// The clone happens under the synchronous process-table mutex and the
    /// guard is dropped before the caller awaits — which is why every
    /// [`ConnectorClient`] variant is `Arc`-wrapped.
    pub async fn connector_client(&self, id: &str) -> Option<ConnectorClient> {
        let table = self.lock_table();
        table
            .live
            .get(id)
            .filter(|rp| matches!(rp.status, PluginRuntimeStatus::Running))
            .and_then(|rp| rp.mcp.clone())
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
            let table = self.lock_table();
            let rp = table
                .live
                .get(plugin_id)
                .ok_or_else(|| RpcError::custom(-32002, "plugin not running"))?;
            if !matches!(rp.status, PluginRuntimeStatus::Running) {
                return Err(RpcError::custom(-32002, "plugin not running"));
            }
            // #1164 §2.5 — `CallbackCtx.mcp` is deliberately still typed
            // `Arc<McpClient>`; the `neige.*` channel does not exist for
            // connectors (no inbound router is built for them), so a
            // non-`Stdio` client is refused here rather than widening the
            // callback surface.
            let Some(stdio) = rp.mcp.as_ref().and_then(|c| c.as_stdio()) else {
                // Two genuinely different situations, and rendering the second
                // through the first's sentence produced "plugin `x` is a
                // `unavailable` connector" — a STATUS word in a KIND slot.
                let detail = match rp.mcp.as_ref() {
                    Some(client) => format!("is a `{}` connector", client.variant_name()),
                    None => {
                        "has no MCP client (it is running without a live transport)".to_string()
                    }
                };
                return Err(RpcError::custom(
                    -32002,
                    format!(
                        "plugin `{plugin_id}` {detail}; \
                         neige.* callbacks are only available to app plugins"
                    ),
                ));
            };
            (Arc::clone(stdio), Arc::clone(&rp.subscriptions))
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

    /// Acquire the per-id emission lock. See [`Self::state_emit`].
    ///
    /// `pub` for the same reason [`Self::reaffirm_running`] is: the acceptance
    /// test drives the real serialization, not a copy of it.
    pub async fn lock_state(&self, id: &str) -> StateEmitGuard {
        let cell = {
            let mut map = self
                .state_emit
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            Arc::clone(map.entry(id.to_string()).or_insert_with(|| {
                Arc::new(StateEmitCell {
                    lock: Arc::new(Mutex::new(())),
                    inflight: std::sync::atomic::AtomicUsize::new(0),
                })
            }))
        };
        let held = Arc::clone(&cell.lock).lock_owned().await;
        StateEmitGuard {
            id: id.to_string(),
            cell,
            _held: held,
        }
    }

    /// High-water mark of concurrent emissions. `1` is the invariant; see
    /// [`Self::state_emit_peak`].
    pub fn peak_concurrent_state_emits(&self) -> usize {
        self.state_emit_peak
            .load(std::sync::atomic::Ordering::SeqCst)
    }

    /// Persist a `plugin.state` event and broadcast it, **taking the per-id
    /// emission lock**. Goes through `Repo::log_pure_event` so every fired
    /// event lands in the events table with a real `_id`; the bus broadcast is
    /// fired only after commit succeeds (commit-then-emit invariant).
    ///
    /// The lock is acquired here rather than at the call sites so that no
    /// emitter can be written that opts out of it — see [`Self::state_emit`].
    /// A caller that must decide and emit as one step takes the guard itself
    /// and calls [`Self::emit_state_under`]; there is no third way to emit.
    async fn emit_state(&self, id: &str, status: &PluginRuntimeStatus) {
        let guard = self.lock_state(id).await;
        self.emit_state_under(&guard, status).await;
    }

    /// [`Self::emit_state`] for a caller that already holds the guard, so that
    /// its decision and this emission are one serialized step.
    ///
    /// The id comes from the guard, never from a second parameter: emitting for
    /// an id you do not hold the lock on is the same defect one indirection
    /// later, and this makes it unrepresentable.
    async fn emit_state_under(&self, guard: &StateEmitGuard, status: &PluginRuntimeStatus) {
        let id = guard.id.as_str();
        let inflight = guard
            .cell
            .inflight
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst)
            + 1;
        self.state_emit_peak
            .fetch_max(inflight, std::sync::atomic::Ordering::SeqCst);
        let _dec = InflightGuard(Arc::clone(&guard.cell));
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
            let table = self.lock_table();
            table.live.get(&id).map(|rp| rp.stopping).unwrap_or(true)
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
            let table = self.lock_table();
            table
                .live
                .get(&id)
                .and_then(|rp| rp.process.as_ref())
                .map(|p| p.stderr_tail(10).join("\n"))
                .unwrap_or_default()
        };
        let combined_reason = if tail.is_empty() {
            reason
        } else {
            format!("{reason}\nstderr tail:\n{tail}")
        };

        // Crash-window bookkeeping.
        let (attempts, exceeded) = {
            let mut table = self.lock_table();
            let entry = match table.live.get_mut(&id) {
                Some(e) => e,
                None => {
                    // Was removed by `stop()` — nothing to do.
                    return;
                }
            };
            if entry.window_started.elapsed() > self.backoff.crash_window {
                entry.window_started = Instant::now();
                entry.crashes_in_window = 0;
            }
            entry.crashes_in_window += 1;
            entry.status = PluginRuntimeStatus::Crashed {
                reason: combined_reason.clone(),
            };
            (
                entry.crashes_in_window,
                entry.crashes_in_window >= self.backoff.crash_window_limit,
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
            let mut table = self.lock_table();
            if let Some(rp) = table.live.get_mut(&id) {
                rp.supervisor = None;
            }
            return;
        }

        // Backoff then respawn. Index by (attempts - 1) clamped to the table.
        let idx = (attempts as usize).saturating_sub(1);
        let delay_ms = self
            .backoff
            .schedule_ms
            .get(idx)
            .copied()
            .unwrap_or_else(|| *self.backoff.schedule_ms.last().expect("non-empty schedule"));
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
            let mut table = self.lock_table();
            table.live.remove(&id);
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
/// for the first workflow id of `manifest` that another **holding trusted**
/// candidate manifest already declares; `None` when the spawn may proceed.
///
/// Rules (design §4.4):
/// * only fires when the spawning plugin itself is trusted — untrusted
///   plugins never enter the workflow resolution set, so their (unreachable)
///   duplicate ids are tolerated;
/// * only holding ∧ trusted candidates count — `holder_ids` is the caller's
///   atomic snapshot of running plugins PLUS admission-reserved (`Spawning`)
///   ids ([`ProcessTable::workflow_holder_ids`]), so a stopped plugin does
///   not squat on its workflow ids but a concurrent mid-spawn one already
///   holds them (#891 review fix — anti-TOCTOU);
/// * the spawning plugin's own registry entry is skipped (respawn path).
///
/// The trust predicate is injected because the trusted set is
/// env-configured (`NEIGE_TRUSTED_FORGE_PLUGINS`), which keeps this core
/// unit-testable without mutating process env.
fn find_workflow_conflict(
    manifest: &Manifest,
    candidates: impl IntoIterator<Item = Manifest>,
    holder_ids: &BTreeSet<String>,
    is_trusted: &dyn Fn(&str) -> bool,
) -> Option<HostError> {
    if !is_trusted(&manifest.id) {
        return None;
    }
    for other in candidates {
        if other.id == manifest.id || !holder_ids.contains(&other.id) || !is_trusted(&other.id) {
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
                { "id": workflow_id }
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
