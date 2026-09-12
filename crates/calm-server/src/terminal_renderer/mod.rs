use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex as StdMutex};
use std::time::Duration;

use crate::db::RouteRepo;
use calm_session::control::{
    AttachRequest, Attached, ControlMsg, ControlReply, EnsureProcRequest, IoMode, ProcSignal,
    SignalRequest,
};
use calm_session::terminal_session::{OwnerRegistry, RenderPlane};
use calm_session::{DaemonMsg, read_frame, write_frame};
use thiserror::Error;
use tokio::io::AsyncRead;
use tokio::net::UnixStream;
use tokio::sync::{broadcast, mpsc, oneshot, watch};
use tokio::task::JoinHandle;
use tokio::time::timeout;
use uuid::Uuid;

mod attach_reader;
mod child_ready;
mod client_pump;
mod control_writer;
mod input_authority;
mod model_view;
pub use input_authority::{ClientInputScope, InputBarrier, WriteAuthority};
pub use model_view::{ModelView, SharedModelView};
#[cfg(test)]
pub(crate) mod establishment_test_hook;
mod output_capture;
pub mod signals;
mod snapshot;
pub use signals::{IncomingSignal, SIGNAL_MESSAGE_MAX_CHARS, Signal, SignalRing, SignalsSince};

pub use client_pump::{ClientPumpContext, run_client_pump};

pub type SharedRenderPlane = Arc<StdMutex<RenderPlane>>;
pub type SharedOwnerRegistry = Arc<StdMutex<OwnerRegistry>>;
pub type SharedExitState = Arc<StdMutex<Option<TerminalExitInfo>>>;

// Mirrors `scrollback` in xterm.js Terminal config at
// `web/src/XtermView.tsx` — must be kept in lockstep so the client's
// local ring isn't smaller than the server cap (which would silently
// trim daemon-retained history on the way to the user's screen).
pub(crate) const SCROLLBACK_MAX_LINES: usize = 2000;
const SPAWN_CONTROL_READ_TIMEOUT: Duration = Duration::from_secs(30);

/// Teardown grace between SIGTERM and SIGKILL for a renderer's pty child.
///
/// An upper bound rather than a fixed sleep — but the event it is cut short by
/// is scoped to the **pty leader**, not to the whole process group (#993 F3,
/// R3-B): teardown escalates to SIGKILL as soon as the leader's exit has been
/// *persisted*, so a group member that is still winding down loses whatever is
/// left of the window. That is deliberate, and it is a real (accepted) change
/// from the old unconditional `sleep`:
///
/// * every member already received the SIGTERM at t=0 — the window only ever
///   bought them time, it never delivered anything extra later;
/// * the leader is the pty session leader, so its exit SIGHUPs the foreground
///   group anyway; a member still alive after that has ignored both TERM and
///   HUP and would not have used the remaining ~195 ms either;
/// * keeping the common case at single-digit ms is what makes the serial batch
///   teardown loops (track delete, sweeper, area delete) cheap.
///
/// The unconditional SIGKILL to the *group* after this window is what
/// guarantees those survivors are still reaped
/// (`drop_entry_kills_process_group_members_that_outlive_the_leader`).
const TERM_TO_KILL_GRACE: Duration = Duration::from_millis(200);

/// Bounded wait, after SIGKILL, for the supervisor attach reader to observe
/// `Exited` and persist it (`terminal_set_exit`, session-projection
/// completion, plan-task hook) before `abort_tasks()` cuts it off.
///
/// Issue #993 R1: the supervisor holds `Exited` back for up to
/// `PTY_DRAIN_GRACE` after the child is reaped (see
/// `calm_proc_supervisor::PTY_DRAIN_GRACE`), so an unconditional
/// abort-right-after-kill can lose the terminal's exit row whenever a
/// grandchild keeps the pty slave open. The wait is on the attach reader's
/// own completion rather than a sleep, so it costs nothing on the common path
/// (the reader is usually already finished) and it does not silently depend on
/// the two constants happening to be ordered — but the const asserts below
/// make a degenerate ordering a compile error anyway.
const EXIT_PERSIST_GRACE: Duration = Duration::from_millis(1000);

/// What the budget actually has to cover, in order:
///
///   `PTY_DRAIN_GRACE` (supervisor holds `Exited` back)
/// + reap → seal → broadcast → UDS write → attach-reader wakeup
/// + `terminal_set_exit` + session lifecycle observation
/// + the plan-task completion hook (one more DB transaction).
///
/// Only the first term is a compile-time constant; everything after it is
/// runtime work whose latency depends on the DB and on scheduler pressure, so
/// **no** const assert can prove the budget is sufficient — the terminal-exit
/// sweep is the real backstop, and `await_exit_persisted` WARNs on timeout so a
/// too-small budget is visible in the logs rather than silent.
///
/// What the asserts below *do* buy: they fail the build if someone shrinks the
/// margin to the point where the constant part alone eats most of the budget.
/// The plain `>` the first version used would have been satisfied by
/// `PTY_DRAIN_GRACE = 999ms`, which is why the ratio and the absolute headroom
/// are both pinned here (#993 R2/F4).
const _: () = assert!(
    EXIT_PERSIST_GRACE.as_millis() >= calm_proc_supervisor::PTY_DRAIN_GRACE.as_millis() * 4,
    "terminal teardown must give the post-drain persistence work several times the \
     supervisor's pty drain grace, not merely one millisecond more (#993 R1)"
);
const _: () = assert!(
    EXIT_PERSIST_GRACE.as_millis() - calm_proc_supervisor::PTY_DRAIN_GRACE.as_millis() >= 500,
    "terminal teardown must leave at least 500ms of absolute headroom over the pty drain \
     grace for reap → broadcast → terminal_set_exit → projection → task hook (#993 R1)"
);

