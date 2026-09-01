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

/// #1196 §2.6 — how many times the supervisor's third segment re-reads the
/// plugin row before giving up and leaving the plugin `Crashed`.
///
/// Bounded rather than infinite so a permanently dead repo does not leave a
/// task spinning forever; the price of exhausting it is an observable
/// `Crashed` state, which is the fail-closed side.
const LIFECYCLE_DB_READ_RETRIES: u32 = 5;

/// Delay between those retries. Short: the read is one indexed `SELECT`, and
/// the plugin is sitting in `Crashed` while we wait.
const LIFECYCLE_DB_READ_RETRY_DELAY: Duration = Duration::from_millis(200);

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
    ///
    /// #1196 §1.3 — this used to be reset to zero on every respawn, because
    /// the supervisor removed the live entry *before* calling `spawn`, and the
    /// spawn path inherited the counter by reading `live.get(id)` — which was
    /// then necessarily `None`. The effect was that `crashes_in_window` was
    /// permanently 1, `CRASH_WINDOW_LIMIT` never fired, and the backoff never left its
    /// first step. The supervisor now carries the pair explicitly across the
    /// remove (see [`CrashWindow`]).
    crashes_in_window: u32,
    window_started: Instant,
    /// #1196 §2.6 — identity of THIS run instance, allocated from
    /// [`PluginHost::run_epoch_seq`] immediately before the live insert and
    /// captured by the supervisor task at creation time.
    ///
    /// It answers exactly one question: "is the entry I am looking at still the
    /// one I was supervising?" A deletion counter cannot: the supervisor's own
    /// third segment removes the entry, `HashMap::insert` replaces entries
    /// without any removal at all, and a counter hung off the lock cell would
    /// fail *open* the moment somebody dropped the cell on uninstall.
    ///
    /// It deliberately does NOT answer "is this plugin still enabled" — that is
    /// a DB fact, and §2.6's third segment reads it separately, fail-closed.
    run_epoch: u64,
    /// #1196 §2.6 — how many crashes this run instance has been through,
    /// monotonic for the life of the entry.
    ///
    /// Separate from [`Self::crashes_in_window`] on purpose: that one is reset
    /// whenever the rolling window expires, so a supervisor comparing it across
    /// its backoff sleep could see the same number for two different crashes
    /// and wrongly conclude nothing had happened.
    crash_attempt: u64,
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

/// #1196 §2.6 — the crash-window counters carried explicitly across the
/// supervisor's `live.remove` into the respawn.
///
/// The old code expected `spawn` to inherit them by reading the live entry it
/// was about to replace, which the supervisor had already deleted. Passing them
/// as a value is the whole fix: there is no window in which the counters live
/// nowhere.
#[derive(Debug, Clone, Copy)]
struct CrashWindow {
    crashes: u32,
    started: Instant,
}

