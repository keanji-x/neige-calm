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
use tokio::sync::{broadcast, mpsc, oneshot};
use tokio::task::JoinHandle;
use tokio::time::timeout;
use uuid::Uuid;

mod attach_reader;
mod child_ready;
mod client_pump;
mod control_writer;
mod snapshot;

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
/// An upper bound, not a fixed sleep: teardown waits on the attach reader and
/// proceeds to the SIGKILL as soon as the child's exit has been observed (#993
/// F3).
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
/// + `terminal_set_exit` + `session_projection_complete_for_terminal`
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
    initial_event_rx: StdMutex<Option<broadcast::Receiver<DaemonMsg>>>,
    exited_rx: StdMutex<Option<oneshot::Receiver<Option<i32>>>>,
    /// Held apart from `tasks` because teardown must *wait* for it rather than
    /// abort it: this is the task that persists the terminal exit (#993 R1).
    attach_task: StdMutex<Option<JoinHandle<()>>>,
    tasks: StdMutex<Vec<JoinHandle<()>>>,
}

impl RendererEntry {
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

    /// Waits (bounded) for the supervisor attach reader to finish on its own.
    ///
    /// It breaks out of its loop right after handling `Exited` — which is also
    /// where it persists the terminal's exit — so completing normally is the
    /// signal that the exit landed. Returns `true` in that case.
    ///
    /// On timeout the handle is put back (so a later stage can keep waiting and
    /// `abort_tasks` can still abort it) and `false` is returned; the caller
    /// decides whether to escalate. Never aborts on its own.
    async fn await_exit_persisted(&self, budget: Duration) -> bool {
        let handle = self
            .attach_task
            .lock()
            .ok()
            .and_then(|mut guard| guard.take());
        let Some(mut handle) = handle else {
            return true;
        };
        if tokio::time::timeout(budget, &mut handle).await.is_ok() {
            return true;
        }
        if let Ok(mut guard) = self.attach_task.lock() {
            *guard = Some(handle);
        }
        false
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
}

impl TerminalRendererRegistry {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            entries: StdMutex::new(HashMap::new()),
            repo: None,
            task_hook: StdMutex::new(None),
        })
    }

    pub fn new_with_repo(repo: Arc<dyn RouteRepo>) -> Arc<Self> {
        Arc::new(Self {
            entries: StdMutex::new(HashMap::new()),
            repo: Some(repo),
            task_hook: StdMutex::new(None),
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

    /// Spawn a PTY proc on the supervisor and stand up the in-process
    /// renderer. Returns a handle the WS pump (in 3b) will use.
    pub async fn ensure(
        &self,
        cfg: RendererConfig,
    ) -> Result<Arc<RendererEntry>, RendererSpawnError> {
        if let Some(existing) = self.get(&cfg.terminal_id) {
            return Ok(existing);
        }

        let entry = Arc::new(ensure_entry(cfg, self.repo.clone(), self.task_hook()).await?);
        let mut entries = self
            .entries
            .lock()
            .map_err(|_| anyhow::anyhow!("terminal renderer registry mutex poisoned"))?;
        if let Some(existing) = entries.get(&entry.terminal_id) {
            entry.abort_tasks();
            Ok(existing.clone())
        } else {
            tracing::info!(
                terminal_id = %entry.terminal_id,
                "terminal renderer registry inserted entry"
            );
            entries.insert(entry.terminal_id.clone(), entry.clone());
            Ok(entry)
        }
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
        let owner_registry: SharedOwnerRegistry = Arc::new(StdMutex::new(OwnerRegistry::new()));
        let exit = Arc::new(StdMutex::new(None));
        let session_id = Uuid::new_v4();
        let (event_tx, initial_event_rx) = broadcast::channel::<DaemonMsg>(2048);
        let event_rx = event_tx.subscribe();
        let (supervisor_tx, _supervisor_rx) = mpsc::unbounded_channel::<SupervisorControl>();
        let (_exited_tx, exited_rx) = oneshot::channel::<Option<i32>>();
        let entry = Arc::new(RendererEntry {
            terminal_id: cfg.terminal_id.clone(),
            proc_id: format!("term:{}", cfg.terminal_id),
            supervisor_sock: cfg.supervisor_sock.clone(),
            handle: RendererHandle {
                session_id,
                event_rx,
                event_tx,
                render_plane,
                owner_registry,
                supervisor_tx,
            },
            config: cfg,
            exit,
            initial_event_rx: StdMutex::new(Some(initial_event_rx)),
            exited_rx: StdMutex::new(Some(exited_rx)),
            attach_task: StdMutex::new(None),
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
    ///   unconditional `sleep(TERM_TO_KILL_GRACE)` is gone.
    /// * Worst case (child ignores SIGTERM *and* a grandchild holds the pty
    ///   slave past the supervisor's drain grace):
    ///   `TERM_TO_KILL_GRACE + EXIT_PERSIST_GRACE` = **1.2 s**.
    ///
    /// The batch teardown paths — `routes::waves` wave-delete,
    /// `terminal_sweeper::sweep`, `routes::coves` — call this serially, once
    /// per terminal, so a wave of N terminals costs up to `N * 1.2 s` in the
    /// worst case. That is accepted deliberately rather than fanned out: those
    /// loops interleave per-card repo writes and event emission whose ordering
    /// the callers rely on, and every one of them is a rare
    /// administrative/janitor path with no interactive latency budget. The
    /// worst case also requires a wedged child *per terminal*; the realistic
    /// batch cost is N × single-digit ms. If a fan-out ever becomes necessary,
    /// the unit to parallelise is this call, not the surrounding repo work.
    pub async fn drop_entry(&self, terminal_id: &str) {
        let entry = self
            .entries
            .lock()
            .ok()
            .and_then(|mut entries| entries.remove(terminal_id));
        let Some(entry) = entry else {
            return;
        };

        tracing::info!(terminal_id, "terminal renderer registry dropping entry");
        entry.shutdown_signal(ProcSignal::Term).await;
        // Wait on the attach reader instead of sleeping the full grace: a child
        // that dies on SIGTERM cuts the teardown from a fixed 200ms to
        // single-digit ms, which is what keeps the serial batch paths cheap
        // (#993 F3). A child that ignores SIGTERM costs exactly the old 200ms.
        let persisted = entry.await_exit_persisted(TERM_TO_KILL_GRACE).await;
        // Unconditional, even when the child is already gone: this signals the
        // whole *process group*, so it is also what reaps group members that
        // outlived the SIGTERM. Skipping it would leak them.
        entry.shutdown_signal(ProcSignal::Kill).await;
        // #993 R1: `Exited` can lag the kill by up to the supervisor's pty
        // drain grace, and the attach reader is what persists it. Let it
        // finish before the abort.
        if !persisted && !entry.await_exit_persisted(EXIT_PERSIST_GRACE).await {
            tracing::warn!(
                terminal_id,
                budget_ms = EXIT_PERSIST_GRACE.as_millis() as u64,
                "supervisor attach reader did not observe Exited before teardown; \
                 aborting it (terminal exit persistence falls back to the sweep)"
            );
        }
        entry.abort_tasks();
    }
}

async fn ensure_entry(
    cfg: RendererConfig,
    repo: Option<Arc<dyn RouteRepo>>,
    task_hook: Option<Arc<crate::scheduler::TerminalTaskHook>>,
) -> anyhow::Result<RendererEntry> {
    let proc_id = format!("term:{}", cfg.terminal_id);
    let mut control_conn = UnixStream::connect(&cfg.supervisor_sock)
        .await
        .map_err(|e| {
            anyhow::anyhow!(
                "connect proc supervisor {}: {e}",
                cfg.supervisor_sock.display()
            )
        })?;
    write_frame(
        &mut control_conn,
        &ControlMsg::EnsureProc(EnsureProcRequest {
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
        }),
    )
    .await?;
    // Kill-by-proc_id is only needed before Spawned{pid} is persisted: the
    // compensation/sweeper paths reap by persisted pid, so they cannot reach an
    // orphaned unacknowledged child. After this point, those existing
    // reconciliation paths handle ready/attach abandonment; killing here would
    // leave a dead-but-row-live terminal with no attach reader to observe exit.
    match read_control_reply_or_kill(
        &mut control_conn,
        SPAWN_CONTROL_READ_TIMEOUT,
        "spawn",
        &cfg.supervisor_sock,
        &proc_id,
    )
    .await?
    {
        ControlReply::Spawned { pid } => {
            if let Some(repo) = repo.as_ref()
                && let Err(e) = repo.terminal_set_pid(&cfg.terminal_id, Some(pid)).await
            {
                tracing::warn!(
                    terminal_id = %cfg.terminal_id,
                    pid,
                    error = %e,
                    "failed to persist terminal pid after supervisor spawn"
                );
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

    let render_plane: SharedRenderPlane = Arc::new(StdMutex::new(RenderPlane::with_colors(
        cfg.cols,
        cfg.rows,
        cfg.buffer_bytes,
        SCROLLBACK_MAX_LINES,
        Some(cfg.terminal_fg),
        Some(cfg.terminal_bg),
    )));
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
    match read_control_reply(&mut attach_conn, SPAWN_CONTROL_READ_TIMEOUT, "attach").await? {
        ControlReply::AttachOk(Attached { replay, .. }) => {
            if !replay.is_empty() {
                let effects = match render_plane.lock() {
                    Ok(mut rp) => rp.on_pty_chunk(replay),
                    Err(_) => Vec::new(),
                };
                client_pump::apply_broadcaster_effects(&event_tx, &supervisor_tx, effects);
            }
        }
        ControlReply::Error { message, .. } => anyhow::bail!("{message}"),
        other => anyhow::bail!("unexpected proc-supervisor attach reply: {other:?}"),
    }

    let (exited_tx, exited_rx) = oneshot::channel::<Option<i32>>();
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
    );
    let ready_task = child_ready::spawn_child_ready_poller(render_plane.clone(), event_tx.clone());

    Ok(RendererEntry {
        terminal_id: cfg.terminal_id.clone(),
        proc_id,
        supervisor_sock: cfg.supervisor_sock.clone(),
        handle: RendererHandle {
            session_id,
            event_rx,
            event_tx,
            render_plane,
            owner_registry,
            supervisor_tx,
        },
        config: cfg,
        exit,
        initial_event_rx: StdMutex::new(Some(initial_event_rx)),
        exited_rx: StdMutex::new(Some(exited_rx)),
        attach_task: StdMutex::new(Some(attach_task)),
        tasks: StdMutex::new(vec![control_task, ready_task]),
    })
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
    use tempfile::tempdir;
    use tokio::net::UnixListener;

    #[tokio::test]
    async fn read_control_reply_or_kill_sends_kill_on_timeout() {
        let dir = tempdir().expect("tempdir");
        let sock = dir.path().join("supervisor.sock");
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
        let dir = tempdir().expect("tempdir");
        let sock = dir.path().join("supervisor.sock");
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