/// One work item on the PTY-writer channel. Carries the bytes to write
/// plus the metadata needed to ack the originating connection after the
/// write completes.
#[derive(Clone)]
pub struct PtyWrite {
    pub authority: WriteAuthority,
    pub data: Vec<u8>,
    pub input_seq: u64,
    pub ack: Option<mpsc::UnboundedSender<DaemonMsg>>,
}

pub enum SupervisorControl {
    Write(PtyWrite),
    Resize { cols: u16, rows: u16 },
    Signal(ProcSignal),
}

#[derive(Clone)]
pub struct RendererConfig {
    pub terminal_id: String,
    pub cols: u16,
    pub rows: u16,
    pub buffer_bytes: usize,
    pub terminal_fg: (u8, u8, u8),
    pub terminal_bg: (u8, u8, u8),
    pub program: String,
    pub args: Vec<String>,
    pub envs: Vec<(String, String)>,
    pub cwd: String,
    pub supervisor_sock: PathBuf,
}

pub struct RendererHandle {
    pub model_view: SharedModelView,
    pub input_barrier: Arc<InputBarrier>,
    pub session_id: Uuid,
    pub event_rx: broadcast::Receiver<DaemonMsg>,
    pub event_tx: broadcast::Sender<DaemonMsg>,
    pub render_plane: SharedRenderPlane,
    pub owner_registry: SharedOwnerRegistry,
    pub supervisor_tx: mpsc::UnboundedSender<SupervisorControl>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TerminalExitInfo {
    pub code: Option<i32>,
    pub pty_seq: u32,
    pub render_rev: u32,
}

pub struct RendererEntry {
    pub terminal_id: String,
    pub proc_id: String,
    pub supervisor_sock: PathBuf,
    pub handle: RendererHandle,
    config: RendererConfig,
    /// Set exactly once when the supervisor's attach stream delivers
    /// `Exited`. Late client pumps replay this immediately after
    /// `ServerHello` because broadcast receivers do not retain history.
    pub exit: SharedExitState,
    /// #1620 hook signals for this terminal (untrusted advisory telemetry;
    /// see `signals.rs`). Lives with the renderer generation: a respawned
    /// terminal starts an empty ring.
    pub signals: SignalRing,
    initial_event_rx: StdMutex<Option<broadcast::Receiver<DaemonMsg>>>,
    exited_rx: StdMutex<Option<oneshot::Receiver<Option<i32>>>>,
    /// Held apart from `tasks` because teardown must let it *finish its exit
    /// arm* rather than abort it: this is the task that persists the terminal
    /// exit (#993 R1). Teardown never awaits this handle — see
    /// `exit_persisted`.
    attach_task: StdMutex<Option<JoinHandle<()>>>,
    /// Flipped by the attach reader once it has run the whole `Exited` arm
    /// (persist + projection + task hook). A closed channel means the reader
    /// ended *without* persisting — an attach-stream read error breaks its
    /// loop exactly like a clean exit does (#993 R3-A).
    exit_persisted: watch::Receiver<bool>,
    tasks: StdMutex<Vec<JoinHandle<()>>>,
}

/// Outcome of waiting for the attach reader to persist the terminal exit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ExitPersistWait {
    /// The reader signalled that the exit reached the database.
    Persisted,
    /// The reader is gone and never signalled: nothing can persist the exit
    /// any more, the terminal-exit sweep is the only remaining backstop.
    ReaderGone,
    /// The reader is still running; the budget expired first.
    Timeout,
}

/// Evidence returned to destructive callers. `ExitPersisted` means the
/// supervisor attach stream observed `Exited` and the reader completed its DB
/// write; `Unverified` must keep the owning workspace in place.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RendererDropOutcome {
    Missing,
    ExitPersisted,
    Unverified,
}

impl RendererEntry {
    /// Sever the supervisor output stream the way a lost attach connection
    /// does: the attach reader is aborted and its drop guard invalidates the
    /// projection (`terminal output source disconnected`) through
    /// `RenderPlane::invalidate_observation`. Test observability only; the
    /// child process and its teardown are untouched.
    #[doc(hidden)]
    pub fn disconnect_output_source_for_test(&self) {
        if let Ok(mut attach) = self.attach_task.lock()
            && let Some(task) = attach.take()
        {
            task.abort();
        }
    }
    pub fn config(&self) -> &RendererConfig {
        &self.config
    }

    pub fn take_initial_event_rx(&self) -> Option<broadcast::Receiver<DaemonMsg>> {
        self.initial_event_rx
            .lock()
            .ok()
            .and_then(|mut guard| guard.take())
    }

    pub fn subscribe(&self) -> broadcast::Receiver<DaemonMsg> {
        self.handle.event_tx.subscribe()
    }

    pub async fn wait_exited(&self) -> Option<Option<i32>> {
        let rx = match self.exited_rx.lock() {
            Ok(mut guard) => guard.take(),
            Err(_) => None,
        };
        match rx {
            Some(rx) => rx.await.ok(),
            None => None,
        }
    }

    pub async fn wait_exit_code(&self) -> Option<i32> {
        self.wait_exited().await.flatten()
    }

