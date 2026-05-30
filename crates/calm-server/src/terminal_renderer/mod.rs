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

pub(crate) const SCROLLBACK_MAX_LINES: usize = 2000;

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
    /// Set exactly once when the supervisor's attach stream delivers
    /// `Exited`. Late client pumps replay this immediately after
    /// `ServerHello` because broadcast receivers do not retain history.
    pub exit: SharedExitState,
    initial_event_rx: StdMutex<Option<broadcast::Receiver<DaemonMsg>>>,
    exited_rx: StdMutex<Option<oneshot::Receiver<Option<i32>>>>,
    tasks: StdMutex<Vec<JoinHandle<()>>>,
}

impl RendererEntry {
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
}

impl TerminalRendererRegistry {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            entries: StdMutex::new(HashMap::new()),
            repo: None,
        })
    }

    pub fn new_with_repo(repo: Arc<dyn RouteRepo>) -> Arc<Self> {
        Arc::new(Self {
            entries: StdMutex::new(HashMap::new()),
            repo: Some(repo),
        })
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

        let entry = Arc::new(ensure_entry(cfg, self.repo.clone()).await?);
        let mut entries = self
            .entries
            .lock()
            .map_err(|_| anyhow::anyhow!("terminal renderer registry mutex poisoned"))?;
        if let Some(existing) = entries.get(&entry.terminal_id) {
            entry.abort_tasks();
            Ok(existing.clone())
        } else {
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

        entry.shutdown_signal(ProcSignal::Term).await;
        tokio::time::sleep(Duration::from_millis(200)).await;
        entry.shutdown_signal(ProcSignal::Kill).await;
        entry.abort_tasks();
    }
}

async fn ensure_entry(cfg: RendererConfig, repo: Option<Arc<dyn RouteRepo>>) -> anyhow::Result<RendererEntry> {
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
            program: cfg.program,
            args: cfg.args,
            envs: cfg.envs,
            cwd: cfg.cwd,
            ready_timeout_ms: 0,
            io_mode: IoMode::Pty {
                cols: cfg.cols,
                rows: cfg.rows,
            },
            replay_bytes: cfg.buffer_bytes,
        }),
    )
    .await?;
    match read_frame(&mut control_conn).await? {
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
    match read_frame(&mut control_conn).await? {
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
    match read_frame(&mut attach_conn).await? {
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
    );
    let ready_task = child_ready::spawn_child_ready_poller(render_plane.clone(), event_tx.clone());

    Ok(RendererEntry {
        terminal_id: cfg.terminal_id,
        proc_id,
        supervisor_sock: cfg.supervisor_sock,
        handle: RendererHandle {
            session_id,
            event_rx,
            event_tx,
            render_plane,
            owner_registry,
            supervisor_tx,
        },
        exit,
        initial_event_rx: StdMutex::new(Some(initial_event_rx)),
        exited_rx: StdMutex::new(Some(exited_rx)),
        tasks: StdMutex::new(vec![control_task, attach_task, ready_task]),
    })
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