pub struct PluginHost {
    registry: Arc<PluginRegistry>,
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
    /// #1196 S1 — **the** per-plugin-id lifecycle lock.
    ///
    /// One id's whole lifecycle vocabulary — install / enable / disable /
    /// uninstall / reload / spawn / stop / restart / token rotation / crash
    /// respawn — runs inside one [`LifecycleGuard`] lifetime, and so does every
    /// `plugin.state` emission that operation produces.
    ///
    /// **Why one lock and not two.** This map used to be `state_emit`, which
    /// serialized emissions only. That left the two halves of every composite
    /// operation splittable: `spawn_mcp_http` could insert a live `Running`
    /// entry, drop the table lock, and only then await its emission — while a
    /// concurrent `stop` removed the entry and committed `disabled` in between.
    /// The live table ended empty while the event log's last word was
    /// `running`, with no later event to reconcile it. A second lock covering
    /// "operations" would not have fixed that: it would have reproduced the
    /// same split one level up, because there would still exist a state in
    /// which a task holds the operation lock but not the emission lock.
    /// Widening the emission lock to cover the whole operation is the fix.
    ///
    /// **Why per-id and not global.** The real cost is runtime, not boot: one
    /// connector bring-up can take ~30 s, and a global lock would block every
    /// other plugin's enable/disable for that whole window. (Boot's autospawn
    /// is a serial `for` loop already, so a global lock would cost it nothing —
    /// that is not the argument.)
    ///
    /// Two acquisition semantics — see [`Self::try_lock_lifecycle`] (external,
    /// non-blocking, refuses with [`HostError::LifecycleBusy`]) and
    /// [`Self::await_lifecycle`] (internal, waits; for callers that have nobody
    /// to answer and would otherwise silently drop information).
    ///
    /// **Lock order is one-way: `lifecycle` (async) → `processes` (sync) →
    /// registry (leaf).** Nothing takes the process-table mutex and then awaits
    /// the lifecycle lock; registry methods are synchronous, never await and
    /// never call back into the host. A transaction closure handed to
    /// `write_in_tx` must never take the lifecycle lock — nothing does today,
    /// and saying so is what keeps the order acyclic (design §5 R2).
    ///
    /// The map only ever grows by installed-plugin count. Entries are never
    /// removed: a `remove` on uninstall would hand a *fresh* mutex to the next
    /// caller while an old guard was still alive, which is the one way to
    /// break mutual exclusion here.
    lifecycle: std::sync::Mutex<HashMap<String, Arc<LifecycleCell>>>,
    /// #1196 §2.6 — allocator for per-run-instance identity. Every live-table
    /// insert of a *running* instance gets a fresh value, which the supervisor
    /// captures at creation time and re-checks at each of its three decision
    /// points. See [`RunningPlugin::run_epoch`].
    run_epoch_seq: std::sync::atomic::AtomicU64,
    /// #1196 §4 acceptance 15/16 — the narrow DB port the supervisor's third
    /// segment and the enable/disable pair speak.
    ///
    /// It exists because `Arc<dyn RouteRepo>` is ~100 methods wide, has exactly
    /// one implementation in this repo, and cannot have a one-shot read failure
    /// injected into it without 600–900 lines of delegating boilerplate
    /// (`pool().close()` is a *permanent* failure and cannot express "fails
    /// once, then recovers"). See [`lifecycle::LifecycleDb`].
    lifecycle_db: Arc<dyn lifecycle::LifecycleDb>,
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

/// Per-id lifecycle lock. One `Arc<Mutex<()>>` behind a map entry that is
/// created on first use and never removed (see [`PluginHost::lifecycle`]).
struct LifecycleCell {
    lock: Arc<Mutex<()>>,
}

/// Proof that the caller holds the lifecycle lock for `id`.
///
/// Only [`PluginHost::try_lock_lifecycle`] and [`PluginHost::await_lifecycle`]
/// construct one, so a function that takes one cannot be called without the
/// lock. That is what makes "the decision and the emission are one step" — and
/// "an id's composite operations are serialized" — properties of the types
/// rather than of reviewer memory.
///
/// The id is read **off the guard**, never passed as a second parameter: acting
/// on an id you do not hold the lock on is the original defect one indirection
/// later, and taking it from the guard makes that unrepresentable.
pub struct LifecycleGuard {
    id: String,
    _held: tokio::sync::OwnedMutexGuard<()>,
}

impl LifecycleGuard {
    /// The plugin id this guard is held for.
    pub fn id(&self) -> &str {
        &self.id
    }

    /// A guard over a throwaway mutex, for the lib's own unit tests of
    /// [`PluginRegistry`]'s mutators.
    ///
    /// `#[cfg(test)]` is load-bearing here and is NOT the escape hatch design
    /// §2.3 warns about: that warning is about a `pub` `insert_unlocked` that
    /// the integration tests in `tests/` (a separate crate, linking the
    /// non-test lib) would need. `tests/` cannot see this constructor at all,
    /// so the migration to guard-carrying writes is still forced there — and
    /// non-test builds of this crate cannot see it either, so no production
    /// path can acquire one.
    #[cfg(test)]
    pub(crate) fn for_test(id: &str) -> Self {
        let lock = Arc::new(Mutex::new(()));
        let held = lock.try_lock_owned().expect("fresh mutex");
        Self {
            id: id.to_string(),
            _held: held,
        }
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
        let lifecycle_db: Arc<dyn lifecycle::LifecycleDb> =
            Arc::new(lifecycle::RepoLifecycleDb::new(Arc::clone(&repo)));
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
            lifecycle: std::sync::Mutex::new(HashMap::new()),
            run_epoch_seq: std::sync::atomic::AtomicU64::new(1),
            lifecycle_db,
            backoff: BackoffConfig::default(),
        }
    }