    pub async fn shutdown_signal(&self, sig: ProcSignal) {
        signal_child_direct(&self.supervisor_sock, &self.proc_id, sig).await;
    }

    /// Waits (bounded) for the supervisor attach reader to *persist* the
    /// terminal exit.
    ///
    /// #993 R3-A: this deliberately does **not** await the reader's join
    /// handle. That handle completes on any loop exit, including the
    /// `Err(_) => break` arm taken when the attach stream dies (supervisor
    /// gone, connection reset) — a path that writes nothing to the database.
    /// Treating it as success made a degraded teardown indistinguishable from
    /// a healthy one, silently collapsed the SIGTERM→SIGKILL grace to zero and
    /// suppressed the WARN that is supposed to make the degradation visible.
    ///
    /// Never aborts anything; `abort_tasks` stays the only abort site.
    async fn await_exit_persisted(&self, budget: Duration) -> ExitPersistWait {
        let mut rx = self.exit_persisted.clone();
        if *rx.borrow_and_update() {
            return ExitPersistWait::Persisted;
        }
        let wait = async {
            while rx.changed().await.is_ok() {
                if *rx.borrow_and_update() {
                    return ExitPersistWait::Persisted;
                }
            }
            // Sender dropped: the reader task is gone and never signalled.
            ExitPersistWait::ReaderGone
        };
        tokio::time::timeout(budget, wait)
            .await
            .unwrap_or(ExitPersistWait::Timeout)
    }

    /// Observe the existing end-of-exit-callback signal without stopping a reader.
    #[cfg(feature = "fixtures")]
    pub async fn wait_exit_persisted_for_test(&self, budget: Duration) -> bool {
        self.await_exit_persisted(budget).await == ExitPersistWait::Persisted
    }

    fn abort_tasks(&self) {
        if let Ok(mut attach) = self.attach_task.lock()
            && let Some(task) = attach.take()
        {
            task.abort();
        }
        if let Ok(mut tasks) = self.tasks.lock() {
            for task in tasks.drain(..) {
                task.abort();
            }
        }
    }
}

