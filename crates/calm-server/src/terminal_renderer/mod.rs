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

    fn abort_tasks(&self) {
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
        tokio::time::sleep(Duration::from_millis(200)).await;
        entry.shutdown_signal(ProcSignal::Kill).await;
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
    match read_control_reply_or_kill(
        &mut control_conn,
        SPAWN_CONTROL_READ_TIMEOUT,
        "ready",
        &cfg.supervisor_sock,
        &proc_id,
    )
    .await?
    {
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
    match read_control_reply_or_kill(
        &mut attach_conn,
        SPAWN_CONTROL_READ_TIMEOUT,
        "attach",
        &cfg.supervisor_sock,
        &proc_id,
    )
    .await?
    {
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
        tasks: StdMutex::new(vec![control_task, attach_task, ready_task]),
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