    /// #1196 §4 acceptance 15/16 — post-construction override of the narrow
    /// [`LifecycleDb`](lifecycle::LifecycleDb) port.
    ///
    /// A builder for the same reason [`Self::with_backoff_schedule`] is one:
    /// `new_full` has ~106 call sites and none of them need to know this seam
    /// exists. Production never calls this — `new_full` installs the repo-backed
    /// implementation.
    #[must_use]
    pub fn with_lifecycle_db(mut self, db: Arc<dyn lifecycle::LifecycleDb>) -> Self {
        self.lifecycle_db = db;
        self
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

    /// Read-only view of the registry.
    ///
    /// #1196 §2.3 — the field itself is private now. This handle is only useful
    /// for `get` / `list` / `install_path` / `len`: the three mutators
    /// ([`PluginRegistry::insert`] / [`PluginRegistry::remove`] /
    /// [`PluginRegistry::set_exposes_tools`]) are `pub(in crate::plugin_host)`
    /// **and** take a [`LifecycleGuard`], so possession of this `Arc` grants no
    /// write capability to anything outside this module — including
    /// `CallbackCtx`, which holds a clone of it.
    ///
    /// Stated honestly (design §2.3): this is not a type-level proof that the
    /// registry cannot be written without the lock. It is a proof that it
    /// cannot be written **from outside `plugin_host`**. The residual inside
    /// the module is the enumerable set of `*_under` functions.
    pub fn registry(&self) -> &Arc<PluginRegistry> {
        &self.registry
    }

    // -----------------------------------------------------------------------
    // #1196 §2.5 — the two acquisition semantics
    // -----------------------------------------------------------------------

    /// The `Arc<Mutex>` for `id`, creating the map entry on first use.
    fn lifecycle_cell(&self, id: &str) -> Arc<Mutex<()>> {
        let mut map = self
            .lifecycle
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        Arc::clone(
            &map.entry(id.to_string())
                .or_insert_with(|| {
                    Arc::new(LifecycleCell {
                        lock: Arc::new(Mutex::new(())),
                    })
                })
                .lock,
        )
    }

    /// **External** acquisition: non-blocking. Returns
    /// [`HostError::LifecycleBusy`] — a 409 with the code `plugin_busy` — when
    /// another operation holds the id, **having done nothing at all**.
    ///
    /// Every public lifecycle entry point takes the lock through this function
    /// as its first (or, for `install`, first side-effecting) act, which is what
    /// makes the refusal inert and therefore safely retryable by the caller.
    ///
    /// Synchronous on purpose: `try_lock_owned` needs no async context, so the
    /// boot fallback in [`Self::autospawn_enabled_within`] can take it from
    /// inside a non-async block without changing [`Self::mark_unavailable_under`]
    /// into an async fn, and without adding an await to the boot wall-clock
    /// formula (design §5 R5).
    ///
    /// `pub` because the acceptance tests must be able to *hold* the real lock
    /// (design §5 R7). An external holder cannot reach any `*_under` function,
    /// so the worst it can do is block itself.
    pub fn try_lock_lifecycle(&self, id: &str) -> Result<LifecycleGuard, HostError> {
        let cell = self.lifecycle_cell(id);
        match cell.try_lock_owned() {
            Ok(held) => Ok(LifecycleGuard {
                id: id.to_string(),
                _held: held,
            }),
            Err(_) => Err(HostError::LifecycleBusy(id.to_string())),
        }
    }

    /// **Internal** acquisition: waits until the lock is free.
    ///
    /// For the callers that have **nobody to answer**, where giving up loses
    /// information permanently:
    ///
    /// * the crash supervisor's three segments. The supervisor task is created
    ///   *before* the live insert and before `spawn`'s trailing `Running`
    ///   emission, while `spawn` holds the guard for the whole of that. A child
    ///   that dies in that window therefore makes the supervisor's first
    ///   segment collide with `spawn`'s own lock **by construction**. One `try`
    ///   and a give-up would mean the crash is never accounted, the live table
    ///   keeps a `Running` entry over a dead child, and no later task exists to
    ///   fix it. The third segment is the same story ending in a permanent
    ///   `Crashed`;
    /// * boot reconciliation ([`Self::publish_unavailable`] /
    ///   [`Self::reaffirm_running`]) and boot autospawn's retry, where a `Busy`
    ///   would be a *third* outcome the two-armed timeout branch has no place
    ///   for — and where the natural mistake (fold it into the failure arm)
    ///   pushes a healthy connector to `Unavailable`.
    ///
    /// Private, and each acquisition **re-decides everything** afterwards: the
    /// world may have moved arbitrarily while we waited, which is what
    /// [`RunningPlugin::run_epoch`] exists to detect.
    ///
    /// There is deliberately no wait budget. Any honest bound would have to
    /// exceed the longest legal critical section, and that has no bound: a
    /// connector bring-up, a reload's file I/O, and several unbounded event
    /// writes all live inside one.
    async fn await_lifecycle(&self, id: &str) -> LifecycleGuard {
        let cell = self.lifecycle_cell(id);
        let held = cell.lock_owned().await;
        LifecycleGuard {
            id: id.to_string(),
            _held: held,
        }
    }

    /// Allocate the next [`RunningPlugin::run_epoch`].
    fn next_run_epoch(&self) -> u64 {
        self.run_epoch_seq
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst)
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
    pub fn registry_insert(
        &self,
        guard: &LifecycleGuard,
        manifest: manifest::Manifest,
        install_path: Option<PathBuf>,
    ) {
        self.registry.insert(guard, manifest, install_path);
    }

    /// #1196 S0a — **runtime** registry removal. See [`Self::registry_insert`].
    pub fn registry_remove(&self, guard: &LifecycleGuard) -> Option<manifest::Manifest> {
        self.registry.remove(guard)
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
    ///
    /// #1196 §2.2 — takes the [`LifecycleGuard`] rather than an id: it writes
    /// `plugin_tokens`, which `uninstall` deletes, so the write must be inside
    /// the same critical section as the spawn that needs it.
    pub async fn ensure_plugin_token(&self, guard: &LifecycleGuard) -> Result<String, HostError> {
        let id = guard.id();
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
    ///
    /// #1196 §2.2 — the registry lookup, the kind guard, the token delete and
    /// the restart are ONE critical section. Split across guards, a concurrent
    /// `uninstall` could land between the kind check and the restart, and the
    /// restart would then respawn a plugin that no longer exists.
    pub async fn rotate_plugin_token(self: &Arc<Self>, id: &str) -> Result<(), HostError> {
        let guard = self.try_lock_lifecycle(id)?;
        self.rotate_plugin_token_under(&guard).await
    }

    async fn rotate_plugin_token_under(
        self: &Arc<Self>,
        guard: &LifecycleGuard,
    ) -> Result<(), HostError> {
        let id = guard.id();
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
        self.restart_under(guard).await
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
                if let Err(e) = self.autospawn_one(&plug.id).await {
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
                // #1196 §5 R5 — the no-lock exception is GONE. `mark_unavailable`
                // writes the live table, and the live table is what
                // `GET /api/plugins/{id}` and `running_plugin_ids` report; an
                // unlocked write here can be the last runtime write of a
                // concurrent `stop`, resurrecting a plugin with no matching
                // event. So take the lock — synchronously, because
                // `try_lock_owned` needs no async context and this arm must
                // stay await-free (an await here is the very thing that ran the
                // boot budget out, and `MAX_CONNECTOR_AUTOSPAWN_WALL` is
                // computed on the assumption that it has none).
                //
                // What is given up when the lock is held is the observability
                // contract, not correctness — and the window is tiny: the only
                // possible holder is the `spawn` future we just dropped, whose
                // guard is released by that drop.
                match self.try_lock_lifecycle(&plug.id) {
                    Ok(g) => {
                        self.mark_unavailable_under(&g, None, reason);
                    }
                    Err(_) => tracing::warn!(
                        plugin_id = %plug.id,
                        "boot budget elapsed and the lifecycle lock was held; \
                         leaving the runtime state to its holder"
                    ),
                }
            }
        }
    }

    /// One non-connector autospawn attempt.
    ///
    /// #1196 §2.5 — `Busy` must have an explicit terminal, not a bare `warn!`
    /// and `continue`: that leaves `enabled = true`, no live entry, and the
    /// event log stopped at whatever the previous operation said — exactly the
    /// "looks like it was never enabled" state the `Unavailable` entry exists to
    /// prevent. Boot's only possible contender is a crash supervisor, so the
    /// retry waits rather than guessing, and the wait is bounded by that
    /// supervisor's own work.
    async fn autospawn_one(self: &Arc<Self>, id: &str) -> Result<(), HostError> {
        match self.try_lock_lifecycle(id) {
            Ok(g) => self.spawn_under(&g, None).await,
            Err(HostError::LifecycleBusy(_)) => {
                tracing::info!(
                    plugin_id = %id,
                    "autospawn found the lifecycle lock held; waiting for it"
                );
                let g = self.await_lifecycle(id).await;
                self.spawn_under(&g, None).await
            }
            Err(other) => Err(other),
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
            let _ = self.publish_unavailable(id, reason).await;
            return;
        }
        let outcome = tokio::time::timeout_at(
            tokio::time::Instant::from_std(spawn_deadline),
            self.autospawn_one(id),
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
                if self.publish_unavailable(id, reason.clone()).await {
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
        let guard = self.try_lock_lifecycle(id)?;
        self.spawn_under(&guard, None).await
    }

    /// [`Self::spawn`] for a caller that already holds the guard.
    ///
    /// `inherit` carries the crash-window counters across a supervisor respawn
    /// (§2.6). `None` means "read whatever the live entry says", which is the
    /// right answer for every other caller: an explicit `spawn` after a crash
    /// finds the `Crashed` entry still in place and must not zero its counters.
    async fn spawn_under(
        self: &Arc<Self>,
        lifecycle: &LifecycleGuard,
        inherit: Option<CrashWindow>,
    ) -> Result<(), HostError> {
        let id = lifecycle.id();
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
        let admitted = match admission {
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
                self.emit_crashed_under(lifecycle, &conflict.to_string())
                    .await;
                return Err(conflict);
            }
        };

        self.spawn_admitted(lifecycle, &manifest, admitted, inherit)
            .await
    }

    /// Everything downstream of a successful admission reservation: token
    /// mint, process exec, MCP handshake, router + supervisor wiring, and
    /// the final swap of the reservation for the live `Running` entry (one
    /// lock). Owns the [`AdmissionGuard`]: every failure exit — `Err`
    /// return, task abort/drop at any `.await`, panic — drops the guard,
    /// which releases the reservation; the success swap disarms it.
    async fn spawn_admitted(
        self: &Arc<Self>,
        lifecycle: &LifecycleGuard,
        manifest: &Manifest,
        guard: AdmissionGuard,
        inherit: Option<CrashWindow>,
    ) -> Result<(), HostError> {
        let id = lifecycle.id();
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
                    .spawn_mcp_http(lifecycle, manifest, &install_path, guard, inherit)
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
                        lifecycle,
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
        let token = self.ensure_plugin_token(lifecycle).await?;

        self.emit_state_under(lifecycle, &PluginRuntimeStatus::Spawning)
            .await;

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
                    self.emit_crashed_under(lifecycle, reason).await;
                    return Err(HostError::AuthMismatch(id.to_string()));
                }
                self.emit_crashed_under(lifecycle, &format!("initialize failed: {e}"))
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
        // #1196 §2.6 — the epoch is allocated BEFORE the supervisor task is
        // created and BEFORE the live insert, and handed to the supervisor by
        // value. Reading it back off the table later would defeat the purpose:
        // the whole question the supervisor asks is "is the entry still MINE",
        // and an answer read from the entry can only ever say yes.
        let run_epoch = self.next_run_epoch();
        let supervisor = {
            let host = Arc::clone(self);
            let plugin_id = id.to_string();
            tokio::spawn(async move {
                host.supervise(plugin_id, run_epoch, child_handle).await;
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
            let (crashes_in_window, window_started) = inherited_window(&table, id, inherit);
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
                    run_epoch,
                    crash_attempt: 0,
                    supervisor: Some(supervisor),
                    router: Some(router),
                    subscriptions,
                },
            );
        }

        self.emit_state_under(lifecycle, &PluginRuntimeStatus::Running)
            .await;
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
        lifecycle: &LifecycleGuard,
        manifest: &Manifest,
        install_path: &std::path::Path,
        guard: AdmissionGuard,
        inherit: Option<CrashWindow>,
    ) -> Result<(), HostError> {
        let id = lifecycle.id();
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

        self.emit_state_under(lifecycle, &PluginRuntimeStatus::Spawning)
            .await;

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
                        lifecycle,
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
            Ok(Err(reason)) => {
                return self.connector_unavailable(lifecycle, guard, reason).await;
            }
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
        if !self.registry.set_exposes_tools(lifecycle, tools) {
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
            self.emit_state_under(lifecycle, &PluginRuntimeStatus::Unavailable { reason })
                .await;
            return Err(HostError::NotFound(id.to_string()));
        }
        self.stamp_spawn_order(id, SpawnOrderStep::Materialized);

        {
            let mut table = self.lock_table();
            let (crashes_in_window, window_started) = inherited_window(&table, id, inherit);
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
                    // Connectors have no supervisor, so nothing ever compares
                    // this epoch. It is still allocated (rather than a
                    // sentinel) so `live` has one meaning of "run instance".
                    run_epoch: self.next_run_epoch(),
                    crash_attempt: 0,
                    supervisor: None,
                    router: None,
                    subscriptions: Arc::new(Mutex::new(Vec::new())),
                },
            );
            drop(table);
            self.stamp_spawn_order(id, SpawnOrderStep::LiveInserted);
        }

        self.emit_state_under(lifecycle, &PluginRuntimeStatus::Running)
            .await;
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
        lifecycle: &LifecycleGuard,
        guard: AdmissionGuard,
        reason: String,
    ) -> Result<(), HostError> {
        // Discarded on purpose: this path runs strictly before the live insert,
        // so the "already Running" arm is unreachable here (see the callee's
        // doc). The error below is the caller's answer either way.
        let _ = self
            .publish_unavailable_under(lifecycle, Some(guard), reason.clone())
            .await;
        Err(HostError::ConnectorUnavailable {
            plugin_id: lifecycle.id().to_string(),
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
    ///
    /// #1196 §2.2 — this has two classes of caller: `spawn_under`'s internal
    /// failure exits, which already hold the lifecycle guard, and boot
    /// reconciliation, which does not. Hence the pair. The reconciliation
    /// wrapper **waits** for the lock (§2.5): it has nobody to answer, and a
    /// `Busy` folded into the failure arm would report a healthy connector as
    /// `Unavailable`.
    async fn publish_unavailable(self: &Arc<Self>, id: &str, reason: String) -> bool {
        let guard = self.await_lifecycle(id).await;
        self.publish_unavailable_under(&guard, None, reason).await
    }

    async fn publish_unavailable_under(
        self: &Arc<Self>,
        lifecycle: &LifecycleGuard,
        guard: Option<AdmissionGuard>,
        reason: String,
    ) -> bool {
        if !self.mark_unavailable_under(lifecycle, guard, reason.clone()) {
            return false;
        }
        self.emit_state_under(lifecycle, &PluginRuntimeStatus::Unavailable { reason })
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
    fn mark_unavailable_under(
        &self,
        lifecycle: &LifecycleGuard,
        guard: Option<AdmissionGuard>,
        reason: String,
    ) -> bool {
        let id = lifecycle.id();
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
                    // Terminal entry, no supervisor: nothing will ever compare
                    // this epoch, but it must be distinct from the run it is
                    // replacing so a stale supervisor cannot match it.
                    run_epoch: self.next_run_epoch(),
                    crash_attempt: 0,
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
        // #1196 §2.5 — boot reconciliation waits. Returning `false` on a busy
        // lock would be indistinguishable from "it is not running", and the
        // caller uses that answer to decide whether the event log still needs a
        // `running` — so a `Busy` would leave the log stuck at `spawning`
        // forever for a connector that is genuinely up.
        let serialized = self.await_lifecycle(id).await;
        self.reaffirm_running_under(&serialized).await
    }

    async fn reaffirm_running_under(self: &Arc<Self>, guard: &LifecycleGuard) -> bool {
        {
            let table = self.lock_table();
            match table.live.get(guard.id()) {
                Some(rp) if matches!(rp.status, PluginRuntimeStatus::Running) && !rp.stopping => {}
                _ => return false,
            }
        }
        self.emit_state_under(guard, &PluginRuntimeStatus::Running)
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
        let guard = self.try_lock_lifecycle(id)?;
        self.stop_under(&guard).await
    }

    /// [`Self::stop`] for a caller that already holds the guard.
    ///
    /// #1196 §2.4 — this is why `stop` no longer needs a "also look in
    /// `spawning`" branch. An in-flight spawn necessarily holds the same guard
    /// from before admission until either the live insert or the
    /// `AdmissionGuard` rollback, so by the time we are here that spawn has
    /// already landed or already unwound. There is no third state to see.
    async fn stop_under(self: &Arc<Self>, guard: &LifecycleGuard) -> Result<(), HostError> {
        let id = guard.id();
        let (process, supervisor, subs) = {
            let mut table = self.lock_table();
            let rp = table
                .live
                .get_mut(id)
                .ok_or_else(|| HostError::NotFound(id.to_string()))?;
            // #1196 §2.3 — unreachable while the guard is held: only a `stop`
            // sets this flag, and only one `stop` can be inside the guard.
            // Kept as a debug assertion rather than deleted so a future
            // refactor that reintroduces an unlocked stop path trips on it in
            // tests instead of silently corrupting the table.
            debug_assert!(
                !rp.stopping,
                "{id} was already stopping while its lifecycle guard was held"
            );
            if rp.stopping {
                return Err(HostError::NotFound(id.to_string()));
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
            let mut table = self.lock_table();
            table.live.remove(id);
        }
        self.emit_state_under(guard, &PluginRuntimeStatus::Disabled)
            .await;
        Ok(())
    }

    /// Stop then spawn. Returns the spawn error if either half fails.
    /// #1196 §2.2 — the stop and the spawn are ONE critical section. As two,
    /// the gap between them is a real window: a concurrent `uninstall` lands
    /// there and the respawn resurrects a deleted plugin.
    pub async fn restart(self: &Arc<Self>, id: &str) -> Result<(), HostError> {
        let guard = self.try_lock_lifecycle(id)?;
        self.restart_under(&guard).await
    }

    async fn restart_under(self: &Arc<Self>, guard: &LifecycleGuard) -> Result<(), HostError> {
        // Stop is best-effort: if it returns NotFound (e.g. already crashed
        // and cleaned up), we proceed to spawn.
        match self.stop_under(guard).await {
            Ok(()) | Err(HostError::NotFound(_)) => {}
            Err(e) => return Err(e),
        }
        self.spawn_under(guard, None).await
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

    /// Persist a `plugin.state` event and broadcast it. Goes through
    /// `Repo::log_pure_event` so every fired event lands in the events table
    /// with a real `_id`; the bus broadcast fires only after commit succeeds
    /// (commit-then-emit invariant).
    ///
    /// **#1196: this is the ONLY emitter, and it demands the guard.** The
    /// `emit_state(id, status)` overload that took the lock for you is gone.
    /// Its existence was the defect: it made "decide under a lock, drop the
    /// lock, emit afterwards" a shape one could write, and seven call sites had
    /// written it. With only this signature left, an emission that is not
    /// inside its decision's critical section is not expressible inside
    /// `plugin_host`.
    ///
    /// Stated honestly (design §4 acceptance 2 / §2.7): that is a property of
    /// this module, not of the process. Any crate can still construct an
    /// `Event::PluginState` and write it to the repo directly; closing *that*
    /// needs a type fence on the event variant and is tracked as #1210. Do not
    /// read this doc as "cannot be bypassed".
    ///
    /// The id comes from the guard, never from a second parameter: emitting for
    /// an id you do not hold the lock on is the same defect one indirection
    /// later, and this makes it unrepresentable.
    async fn emit_state_under(&self, guard: &LifecycleGuard, status: &PluginRuntimeStatus) {
        let id = guard.id();
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

    async fn emit_crashed_under(&self, guard: &LifecycleGuard, reason: &str) {
        let status = PluginRuntimeStatus::Crashed {
            reason: reason.to_string(),
        };
        self.emit_state_under(guard, &status).await;
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
        run_epoch: u64,
        child: tokio::process::Child,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>> {
        Box::pin(self.supervise_inner(id, run_epoch, child))
    }

    /// #1196 §2.6 — the supervisor in three segments:
    ///
    /// 1. **[lock held]** decide it was a crash, account it, rewrite the live
    ///    entry, emit `crashed`;
    /// 2. **[no lock]** sleep out the backoff (up to 30 s — holding the lock
    ///    across it would make `disable` wait half a minute);
    /// 3. **[lock re-taken]** re-decide *everything* and respawn.
    ///
    /// The supervisor is the one continuation not covered by the guard of the
    /// operation that created it, which is why it is the only thing here that
    /// needs a run-instance identity to re-validate against.
    async fn supervise_inner(
        self: Arc<Self>,
        id: String,
        run_epoch: u64,
        mut child: tokio::process::Child,
    ) {
        let exit_result = child.wait().await;

        // ---- segment 1 -----------------------------------------------------
        // `await_lifecycle`, not `try`: the task was created BEFORE the live
        // insert and before `spawn`'s trailing `Running` emission, all of which
        // happen under the spawn's own guard. A child that dies inside that
        // window collides with that guard by construction, and a `try` that
        // gave up here would never account the crash — leaving a `Running`
        // entry over a dead process with no task left to notice.
        let (attempt, delay_ms) = {
            let guard = self.await_lifecycle(&id).await;

            // Is this still MY run instance, and is it still in the state a
            // crash can be attributed to? Without this check an old supervisor
            // that only now got the lock — after a `stop` and a fresh `spawn` —
            // would mark the NEW entry crashed.
            let observed = {
                let table = self.lock_table();
                table.live.get(&id).map(|rp| {
                    (
                        rp.run_epoch,
                        rp.stopping,
                        matches!(rp.status, PluginRuntimeStatus::Running),
                    )
                })
            };
            match observed {
                // Removed by `stop`/`uninstall`, or replaced by a newer run:
                // not ours to report.
                None => {
                    tracing::info!(plugin_id = %id, "plugin exited; its live entry is gone");
                    return;
                }
                Some((epoch, _, _)) if epoch != run_epoch => {
                    tracing::info!(
                        plugin_id = %id,
                        "plugin exited; a newer run instance owns the entry"
                    );
                    return;
                }
                Some((_, true, _)) => {
                    tracing::info!(plugin_id = %id, "plugin exited gracefully");
                    return;
                }
                Some((_, false, false)) => {
                    // Somebody already wrote a terminal state for this exact
                    // run (`Crashed` from the auth-mismatch path, `Unavailable`
                    // from a boot fence). Not a second crash.
                    tracing::info!(
                        plugin_id = %id,
                        "plugin exited but its run instance is no longer Running"
                    );
                    return;
                }
                Some((_, false, true)) => {}
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
            let (attempts, attempt, exceeded) = {
                let mut table = self.lock_table();
                let Some(entry) = table.live.get_mut(&id) else {
                    return;
                };
                if entry.window_started.elapsed() > self.backoff.crash_window {
                    entry.window_started = Instant::now();
                    entry.crashes_in_window = 0;
                }
                entry.crashes_in_window += 1;
                entry.crash_attempt += 1;
                entry.status = PluginRuntimeStatus::Crashed {
                    reason: combined_reason.clone(),
                };
                (
                    entry.crashes_in_window,
                    entry.crash_attempt,
                    entry.crashes_in_window >= self.backoff.crash_window_limit,
                )
            };

            self.emit_crashed_under(&guard, &combined_reason).await;

            if exceeded {
                tracing::error!(
                    plugin_id = %id,
                    attempts,
                    "plugin exceeded crash-window limit; not respawning",
                );
                // Leave the Crashed entry in place so `status()` returns it. The
                // supervisor task ends here; an explicit `spawn(id)` revives.
                // We do, however, drop the supervisor handle so it gets reaped.
                //
                // Epoch-checked like every other write: without it, a
                // `stop`+`spawn` that raced us would have its NEW entry's
                // supervisor handle cleared, and that entry would then never be
                // respawned after ITS crash.
                let mut table = self.lock_table();
                if let Some(rp) = table.live.get_mut(&id)
                    && rp.run_epoch == run_epoch
                {
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
            (attempt, delay_ms)
        };

        // ---- segment 2: the lock is NOT held ------------------------------
        // Up to 30 s. Holding the lifecycle guard across it would make a
        // `disable` issued during a crash loop block for the whole backoff.
        tokio::time::sleep(Duration::from_millis(delay_ms)).await;

        // ---- segment 3 ----------------------------------------------------
        self.respawn_after_backoff(&id, run_epoch, attempt).await;
    }

    /// Segment 3 of [`Self::supervise_inner`]: re-decide everything, then
    /// respawn.
    ///
    /// Everything decided before the sleep is re-derived here. Nothing is
    /// carried over except the two identity values (`run_epoch`, `attempt`),
    /// and in particular the manifest is NOT: a `reload` during the backoff may
    /// have replaced it wholesale, so the respawn goes through the full
    /// `spawn_under`.
    async fn respawn_after_backoff(self: &Arc<Self>, id: &str, run_epoch: u64, attempt: u64) {
        for retry in 0..LIFECYCLE_DB_READ_RETRIES {
            let guard = self.await_lifecycle(id).await;

            // (a) still our run instance, still the Crashed state WE wrote,
            //     not being stopped, and no further crash has happened.
            let ok = {
                let table = self.lock_table();
                match table.live.get(id) {
                    Some(rp) => {
                        rp.run_epoch == run_epoch
                            && rp.crash_attempt == attempt
                            && !rp.stopping
                            && matches!(rp.status, PluginRuntimeStatus::Crashed { .. })
                    }
                    None => false,
                }
            };
            if !ok {
                tracing::info!(
                    plugin_id = %id,
                    "backoff elapsed but the run instance is gone or has moved on; not respawning"
                );
                return;
            }

            // (b) still installed.
            if self.registry.get(id).is_none() {
                tracing::info!(
                    plugin_id = %id,
                    "backoff elapsed but the plugin left the registry; not respawning"
                );
                return;
            }

            // (c) still enabled, per the DB. **Fail closed.**
            //
            // `run_epoch` proves the *runtime* instance was not replaced. It
            // proves nothing about the `enabled` bit: the route layer holds an
            // `Arc<dyn RouteRepo>` and can write the plugin row without going
            // through the host at all (design §2.3 registers that residual
            // explicitly). So a read failure must not be treated as "probably
            // still enabled" — that respawns a plugin an operator has disabled.
            // We keep `Crashed`, release the lock so nothing else is blocked,
            // and retry the whole set of predicates from scratch.
            match self.lifecycle_db.enabled_row(id).await {
                Ok(Some(true)) => {}
                Ok(Some(false)) | Ok(None) => {
                    tracing::info!(
                        plugin_id = %id,
                        "backoff elapsed but the plugin is no longer enabled; not respawning"
                    );
                    return;
                }
                Err(e) => {
                    drop(guard);
                    tracing::warn!(
                        plugin_id = %id,
                        error = %e,
                        retry,
                        "could not read the plugin row after backoff; keeping Crashed and retrying"
                    );
                    tokio::time::sleep(LIFECYCLE_DB_READ_RETRY_DELAY).await;
                    continue;
                }
            }

            // #1196 §2.6 / §1.3 — carry the crash window ACROSS the remove.
            // The old code removed the entry and then relied on `spawn` reading
            // `live.get(id)` to inherit the counters, which was necessarily
            // `None` — so every respawn started from zero, the crash-window
            // limit never fired and the backoff never advanced past its first
            // step. Taking the values out before the remove is the fix.
            let carried = {
                let mut table = self.lock_table();
                let carried = table.live.get(id).map(|rp| CrashWindow {
                    crashes: rp.crashes_in_window,
                    started: rp.window_started,
                });
                table.live.remove(id);
                carried
            };
            if let Err(e) = self.spawn_under(&guard, carried).await {
                tracing::error!(plugin_id = %id, error = %e, "respawn failed");
                self.emit_crashed_under(&guard, &format!("respawn failed: {e}"))
                    .await;
            }
            return;
        }
        tracing::error!(
            plugin_id = %id,
            "gave up respawning after {LIFECYCLE_DB_READ_RETRIES} failed plugin-row reads; \
             the plugin stays Crashed"
        );
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// The crash-window counters a new live entry starts from.
///
/// `inherit` (the supervisor's explicit carry across its own `live.remove`)
/// wins; otherwise we read whatever entry we are about to replace, which is
/// what an explicit `spawn` after a crash needs — the `Crashed` entry is still
/// there and its counters must not be zeroed.
fn inherited_window(
    table: &ProcessTable,
    id: &str,
    inherit: Option<CrashWindow>,
) -> (u32, Instant) {
    match inherit {
        Some(w) => (w.crashes, w.started),
        None => match table.live.get(id) {
            Some(prev) => (prev.crashes_in_window, prev.window_started),
            None => (0, Instant::now()),
        },
    }
}

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