#[derive(Debug, Error)]
#[error(transparent)]
pub struct RendererSpawnError(#[from] anyhow::Error);

pub struct TerminalRendererRegistry {
    entries: StdMutex<HashMap<String, Arc<RendererEntry>>>,
    repo: Option<Arc<dyn RouteRepo>>,
    /// Issue #644 M2 — terminal-exit completion bundle, installed by the
    /// dispatcher construction site (it owns the EventBus + role caches
    /// the hook needs; the registry is built earlier in boot). `None`
    /// until installed; entries spawned before installation simply skip
    /// the task hook (boot spawns nothing before `AppState` completes).
    task_hook: StdMutex<Option<Arc<crate::scheduler::TerminalTaskHook>>>,
    /// #1620 — server-owned directory of generated Planner terminal hook
    /// settings files (`<data_dir>/terminal-hooks/<card_id>.json`). Installed
    /// at boot next to the adapters that write the files; teardown deletes
    /// only paths derived from this directory and the card id, never a path
    /// read from a terminal row's env.
    hook_settings_dir: StdMutex<Option<PathBuf>>,
}

impl TerminalRendererRegistry {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            entries: StdMutex::new(HashMap::new()),
            repo: None,
            task_hook: StdMutex::new(None),
            hook_settings_dir: StdMutex::new(None),
        })
    }

    pub fn new_with_repo(repo: Arc<dyn RouteRepo>) -> Arc<Self> {
        Arc::new(Self {
            entries: StdMutex::new(HashMap::new()),
            repo: Some(repo),
            task_hook: StdMutex::new(None),
            hook_settings_dir: StdMutex::new(None),
        })
    }

    /// Install the issue #644 M2 terminal-exit completion bundle. Called
    /// by the dispatcher construction funnel; idempotent (last write
    /// wins — every production caller passes an equivalent bundle).
    pub fn set_task_hook(&self, hook: Arc<crate::scheduler::TerminalTaskHook>) {
        if let Ok(mut guard) = self.task_hook.lock() {
            *guard = Some(hook);
        }
    }

    fn task_hook(&self) -> Option<Arc<crate::scheduler::TerminalTaskHook>> {
        self.task_hook.lock().ok().and_then(|guard| guard.clone())
    }

    /// Install the #1620 hook settings directory (idempotent, last write wins).
    pub fn set_hook_settings_dir(&self, dir: PathBuf) {
        if let Ok(mut guard) = self.hook_settings_dir.lock() {
            *guard = Some(dir);
        }
    }

    /// Delete the generated hook settings file for `card_id`, if this
    /// registry owns a settings directory. The path is derived here from the
    /// server-owned directory and the card id only.
    pub fn remove_hook_settings(&self, card_id: &str) {
        let Some(dir) = self.hook_settings_dir.lock().ok().and_then(|g| g.clone()) else {
            return;
        };
        crate::terminal_hooks::remove_settings_file(&dir, card_id);
    }

    /// Append a hook signal to the CURRENT renderer entry of `terminal_id`,
    /// under the registry lock so a concurrent drop/ensure cannot route it to
    /// a superseded generation. Returns the seq, `None` when there is no live
    /// entry or the delivery was a duplicate.
    pub fn push_signal(
        &self,
        terminal_id: &str,
        idempotency_key: &str,
        incoming: IncomingSignal,
        now_ms: i64,
    ) -> Option<u64> {
        let entries = self.entries.lock().ok()?;
        let entry = entries.get(terminal_id)?;
        entry.signals.push(idempotency_key, incoming, now_ms)
    }

    /// Spawn a PTY proc on the supervisor and stand up the in-process
    /// renderer. Returns a handle the WS pump (in 3b) will use.
    pub async fn ensure(
        &self,
        cfg: RendererConfig,
    ) -> Result<Arc<RendererEntry>, RendererSpawnError> {
        self.ensure_with_launch(cfg, None).await
    }

    pub(crate) async fn ensure_for_task(
        &self,
        cfg: RendererConfig,
        launch: crate::operation::task_launch::TaskLaunch,
    ) -> Result<Arc<RendererEntry>, RendererSpawnError> {
        self.ensure_with_launch(cfg, Some(launch)).await
    }

    async fn ensure_with_launch(
        &self,
        cfg: RendererConfig,
        launch: Option<crate::operation::task_launch::TaskLaunch>,
    ) -> Result<Arc<RendererEntry>, RendererSpawnError> {
        if let Some(existing) = self.get(&cfg.terminal_id) {
            return Ok(existing);
        }

        let EstablishedRenderer { entry, handoff } =
            ensure_entry(cfg, self.repo.clone(), self.task_hook(), launch).await?;
        #[cfg(test)]
        if let Some((launch, _)) = handoff.as_ref() {
            establishment_test_hook::pause(launch.task_id(), &entry.terminal_id).await;
        }
        let entry = Arc::new(entry);
        let entry = {
            let mut entries = self
                .entries
                .lock()
                .map_err(|_| anyhow::anyhow!("terminal renderer registry mutex poisoned"))?;
            if let Some(existing) = entries.get(&entry.terminal_id) {
                entry.abort_tasks();
                if handoff.is_some()
                    && (existing.terminal_id != entry.terminal_id
                        || existing.proc_id != entry.proc_id
                        || existing.supervisor_sock != entry.supervisor_sock)
                {
                    return Err(anyhow::anyhow!("concurrent renderer identity changed; retain owned launch for reconciliation").into());
                }
                // A read-only caller has no handoff proof. A fresh caller still
                // owns its observed PID and must finish the same durable handoff
                // even when the UI installed this exact renderer first.
                existing.clone()
            } else {
                tracing::info!(terminal_id=%entry.terminal_id, "terminal renderer registry inserted entry");
                entries.insert(entry.terminal_id.clone(), entry.clone());
                entry
            }
        };
        if let Some((launch, pid)) = handoff {
            let repo = self
                .repo
                .as_deref()
                .ok_or_else(|| anyhow::anyhow!("task launch requires its repository"))?;
            crate::operation::terminal_launch::hand_off(
                repo,
                launch,
                entry.terminal_id.clone(),
                pid,
            )
            .await
            .map_err(anyhow::Error::from)?;
        }
        Ok(entry)
    }

    /// Look up an existing entry by terminal id, no spawn.
    pub fn get(&self, terminal_id: &str) -> Option<Arc<RendererEntry>> {
        self.entries
            .lock()
            .ok()
            .and_then(|entries| entries.get(terminal_id).cloned())
    }

    #[cfg(feature = "fixtures")]
    pub fn insert_test_entry(&self, cfg: RendererConfig) -> Arc<RendererEntry> {
        let render_plane: SharedRenderPlane = Arc::new(StdMutex::new(RenderPlane::with_colors(
            cfg.cols,
            cfg.rows,
            cfg.buffer_bytes,
            SCROLLBACK_MAX_LINES,
            Some(cfg.terminal_fg),
            Some(cfg.terminal_bg),
        )));
        let model_view = ModelView::new(cfg.cols, cfg.rows, cfg.terminal_fg, cfg.terminal_bg);
        render_plane
            .lock()
            .unwrap()
            .install_observer(Box::new(model_view::Observer(model_view.clone())));
        let owner_registry: SharedOwnerRegistry = Arc::new(StdMutex::new(OwnerRegistry::new()));
        let exit = Arc::new(StdMutex::new(None));
        let session_id = Uuid::new_v4();
        let (event_tx, initial_event_rx) = broadcast::channel::<DaemonMsg>(2048);
        let event_rx = event_tx.subscribe();
        let (supervisor_tx, _supervisor_rx) = mpsc::unbounded_channel::<SupervisorControl>();
        let (_exited_tx, exited_rx) = oneshot::channel::<Option<i32>>();
        // No attach reader in a fixture entry: drop the sender right away so a
        // teardown sees `ReaderGone` (honest) instead of waiting out a budget
        // for a reader that does not exist.
        let exit_persisted = watch::channel(false).1;
        let entry = Arc::new(RendererEntry {
            terminal_id: cfg.terminal_id.clone(),
            proc_id: format!("term:{}", cfg.terminal_id),
            supervisor_sock: cfg.supervisor_sock.clone(),
            handle: RendererHandle {
                model_view,
                input_barrier: Arc::new(InputBarrier::default()),
                session_id,
                event_rx,
                event_tx,
                render_plane,
                owner_registry,
                supervisor_tx,
            },
            config: cfg,
            exit,
            signals: SignalRing::new(),
            initial_event_rx: StdMutex::new(Some(initial_event_rx)),
            exited_rx: StdMutex::new(Some(exited_rx)),
            attach_task: StdMutex::new(None),
            exit_persisted,
            tasks: StdMutex::new(Vec::new()),
        });
        let mut entries = self
            .entries
            .lock()
            .expect("terminal renderer registry mutex");
        entries.insert(entry.terminal_id.clone(), entry.clone());
        entry
    }

    pub fn is_empty(&self) -> bool {
        self.entries
            .lock()
            .map(|entries| entries.is_empty())
            .unwrap_or(false)
    }

    /// Tear down a renderer: drop the broadcast, signal Term/Kill to
    /// the supervisor via a fresh UDS connection, and remove from the map.
    ///
    /// # Latency contract (#993 F3) — read this before calling it in a loop
    ///
    /// * Common case (child dies on SIGTERM): returns as soon as the attach
    ///   reader has persisted `Exited`, typically single-digit ms. The old
    ///   unconditional `sleep(TERM_TO_KILL_GRACE)` is gone — note that this
    ///   shortens the TERM→KILL window for *other members* of the child's
    ///   process group too; see `TERM_TO_KILL_GRACE` for why that is accepted.
    /// * Worst case (child ignores SIGTERM *and* a grandchild holds the pty
    ///   slave past the supervisor's drain grace):
    ///   `TERM_TO_KILL_GRACE + EXIT_PERSIST_GRACE` = **1.2 s**.
    ///
    /// The batch teardown paths — `routes::tracks` track-delete,
    /// `terminal_sweeper::sweep`, `routes::areas` — call this serially, once
    /// per terminal, so a track of N terminals costs up to `N * 1.2 s` in the
    /// worst case. That is accepted deliberately rather than fanned out: those
    /// loops interleave per-card repo writes and event emission whose ordering
    /// the callers rely on, and every one of them is a rare
    /// administrative/janitor path with no interactive latency budget. The
    /// worst case also requires a wedged child *per terminal*; the realistic
    /// batch cost is N × single-digit ms. If a fan-out ever becomes necessary,
    /// the unit to parallelise is this call, not the surrounding repo work.
    pub async fn drop_entry(&self, terminal_id: &str) {
        let _ = self.drop_entry_with_outcome(terminal_id).await;
    }

    pub(crate) async fn require_disposal_safe(
        &self,
        terminal_id: &str,
    ) -> crate::error::Result<()> {
        if let Some(repo) = self.repo.as_deref() {
            crate::operation::terminal_disposal::require_safe(
                repo,
                crate::operation::terminal_disposal::Scope::Terminal(terminal_id.to_owned()),
                None,
            )
            .await?;
        }
        Ok(())
    }

    pub(crate) async fn drop_entry_for_deletion(&self, terminal_id: &str) -> RendererDropOutcome {
        self.drop_entry_with_outcome(terminal_id).await
    }

    async fn drop_entry_with_outcome(&self, terminal_id: &str) -> RendererDropOutcome {
        let entry = self
            .entries
            .lock()
            .ok()
            .and_then(|mut entries| entries.remove(terminal_id));
        let Some(entry) = entry else {
            return RendererDropOutcome::Missing;
        };

        tracing::info!(terminal_id, "terminal renderer registry dropping entry");
        let term_at = tokio::time::Instant::now();
        entry.shutdown_signal(ProcSignal::Term).await;
        // Wait for the exit to be *persisted* instead of sleeping the full
        // grace: a child that dies on SIGTERM cuts the teardown from a fixed
        // 200ms to single-digit ms, which is what keeps the serial batch paths
        // cheap (#993 F3). A child that ignores SIGTERM costs exactly the old
        // 200ms.
        let mut outcome = entry.await_exit_persisted(TERM_TO_KILL_GRACE).await;
        if outcome == ExitPersistWait::ReaderGone {
            // #993 R3-A: the reader died early (attach stream error), so
            // nothing will ever persist this exit — but the SIGTERM→SIGKILL
            // window belongs to the *child*, not to our bookkeeping. Burn what
            // is left of it instead of escalating instantly; only the reader
            // is degraded here, the child may still be exiting cleanly.
            tokio::time::sleep_until(term_at + TERM_TO_KILL_GRACE).await;
        }
        // Unconditional, even when the child is already gone: this signals the
        // whole *process group*, so it is also what reaps group members that
        // outlived the SIGTERM. Skipping it would leak them.
        entry.shutdown_signal(ProcSignal::Kill).await;
        // #993 R1: `Exited` can lag the kill by up to the supervisor's pty
        // drain grace, and the attach reader is what persists it. Let it
        // finish before the abort — but only while it is still alive.
        if outcome == ExitPersistWait::Timeout {
            outcome = entry.await_exit_persisted(EXIT_PERSIST_GRACE).await;
        }
        if outcome != ExitPersistWait::Persisted {
            tracing::warn!(
                terminal_id,
                reason = ?outcome,
                budget_ms = EXIT_PERSIST_GRACE.as_millis() as u64,
                "supervisor attach reader did not persist the terminal exit before \
                 teardown; aborting it (terminal exit persistence falls back to the sweep)"
            );
        }
        entry.abort_tasks();
        if outcome == ExitPersistWait::Persisted {
            RendererDropOutcome::ExitPersisted
        } else {
            RendererDropOutcome::Unverified
        }
    }
}

struct EstablishedRenderer {
    entry: RendererEntry,
    handoff: Option<(crate::operation::task_launch::TaskLaunch, u32)>,
}

async fn ensure_entry(
    mut cfg: RendererConfig,
    repo: Option<Arc<dyn RouteRepo>>,
    task_hook: Option<Arc<crate::scheduler::TerminalTaskHook>>,
    launch: Option<crate::operation::task_launch::TaskLaunch>,
) -> anyhow::Result<EstablishedRenderer> {
    use crate::operation::terminal_launch::{self, TerminalStart};
    // Match the absolute endpoint persisted in the one-use launch record.
    if !cfg.supervisor_sock.is_absolute() {
        cfg.supervisor_sock = std::env::current_dir()?.join(&cfg.supervisor_sock);
    }
    let start = match repo.as_deref() {
        Some(repo) => {
            terminal_launch::resolve(repo, &cfg.terminal_id, &cfg.supervisor_sock, launch).await?
        }
        None if launch.is_some() => anyhow::bail!("task launch requires its repository"),
        None => TerminalStart::Unbound,
    };
    let (launch, attach_only) = match start {
        TerminalStart::Fresh(launch) => (Some(*launch), false),
        TerminalStart::Unbound => (None, false),
        TerminalStart::AttachOnly(sock) => {
            cfg.supervisor_sock = sock;
            (None, true)
        }
    };
    // A replay alone cannot reconstruct geometry changes from an earlier
    // server lifetime. Human reattachment remains available; model observation
    // refuses that unproven projection instead of inventing a fresh screen.
    let observation_replay_proven = !attach_only
        && match repo.as_deref() {
            Some(repo) => repo
                .terminal_get(&cfg.terminal_id)
                .await?
                .is_some_and(|terminal| terminal.pid.is_none()),
            None => true,
        };
    let mut handoff = None;
    let proc_id = format!("term:{}", cfg.terminal_id);
    let mut control_conn = match UnixStream::connect(&cfg.supervisor_sock).await {
        Ok(connection) => connection,
        Err(error) => {
            if let (Some(repo), Some(launch)) = (repo.as_deref(), launch.as_ref())
                && let Err(reset_error) = terminal_launch::reset_unissued(repo, launch).await
            {
                tracing::warn!(terminal_id=%cfg.terminal_id, %reset_error, "unable to record terminal request not sent; ownership retained");
            }
            return Err(anyhow::anyhow!(
                "connect proc supervisor {}: {error}",
                cfg.supervisor_sock.display()
            ));
        }
    };
    if !attach_only {
        let request = ControlMsg::EnsureProc(EnsureProcRequest {
            proc_id: proc_id.clone(),
            program: cfg.program.clone(),
            args: cfg.args.clone(),
            envs: cfg.envs.clone(),
            cwd: cfg.cwd.clone(),
            ready_timeout_ms: 0,
            io_mode: IoMode::Pty {
                cols: cfg.cols,
                rows: cfg.rows,
            },
            replay_bytes: cfg.buffer_bytes,
        });
        let launch_sock = cfg.supervisor_sock.clone();
        let launch_proc_id = proc_id.clone();
        let exchange = async move {
            write_frame(&mut control_conn, &request)
                .await
                .map_err(|error| crate::error::CalmError::Internal(error.to_string()))?;
            // Unacknowledged launch cleanup remains in the existing bounded reader.
            let reply = read_control_reply_or_kill(
                &mut control_conn,
                SPAWN_CONTROL_READ_TIMEOUT,
                "spawn",
                &launch_sock,
                &launch_proc_id,
            )
            .await
            .map_err(|error| crate::error::CalmError::Internal(error.to_string()))?;
            Ok((reply, control_conn))
        };
        let (reply, returned_connection) = match launch.as_ref() {
            Some(launch) => {
                let repo = repo
                    .as_deref()
                    .ok_or_else(|| anyhow::anyhow!("task launch requires its repository"))?;
                match launch.clone().run_observed(repo, exchange).await {
                    Ok(reply) => reply,
                    Err(failure) => {
                        if !failure.effect_started
                            && let Err(error) = terminal_launch::reset_unissued(repo, launch).await
                        {
                            tracing::warn!(terminal_id=%cfg.terminal_id, %error, "could not record unissued terminal request; retaining ownership");
                        }
                        if let Some((ControlReply::Spawned { pid }, _connection)) = failure.observed
                            && let Err(error) =
                                repo.terminal_set_pid(&cfg.terminal_id, Some(pid)).await
                        {
                            tracing::warn!(terminal_id=%cfg.terminal_id, pid, %error, "post-ack commit failure: prepared operation retains process ownership");
                        }
                        if failure.effect_started {
                            request_terminal_stop(&cfg.supervisor_sock, &cfg.terminal_id).await;
                        }
                        return Err(failure.error.into());
                    }
                }
            }
            None => exchange.await?,
        };
        control_conn = returned_connection;
        // The admission transaction is over before PID and session persistence.
        match reply {
            ControlReply::Spawned { pid } => {
                if let Some(repo) = repo.as_ref() {
                    match repo.terminal_set_pid(&cfg.terminal_id, Some(pid)).await {
                        Ok(()) => handoff = launch.map(|launch| (launch, pid)),
                        Err(e) => tracing::warn!(terminal_id=%cfg.terminal_id, pid, error=%e,
                            "failed to persist terminal pid after supervisor spawn; ownership retained"),
                    }
                }
            }
            ControlReply::SpawnFailed { error, .. } => anyhow::bail!("{error}"),
            other => anyhow::bail!("unexpected proc-supervisor spawn reply: {other:?}"),
        }
        match read_control_reply(&mut control_conn, SPAWN_CONTROL_READ_TIMEOUT, "ready").await? {
            ControlReply::Ready => {}
            ControlReply::ReadyFailed { error, .. } => anyhow::bail!("{error}"),
            other => anyhow::bail!("unexpected proc-supervisor ready reply: {other:?}"),
        }
    }
    let render_plane: SharedRenderPlane = Arc::new(StdMutex::new(RenderPlane::with_colors(
        cfg.cols,
        cfg.rows,
        cfg.buffer_bytes,
        SCROLLBACK_MAX_LINES,
        Some(cfg.terminal_fg),
        Some(cfg.terminal_bg),
    )));
    let model_view = ModelView::new(cfg.cols, cfg.rows, cfg.terminal_fg, cfg.terminal_bg);
    if !observation_replay_proven {
        model_view
            .lock()
            .unwrap()
            .invalidate("terminal renderer reattached without complete geometry history");
    }

    render_plane
        .lock()
        .unwrap()
        .install_observer(Box::new(model_view::Observer(model_view.clone())));
    let owner_registry: SharedOwnerRegistry = Arc::new(StdMutex::new(OwnerRegistry::new()));
    let exit = Arc::new(StdMutex::new(None));
    let session_id = Uuid::new_v4();
    let (event_tx, initial_event_rx) = broadcast::channel::<DaemonMsg>(2048);
    let event_rx = event_tx.subscribe();
    let (supervisor_tx, supervisor_rx) = mpsc::unbounded_channel::<SupervisorControl>();

    let control_task = control_writer::spawn_supervisor_control_writer(
        control_conn,
        proc_id.clone(),
        supervisor_rx,
    );

    let mut attach_conn = UnixStream::connect(&cfg.supervisor_sock)
        .await
        .map_err(|e| {
            anyhow::anyhow!(
                "connect proc supervisor attach {}: {e}",
                cfg.supervisor_sock.display()
            )
        })?;
    write_frame(
        &mut attach_conn,
        &ControlMsg::Attach(AttachRequest {
            proc_id: proc_id.clone(),
            from_cursor: None,
            reader_id: "calm-server-renderer".into(),
        }),
    )
    .await?;
    let output_capture =
        match read_control_reply(&mut attach_conn, SPAWN_CONTROL_READ_TIMEOUT, "attach").await? {
            ControlReply::AttachOk(Attached {
                cursor_head,
                replay,
                ..
            }) => {
                let output_capture =
                    output_capture::TerminalOutputCapture::shared(cursor_head, &replay);
                if cursor_head != 0 {
                    model_view
                        .lock()
                        .unwrap()
                        .invalidate("supervisor history gap before renderer attach");
                }
                if !replay.is_empty() {
                    let effects = match render_plane.lock() {
                        Ok(mut rp) => rp.on_pty_chunk(replay),
                        Err(_) => Vec::new(),
                    };
                    client_pump::apply_broadcaster_effects(&event_tx, &supervisor_tx, effects);
                }
                output_capture
            }
            ControlReply::Error { message, .. } => anyhow::bail!("{message}"),
            other => anyhow::bail!("unexpected proc-supervisor attach reply: {other:?}"),
        };

    let (exited_tx, exited_rx) = oneshot::channel::<Option<i32>>();
    // Teardown's persistence signal. The sender lives inside the attach reader
    // task, so it is dropped the moment that task ends — which is how
    // `await_exit_persisted` tells "ended without persisting" apart from
    // "still working" (#993 R3-A).
    let (exit_persisted_tx, exit_persisted) = watch::channel(false);
    let attach_task = attach_reader::spawn_supervisor_attach_reader(
        attach_conn,
        proc_id.clone(),
        render_plane.clone(),
        exit.clone(),
        event_tx.clone(),
        supervisor_tx.clone(),
        exited_tx,
        repo,
        cfg.terminal_id.clone(),
        task_hook,
        exit_persisted_tx,
        output_capture,
    );
    let ready_task = child_ready::spawn_child_ready_poller(render_plane.clone(), event_tx.clone());

    Ok(EstablishedRenderer {
        entry: RendererEntry {
            terminal_id: cfg.terminal_id.clone(),
            proc_id,
            supervisor_sock: cfg.supervisor_sock.clone(),
            handle: RendererHandle {
                model_view,
                input_barrier: Arc::new(InputBarrier::default()),
                session_id,
                event_rx,
                event_tx,
                render_plane,
                owner_registry,
                supervisor_tx,
            },
            config: cfg,
            exit,
            signals: SignalRing::new(),
            initial_event_rx: StdMutex::new(Some(initial_event_rx)),
            exited_rx: StdMutex::new(Some(exited_rx)),
            attach_task: StdMutex::new(Some(attach_task)),
            exit_persisted,
            tasks: StdMutex::new(vec![control_task, ready_task]),
        },
        handoff,
    })
}

/// Best-effort exact supervisor request. An acknowledgement proves neither
/// descendant termination nor permission to discard a workspace.
pub(crate) async fn request_terminal_stop(supervisor_sock: &Path, terminal_id: &str) {
    let proc_id = format!("term:{terminal_id}");
    if timeout(
        Duration::from_secs(1),
        signal_child_direct(supervisor_sock, &proc_id, ProcSignal::Kill),
    )
    .await
    .is_err()
    {
        tracing::warn!(
            terminal_id,
            "owned terminal stop request timed out; resources remain retained"
        );
    }
}

async fn read_control_reply_or_kill<R>(
    conn: &mut R,
    read_timeout: Duration,
    what: &str,
    supervisor_sock: &Path,
    proc_id: &str,
) -> anyhow::Result<ControlReply>
where
    R: AsyncRead + Unpin,
{
    match read_control_reply(conn, read_timeout, what).await {
        Ok(reply) => Ok(reply),
        Err(e) => {
            signal_child_direct(supervisor_sock, proc_id, ProcSignal::Kill).await;
            Err(e)
        }
    }
}

async fn read_control_reply<R>(
    conn: &mut R,
    read_timeout: Duration,
    what: &str,
) -> anyhow::Result<ControlReply>
where
    R: AsyncRead + Unpin,
{
    match timeout(read_timeout, read_frame::<ControlReply, _>(conn)).await {
        Ok(Ok(reply)) => Ok(reply),
        Ok(Err(e)) => Err(e.into()),
        Err(_) => {
            anyhow::bail!("proc-supervisor {what} reply timed out after {read_timeout:?}")
        }
    }
}

// Copied from crates/calm-session/src/bin/daemon.rs::signal_child_direct as part of #388 Phase 3a lift. Daemon binary retires in 3c; until then we live with duplication.
async fn signal_child_direct(supervisor_sock: &Path, proc_id: &str, sig: ProcSignal) {
    let mut conn = match UnixStream::connect(supervisor_sock).await {
        Ok(conn) => conn,
        Err(e) => {
            tracing::warn!(
                error = %e,
                sock = %supervisor_sock.display(),
                ?sig,
                "failed to connect proc supervisor for direct signal"
            );
            return;
        }
    };
    if let Err(e) = write_frame(
        &mut conn,
        &ControlMsg::Signal(SignalRequest {
            proc_id: proc_id.to_string(),
            sig,
        }),
    )
    .await
    {
        tracing::warn!(error = %e, ?sig, "failed to send direct supervisor signal");
        return;
    }
    match timeout(
        Duration::from_millis(200),
        read_frame::<ControlReply, _>(&mut conn),
    )
    .await
    {
        Ok(Ok(ControlReply::SignalOk)) => {}
        Ok(Ok(ControlReply::Error { kind, message })) => {
            tracing::debug!(?kind, %message, ?sig, "direct supervisor signal returned error");
        }
        Ok(Ok(other)) => {
            tracing::debug!(reply = ?other, ?sig, "unexpected direct supervisor signal reply");
        }
        Ok(Err(e)) => {
            tracing::debug!(error = %e, ?sig, "failed to read direct supervisor signal reply");
        }
        Err(_) => {
            tracing::debug!(?sig, "timed out reading direct supervisor signal reply");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::net::UnixListener;

    #[tokio::test]
    async fn read_control_reply_or_kill_sends_kill_on_timeout() {
        let dir = calm_test_sockets::socket_dir("tr");
        let sock = calm_test_sockets::socket_path(dir.path(), "supervisor.sock");
        let listener = UnixListener::bind(&sock).expect("bind listener");
        let proc_id = "term:test-timeout".to_string();
        let expected_proc_id = proc_id.clone();
        let accept_task = tokio::spawn(async move {
            let (_silent_stream, _) = listener.accept().await.expect("accept silent connection");
            let (mut signal_conn, _) = listener.accept().await.expect("accept signal connection");
            let msg = timeout(
                Duration::from_millis(500),
                read_frame::<ControlMsg, _>(&mut signal_conn),
            )
            .await
            .expect("signal frame should arrive")
            .expect("read signal frame");
            match msg {
                ControlMsg::Signal(SignalRequest { proc_id, sig }) => {
                    assert_eq!(proc_id, expected_proc_id);
                    assert_eq!(sig, ProcSignal::Kill);
                }
                other => panic!("unexpected control message: {other:?}"),
            }
            write_frame(&mut signal_conn, &ControlReply::SignalOk)
                .await
                .expect("write signal ack");
        });

        let mut conn = UnixStream::connect(&sock).await.expect("connect");
        let err = timeout(
            Duration::from_millis(1000),
            read_control_reply_or_kill(
                &mut conn,
                Duration::from_millis(50),
                "spawn",
                &sock,
                &proc_id,
            ),
        )
        .await
        .expect("read helper should return before outer timeout")
        .expect_err("silent supervisor should time out");

        assert!(
            err.to_string()
                .contains("proc-supervisor spawn reply timed out after 50ms"),
            "unexpected error: {err}"
        );

        timeout(Duration::from_millis(1000), accept_task)
            .await
            .expect("supervisor task should finish")
            .expect("supervisor task should not panic");
    }

    #[tokio::test]
    async fn read_control_reply_times_out_when_supervisor_is_silent() {
        let dir = calm_test_sockets::socket_dir("tr");
        let sock = calm_test_sockets::socket_path(dir.path(), "supervisor.sock");
        let listener = UnixListener::bind(&sock).expect("bind listener");
        let accept_task = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.expect("accept connection");
            tokio::time::sleep(Duration::from_secs(1)).await;
            drop(stream);
        });

        let mut conn = UnixStream::connect(&sock).await.expect("connect");
        let err = timeout(
            Duration::from_millis(500),
            read_control_reply(&mut conn, Duration::from_millis(50), "spawn"),
        )
        .await
        .expect("read helper should return before outer timeout")
        .expect_err("silent supervisor should time out");

        assert!(
            err.to_string()
                .contains("proc-supervisor spawn reply timed out after 50ms"),
            "unexpected error: {err}"
        );

        accept_task.abort();
        let _ = accept_task.await;
    }
}
