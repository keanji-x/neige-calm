use calm_session::control::{
    AttachRequest, Attached, CleanupRequest, ControlErrorKind, ControlMsg, ControlReply,
    EnsureProcRequest, IoMode, ProbeRequest, ProcSignal, ResizePtyRequest, SignalRequest,
    WriteStdinRequest,
};
use calm_session::{FrameError, read_frame, write_frame};
use portable_pty::{CommandBuilder, MasterPty, PtySize as PtPtySize, native_pty_system};
use std::collections::{HashMap, VecDeque};
use std::io::{self, Read as _, Write as _};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::sync::Mutex as StdMutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;
use tokio::io::unix::AsyncFd;
use tokio::net::{UnixListener, UnixStream};
use tokio::process::{Child, Command};
use tokio::sync::{Mutex, broadcast, oneshot};

const DAEMON_READY_SIGNAL: &[u8] = b"ready\n";
const DAEMON_READY_MAX_BYTES: usize = 64;

#[derive(Clone)]
pub struct ProcRegistry {
    inner: Arc<StdMutex<HashMap<String, Arc<ProcEntry>>>>,
    reap_children: bool,
}

struct ProcEntry {
    pid: u32,
    io_mode: IoMode,
    runtime: ProcRuntime,
    byte_ring: StdMutex<ByteRing>,
    cursor_tail: AtomicU64,
    cursor_head: AtomicU64,
    exit: StdMutex<Option<ProcExit>>,
    broadcast_tx: broadcast::Sender<DataFrame>,
}

enum ProcRuntime {
    Pipe {
        child: Arc<Mutex<Child>>,
    },
    Pty {
        master: Arc<StdMutex<Box<dyn MasterPty + Send>>>,
        writer: Arc<StdMutex<Box<dyn io::Write + Send>>>,
    },
}

#[derive(Clone, Debug)]
struct ProcExit {
    status: Option<i32>,
    signalled: bool,
    cursor: u64,
}

#[derive(Clone, Debug)]
enum DataFrame {
    Output { cursor: u64, bytes: Vec<u8> },
    Exited(ProcExit),
}

struct ByteRing {
    capacity: usize,
    chunks: VecDeque<(u64, Vec<u8>)>,
    cursor_tail: u64,
    cursor_head: u64,
}

enum ByteRingSlice {
    Replay {
        cursor_head: u64,
        cursor_tail: u64,
        bytes: Vec<u8>,
    },
    Gap {
        cursor_head: u64,
        cursor_tail: u64,
    },
}

impl ByteRing {
    fn new(capacity: usize) -> Self {
        Self {
            capacity,
            chunks: VecDeque::new(),
            cursor_tail: 0,
            cursor_head: 0,
        }
    }

    fn append(&mut self, bytes: Vec<u8>) -> (u64, u64) {
        let start = self.cursor_tail;
        self.cursor_tail = self.cursor_tail.saturating_add(bytes.len() as u64);
        self.chunks.push_back((start, bytes));
        while self.buffered_len() > self.capacity && self.chunks.len() > 1 {
            let (_, dropped) = self.chunks.pop_front().expect("chunk");
            self.cursor_head = self.cursor_head.saturating_add(dropped.len() as u64);
        }
        if self.capacity == 0 {
            self.chunks.clear();
            self.cursor_head = self.cursor_tail;
        }
        (start, self.cursor_tail)
    }

    fn slice_from(&self, cursor: u64) -> ByteRingSlice {
        if cursor < self.cursor_head {
            return ByteRingSlice::Gap {
                cursor_head: self.cursor_head,
                cursor_tail: self.cursor_tail,
            };
        }
        let mut out = Vec::with_capacity((self.cursor_tail.saturating_sub(cursor)) as usize);
        for (start, chunk) in &self.chunks {
            let end = start.saturating_add(chunk.len() as u64);
            if end <= cursor {
                continue;
            }
            let offset = cursor.saturating_sub(*start) as usize;
            out.extend_from_slice(&chunk[offset..]);
        }
        ByteRingSlice::Replay {
            cursor_head: self.cursor_head,
            cursor_tail: self.cursor_tail,
            bytes: out,
        }
    }

    fn window(&self) -> (u64, u64) {
        (self.cursor_head, self.cursor_tail)
    }

    fn buffered_len(&self) -> usize {
        self.chunks.iter().map(|(_, chunk)| chunk.len()).sum()
    }
}

#[derive(Debug)]
pub struct EnsureProcFailure {
    pub error: String,
    pub child_already_reaped: bool,
}

impl ProcRegistry {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(StdMutex::new(HashMap::new())),
            reap_children: true,
        }
    }

    fn without_reaper() -> Self {
        Self {
            inner: Arc::new(StdMutex::new(HashMap::new())),
            reap_children: false,
        }
    }

    pub async fn live_pids(&self) -> Vec<u32> {
        self.inner
            .lock()
            .map(|entries| entries.values().map(|entry| entry.pid).collect())
            .unwrap_or_default()
    }

    pub async fn terminate_all_process_groups(&self) {
        self.terminate_all_process_groups_sync();
    }

    pub fn terminate_all_process_groups_sync(&self) {
        let entries: Vec<Arc<ProcEntry>> = self
            .inner
            .lock()
            .map(|entries| entries.values().cloned().collect())
            .unwrap_or_default();
        let pgids: Vec<i32> = entries
            .iter()
            .filter(|entry| {
                entry
                    .exit
                    .lock()
                    .map(|exit| exit.is_none())
                    .unwrap_or(false)
            })
            .filter_map(|entry| current_process_group(entry).ok())
            .collect();
        for pgid in pgids {
            #[cfg(unix)]
            unsafe {
                let _ = libc::kill(-(pgid as libc::pid_t), libc::SIGTERM);
            }
        }
    }
}

impl Default for ProcRegistry {
    fn default() -> Self {
        Self::new()
    }
}

pub async fn serve_control_socket(
    control_sock: PathBuf,
    registry: ProcRegistry,
    shutdown: oneshot::Receiver<()>,
) -> anyhow::Result<()> {
    let listener = bind_control_listener(&control_sock)?;
    serve_with_listener(listener, control_sock, registry, shutdown).await
}

/// Binds the control listener synchronously. Used by both the production
/// `serve_control_socket` path and the test fixture's synchronous start
/// (which needs the socket to be reachable before returning, eliminating
/// the listen-race window under heavy parallel test load).
pub fn bind_control_listener(control_sock: &Path) -> anyhow::Result<UnixListener> {
    if let Some(parent) = control_sock.parent() {
        std::fs::create_dir_all(parent)?;
    }
    if control_sock.exists() {
        let _ = std::fs::remove_file(control_sock);
    }
    Ok(UnixListener::bind(control_sock)?)
}

pub async fn serve_with_listener(
    listener: UnixListener,
    control_sock: PathBuf,
    registry: ProcRegistry,
    mut shutdown: oneshot::Receiver<()>,
) -> anyhow::Result<()> {
    tracing::info!(
        control_sock = %control_sock.display(),
        "calm-proc-supervisor listening"
    );
    loop {
        tokio::select! {
            _ = &mut shutdown => {
                break;
            }
            accepted = listener.accept() => {
                let (stream, _) = accepted?;
                let registry = registry.clone();
                tokio::spawn(async move {
                    if let Err(err) = handle_connection(stream, registry).await {
                        tracing::warn!(error = %err, "control connection failed");
                    }
                });
            }
        }
    }
    let _ = std::fs::remove_file(control_sock);
    Ok(())
}

async fn handle_connection(mut stream: UnixStream, registry: ProcRegistry) -> anyhow::Result<()> {
    loop {
        let msg: ControlMsg = match read_frame(&mut stream).await {
            Ok(msg) => msg,
            Err(FrameError::Io(e)) if e.kind() == io::ErrorKind::UnexpectedEof => break,
            Err(err) => return Err(err.into()),
        };
        match msg {
            ControlMsg::EnsureProc(request) => {
                // Idempotent fast path: a live proc with this id is already
                // past readiness, so emit Spawned+Ready immediately.
                if let Some(pid) = existing_live_pid(&registry, &request.proc_id).await {
                    write_frame(&mut stream, &ControlReply::Spawned { pid }).await?;
                    write_frame(&mut stream, &ControlReply::Ready).await?;
                    continue;
                }
                match try_spawn(registry.clone(), request).await {
                    Err(err) => {
                        write_frame(
                            &mut stream,
                            &ControlReply::SpawnFailed {
                                error: err.error,
                                child_already_reaped: err.child_already_reaped,
                            },
                        )
                        .await?;
                    }
                    Ok(spawned) => {
                        write_frame(&mut stream, &ControlReply::Spawned { pid: spawned.pid })
                            .await?;
                        match await_ready_phase(spawned).await {
                            Ok(_pid) => {
                                write_frame(&mut stream, &ControlReply::Ready).await?;
                            }
                            Err(err) => {
                                write_frame(
                                    &mut stream,
                                    &ControlReply::ReadyFailed {
                                        error: err.error,
                                        child_already_reaped: err.child_already_reaped,
                                    },
                                )
                                .await?;
                            }
                        }
                    }
                }
            }
            ControlMsg::Attach(request) => {
                handle_attach(stream, registry, request).await?;
                return Ok(());
            }
            ControlMsg::WriteStdin(request) => {
                handle_write_stdin(&mut stream, registry.clone(), request).await?;
            }
            ControlMsg::ResizePty(request) => {
                handle_resize_pty(&mut stream, registry.clone(), request).await?;
            }
            ControlMsg::Signal(request) => {
                handle_signal(&mut stream, registry.clone(), request).await?;
            }
            ControlMsg::Cleanup(request) => {
                handle_cleanup(&mut stream, registry.clone(), request).await?;
            }
            ControlMsg::Probe(request) => {
                handle_probe(&mut stream, registry.clone(), request).await?;
            }
        }
    }
    Ok(())
}

/// Single-shot variant: combines try_spawn + await_ready_phase. Kept
/// out of the connection-level path (which streams Spawned+Ready/Failed
/// separately so the client can persist pid+handle between frames) but
/// exposed for tests that don't care about the two-phase shape.
#[doc(hidden)]
pub async fn ensure_proc_impl(
    registry: ProcRegistry,
    request: EnsureProcRequest,
) -> Result<u32, EnsureProcFailure> {
    if let Some(pid) = existing_live_pid(&registry, &request.proc_id).await {
        return Ok(pid);
    }
    let spawned = try_spawn(registry, request).await?;
    await_ready_phase(spawned).await
}

async fn lookup_proc(
    registry: &ProcRegistry,
    proc_id: &str,
) -> Result<Arc<ProcEntry>, ControlReply> {
    registry
        .inner
        .lock()
        .map_err(|_| ControlReply::Error {
            kind: ControlErrorKind::Internal,
            message: "proc registry mutex poisoned".into(),
        })?
        .get(proc_id)
        .cloned()
        .ok_or_else(|| ControlReply::Error {
            kind: ControlErrorKind::UnknownProc,
            message: format!("unknown proc_id {proc_id}"),
        })
}

async fn handle_attach(
    mut stream: UnixStream,
    registry: ProcRegistry,
    request: AttachRequest,
) -> anyhow::Result<()> {
    let entry = match lookup_proc(&registry, &request.proc_id).await {
        Ok(entry) => entry,
        Err(reply) => {
            write_frame(&mut stream, &reply).await?;
            return Ok(());
        }
    };
    if !matches!(entry.io_mode, IoMode::Pty { .. }) {
        write_frame(
            &mut stream,
            &ControlReply::Error {
                kind: ControlErrorKind::WrongState,
                message: format!("proc {} is not pty-backed", request.proc_id),
            },
        )
        .await?;
        return Ok(());
    }

    let mut rx = entry.broadcast_tx.subscribe();
    let mut requested_gap = None;
    let attached = {
        let ring = entry
            .byte_ring
            .lock()
            .map_err(|_| anyhow::anyhow!("byte ring mutex poisoned"))?;
        let (head, _) = ring.window();
        let requested = request.from_cursor.unwrap_or(head);
        match ring.slice_from(requested) {
            ByteRingSlice::Replay {
                cursor_head,
                cursor_tail,
                bytes,
            } => Attached {
                proc_id: request.proc_id.clone(),
                running: entry
                    .exit
                    .lock()
                    .map(|exit| exit.is_none())
                    .unwrap_or(false),
                cursor_head,
                cursor_tail,
                replay: bytes,
            },
            ByteRingSlice::Gap {
                cursor_head,
                cursor_tail,
            } => {
                requested_gap = Some((cursor_head, requested));
                let replay = match ring.slice_from(cursor_head) {
                    ByteRingSlice::Replay { bytes, .. } => bytes,
                    ByteRingSlice::Gap { .. } => Vec::new(),
                };
                Attached {
                    proc_id: request.proc_id.clone(),
                    running: entry
                        .exit
                        .lock()
                        .map(|exit| exit.is_none())
                        .unwrap_or(false),
                    cursor_head,
                    cursor_tail,
                    replay,
                }
            }
        }
    };
    let snapshot_tail = attached.cursor_tail;
    write_frame(&mut stream, &ControlReply::AttachOk(attached)).await?;
    if let Some((earliest_cursor, requested_cursor)) = requested_gap {
        write_frame(
            &mut stream,
            &ControlReply::Gap {
                earliest_cursor,
                requested_cursor,
            },
        )
        .await?;
    }
    let sticky_exit = entry.exit.lock().ok().and_then(|exit| exit.clone());
    if let Some(exit) = sticky_exit
        && exit.cursor <= snapshot_tail
    {
        write_frame(
            &mut stream,
            &ControlReply::Exited {
                proc_id: request.proc_id,
                status: exit.status,
                signalled: exit.signalled,
                cursor: exit.cursor,
            },
        )
        .await?;
        return Ok(());
    }

    loop {
        match rx.recv().await {
            Ok(DataFrame::Output { cursor, mut bytes }) => {
                let frame_tail = cursor.saturating_add(bytes.len() as u64);
                if frame_tail <= snapshot_tail {
                    continue;
                }
                let cursor = if cursor < snapshot_tail {
                    let skip = (snapshot_tail - cursor) as usize;
                    bytes = bytes.split_off(skip);
                    snapshot_tail
                } else {
                    cursor
                };
                write_frame(
                    &mut stream,
                    &ControlReply::Output {
                        proc_id: request.proc_id.clone(),
                        cursor,
                        bytes,
                    },
                )
                .await?;
            }
            Ok(DataFrame::Exited(exit)) => {
                write_frame(
                    &mut stream,
                    &ControlReply::Exited {
                        proc_id: request.proc_id.clone(),
                        status: exit.status,
                        signalled: exit.signalled,
                        cursor: exit.cursor,
                    },
                )
                .await?;
                break;
            }
            Err(broadcast::error::RecvError::Lagged(_)) => {
                let earliest_cursor = entry.cursor_head.load(Ordering::SeqCst);
                write_frame(
                    &mut stream,
                    &ControlReply::Gap {
                        earliest_cursor,
                        requested_cursor: earliest_cursor,
                    },
                )
                .await?;
            }
            Err(broadcast::error::RecvError::Closed) => break,
        }
    }
    Ok(())
}

async fn handle_write_stdin(
    stream: &mut UnixStream,
    registry: ProcRegistry,
    request: WriteStdinRequest,
) -> anyhow::Result<()> {
    let entry = match lookup_proc(&registry, &request.proc_id).await {
        Ok(entry) => entry,
        Err(reply) => {
            write_frame(stream, &reply).await?;
            return Ok(());
        }
    };
    let ProcRuntime::Pty { writer, .. } = &entry.runtime else {
        write_frame(
            stream,
            &ControlReply::Error {
                kind: ControlErrorKind::WrongState,
                message: format!("proc {} is not pty-backed", request.proc_id),
            },
        )
        .await?;
        return Ok(());
    };
    let writer = writer.clone();
    let bytes = request.bytes;
    let write_res = tokio::task::spawn_blocking(move || {
        let mut writer = writer
            .lock()
            .map_err(|_| io::Error::other("pty writer mutex poisoned"))?;
        writer.write_all(&bytes)?;
        writer.flush()
    })
    .await
    .map_err(|e| anyhow::anyhow!("join pty write task: {e}"))?;
    if let Err(e) = write_res {
        write_frame(
            stream,
            &ControlReply::Error {
                kind: ControlErrorKind::Internal,
                message: format!("write pty stdin for {}: {e}", request.proc_id),
            },
        )
        .await?;
        return Ok(());
    }
    if let Some(write_seq) = request.write_seq {
        write_frame(stream, &ControlReply::WriteAck { write_seq }).await?;
    }
    Ok(())
}

async fn handle_resize_pty(
    stream: &mut UnixStream,
    registry: ProcRegistry,
    request: ResizePtyRequest,
) -> anyhow::Result<()> {
    let entry = match lookup_proc(&registry, &request.proc_id).await {
        Ok(entry) => entry,
        Err(reply) => {
            write_frame(stream, &reply).await?;
            return Ok(());
        }
    };
    let ProcRuntime::Pty { master, .. } = &entry.runtime else {
        write_frame(
            stream,
            &ControlReply::Error {
                kind: ControlErrorKind::WrongState,
                message: format!("proc {} is not pty-backed", request.proc_id),
            },
        )
        .await?;
        return Ok(());
    };
    let res = master
        .lock()
        .map_err(|_| anyhow::anyhow!("pty master mutex poisoned"))?
        .resize(PtPtySize {
            cols: request.cols,
            rows: request.rows,
            pixel_width: request.pixel_w,
            pixel_height: request.pixel_h,
        });
    match res {
        Ok(()) => write_frame(stream, &ControlReply::ResizeOk).await?,
        Err(e) => {
            write_frame(
                stream,
                &ControlReply::Error {
                    kind: ControlErrorKind::Internal,
                    message: format!("resize pty for {}: {e}", request.proc_id),
                },
            )
            .await?;
        }
    }
    Ok(())
}

async fn handle_signal(
    stream: &mut UnixStream,
    registry: ProcRegistry,
    request: SignalRequest,
) -> anyhow::Result<()> {
    let entry = match lookup_proc(&registry, &request.proc_id).await {
        Ok(entry) => entry,
        Err(reply) => {
            write_frame(stream, &reply).await?;
            return Ok(());
        }
    };
    let sig = match request.sig {
        ProcSignal::Term => libc::SIGTERM,
        ProcSignal::Kill => libc::SIGKILL,
        ProcSignal::Hup => libc::SIGHUP,
    };
    let pgid = current_process_group(&entry)?;
    let rc = unsafe { libc::kill(-(pgid as libc::pid_t), sig) };
    if rc == 0 {
        write_frame(stream, &ControlReply::SignalOk).await?;
    } else {
        write_frame(
            stream,
            &ControlReply::Error {
                kind: ControlErrorKind::Internal,
                message: format!(
                    "signal proc {} pgid {}: {}",
                    request.proc_id,
                    pgid,
                    io::Error::last_os_error()
                ),
            },
        )
        .await?;
    }
    Ok(())
}

fn current_process_group(entry: &ProcEntry) -> anyhow::Result<i32> {
    match &entry.runtime {
        ProcRuntime::Pipe { .. } => Ok(entry.pid as i32),
        ProcRuntime::Pty { master, .. } => {
            let master = master
                .lock()
                .map_err(|_| anyhow::anyhow!("pty master mutex poisoned"))?;
            Ok(master
                .process_group_leader()
                .unwrap_or(entry.pid as libc::pid_t))
        }
    }
}

async fn handle_cleanup(
    stream: &mut UnixStream,
    registry: ProcRegistry,
    request: CleanupRequest,
) -> anyhow::Result<()> {
    let entry = match lookup_proc(&registry, &request.proc_id).await {
        Ok(entry) => entry,
        Err(reply) => {
            write_frame(stream, &reply).await?;
            return Ok(());
        }
    };
    if entry.exit.lock().map(|exit| exit.is_none()).unwrap_or(true) {
        write_frame(
            stream,
            &ControlReply::Error {
                kind: ControlErrorKind::WrongState,
                message: format!("proc {} is still running", request.proc_id),
            },
        )
        .await?;
        return Ok(());
    }
    registry
        .inner
        .lock()
        .map(|mut entries| entries.remove(&request.proc_id))
        .ok();
    write_frame(stream, &ControlReply::CleanupOk).await?;
    Ok(())
}

async fn handle_probe(
    stream: &mut UnixStream,
    registry: ProcRegistry,
    request: ProbeRequest,
) -> anyhow::Result<()> {
    let proc_running = match lookup_proc(&registry, &request.proc_id).await {
        Ok(entry) => entry
            .exit
            .lock()
            .map(|exit| exit.is_none())
            .unwrap_or(false),
        Err(_) => false,
    };
    write_frame(
        stream,
        &ControlReply::ProbeOk {
            supervisor_version: 1,
            proc_running,
        },
    )
    .await?;
    Ok(())
}

struct Spawned {
    proc_id: String,
    pid: u32,
    pipe_child: Option<Arc<Mutex<Child>>>,
    ready_reader: Option<AsyncFd<OwnedFd>>,
    ready_timeout: Duration,
    registry: ProcRegistry,
}

async fn try_spawn(
    registry: ProcRegistry,
    request: EnsureProcRequest,
) -> Result<Spawned, EnsureProcFailure> {
    match request.io_mode.clone() {
        IoMode::Pipe => try_spawn_pipe(registry, request).await,
        IoMode::Pty { cols, rows } => try_spawn_pty(registry, request, cols, rows).await,
    }
}

async fn try_spawn_pipe(
    registry: ProcRegistry,
    request: EnsureProcRequest,
) -> Result<Spawned, EnsureProcFailure> {
    if let Some(sock) = sock_arg(&request.args) {
        let _ = std::fs::remove_file(exit_sidecar_path(&sock));
    }

    let (ready_reader, ready_writer) = ready_pipe().map_err(|e| EnsureProcFailure {
        error: format!("create daemon ready pipe: {e}"),
        child_already_reaped: false,
    })?;
    let ready_fd = ready_writer.as_raw_fd();
    let mut args = request.args;
    replace_ready_fd_arg(&mut args, ready_fd).map_err(|e| EnsureProcFailure {
        error: format!(
            "daemon for terminal {} did not become ready ({e})",
            request.proc_id
        ),
        child_already_reaped: false,
    })?;

    // `EnsureProcRequest.cwd` is INTENTIONALLY NOT APPLIED here.
    //
    // Pre-#388 `spawn_daemon_with_parts` never set the daemon process's
    // cwd: the desired cwd is only passed via the `--cwd` argv flag for
    // the daemon to apply to its PTY child. Applying it as the daemon
    // process's own cwd breaks callers that name a directory the daemon
    // will create (or that doesn't need to exist for the supervisor /
    // daemon themselves) — e.g. `wave_create_sync_daemon`'s
    // `/tmp/issue-250-pr2-test`.
    //
    // The field is retained on the wire so future phases can choose to
    // honor it for the PTY child's chdir separately from the supervisor
    // process cwd; if you find yourself wanting to `cmd
    // .current_dir(&request.cwd)` here, reconsider — you want the
    // `--cwd` argv flag the kernel already builds.
    let _intentionally_unused_at_supervisor = &request.cwd;
    let mut cmd = Command::new(&request.program);
    cmd.args(&args)
        .envs(request.envs)
        .stdin(Stdio::null())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .kill_on_drop(false);
    #[cfg(unix)]
    {
        cmd.process_group(0);
    }
    unsafe {
        cmd.pre_exec(move || {
            let flags = libc::fcntl(ready_fd, libc::F_GETFD);
            if flags == -1 {
                return Err(io::Error::last_os_error());
            }
            if libc::fcntl(ready_fd, libc::F_SETFD, flags & !libc::FD_CLOEXEC) == -1 {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        });
    }

    let child = cmd.spawn().map_err(|e| EnsureProcFailure {
        error: format!("spawn pty bootstrap process: {e}"),
        child_already_reaped: false,
    })?;
    drop(ready_writer);
    let pid = child.id().unwrap_or_default();
    let child = Arc::new(Mutex::new(child));
    let (broadcast_tx, _) = broadcast::channel(2048);
    {
        let mut entries = registry.inner.lock().map_err(|_| EnsureProcFailure {
            error: "proc registry mutex poisoned".into(),
            child_already_reaped: false,
        })?;
        entries.insert(
            request.proc_id.clone(),
            Arc::new(ProcEntry {
                pid,
                io_mode: IoMode::Pipe,
                runtime: ProcRuntime::Pipe {
                    child: child.clone(),
                },
                byte_ring: StdMutex::new(ByteRing::new(request.replay_bytes)),
                cursor_tail: AtomicU64::new(0),
                cursor_head: AtomicU64::new(0),
                exit: StdMutex::new(None),
                broadcast_tx,
            }),
        );
    }
    Ok(Spawned {
        proc_id: request.proc_id,
        pid,
        pipe_child: Some(child),
        ready_reader: Some(ready_reader),
        ready_timeout: Duration::from_millis(request.ready_timeout_ms),
        registry,
    })
}

async fn try_spawn_pty(
    registry: ProcRegistry,
    request: EnsureProcRequest,
    cols: u16,
    rows: u16,
) -> Result<Spawned, EnsureProcFailure> {
    let pty_system = native_pty_system();
    let pair = pty_system
        .openpty(PtPtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        })
        .map_err(|e| EnsureProcFailure {
            error: format!("allocate pty for {}: {e}", request.proc_id),
            child_already_reaped: false,
        })?;
    let mut cmd = CommandBuilder::new(&request.program);
    for arg in &request.args {
        cmd.arg(arg);
    }
    if !request.cwd.is_empty() {
        cmd.cwd(&request.cwd);
    }
    for (key, value) in &request.envs {
        cmd.env(key, value);
    }
    let reader = pair
        .master
        .try_clone_reader()
        .map_err(|e| EnsureProcFailure {
            error: format!("clone pty reader for {}: {e}", request.proc_id),
            child_already_reaped: false,
        })?;
    let writer = pair.master.take_writer().map_err(|e| EnsureProcFailure {
        error: format!("take pty writer for {}: {e}", request.proc_id),
        child_already_reaped: false,
    })?;
    let child = pair
        .slave
        .spawn_command(cmd)
        .map_err(|e| EnsureProcFailure {
            error: format!("spawn pty child for {}: {e}", request.proc_id),
            child_already_reaped: false,
        })?;
    drop(pair.slave);

    let pid = child.process_id().unwrap_or_default();
    let master = Arc::new(StdMutex::new(pair.master));
    let writer = Arc::new(StdMutex::new(writer));
    let (broadcast_tx, _) = broadcast::channel(2048);
    let replay_bytes = if request.replay_bytes == 0 {
        1024 * 1024
    } else {
        request.replay_bytes
    };
    let entry = Arc::new(ProcEntry {
        pid,
        io_mode: IoMode::Pty { cols, rows },
        runtime: ProcRuntime::Pty {
            master: master.clone(),
            writer,
        },
        byte_ring: StdMutex::new(ByteRing::new(replay_bytes)),
        cursor_tail: AtomicU64::new(0),
        cursor_head: AtomicU64::new(0),
        exit: StdMutex::new(None),
        broadcast_tx: broadcast_tx.clone(),
    });
    registry
        .inner
        .lock()
        .map_err(|_| EnsureProcFailure {
            error: "proc registry mutex poisoned".into(),
            child_already_reaped: false,
        })?
        .insert(request.proc_id.clone(), entry.clone());
    spawn_pty_reader_task(request.proc_id.clone(), entry.clone(), reader);
    spawn_pty_waiter(request.proc_id.clone(), entry, child);

    Ok(Spawned {
        proc_id: request.proc_id,
        pid,
        pipe_child: None,
        ready_reader: None,
        ready_timeout: Duration::from_millis(request.ready_timeout_ms),
        registry,
    })
}

fn spawn_pty_reader_task(
    proc_id: String,
    entry: Arc<ProcEntry>,
    mut reader: Box<dyn io::Read + Send>,
) {
    std::thread::spawn(move || {
        let mut buf = [0_u8; 8192];
        loop {
            match reader.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    let bytes = buf[..n].to_vec();
                    let start = {
                        let mut ring = match entry.byte_ring.lock() {
                            Ok(ring) => ring,
                            Err(_) => break,
                        };
                        let (start, tail) = ring.append(bytes.clone());
                        let (head, _) = ring.window();
                        entry.cursor_head.store(head, Ordering::SeqCst);
                        entry.cursor_tail.store(tail, Ordering::SeqCst);
                        start
                    };
                    let _ = entry.broadcast_tx.send(DataFrame::Output {
                        cursor: start,
                        bytes,
                    });
                }
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(e) => {
                    tracing::warn!(proc_id = %proc_id, error = %e, "pty read error; stopping reader");
                    break;
                }
            }
        }
    });
}

fn spawn_pty_waiter(
    proc_id: String,
    entry: Arc<ProcEntry>,
    mut child: Box<dyn portable_pty::Child + Send + Sync>,
) {
    // OS thread, NOT `tokio::task::spawn_blocking`: a long-lived PTY child
    // (shell / codex / claude) keeps `child.wait()` blocked for the
    // session's entire lifetime. `BlockingPool::shutdown` (called from
    // `Runtime::drop`) waits unconditionally for every spawn_blocking
    // future to complete, so a `#[tokio::test]` fn that drops its runtime
    // while a PTY child is still alive would hang forever on the
    // blocking pool. A plain `std::thread::spawn` is not tracked by the
    // blocking pool; same reasoning as `spawn_pty_reader_task` above. The
    // body is sync-only (Mutex / atomic / broadcast::Sender::send /
    // tracing) — no `.await`, no tokio context required.
    std::thread::spawn(move || {
        let status = child.wait().ok();
        let exit = match status.as_ref() {
            Some(status) if status.signal().is_some() => ProcExit {
                status: None,
                signalled: true,
                cursor: entry.cursor_tail.load(Ordering::SeqCst),
            },
            Some(status) => ProcExit {
                status: Some(status.exit_code() as i32),
                signalled: false,
                cursor: entry.cursor_tail.load(Ordering::SeqCst),
            },
            None => ProcExit {
                status: None,
                signalled: false,
                cursor: entry.cursor_tail.load(Ordering::SeqCst),
            },
        };
        if let Ok(mut slot) = entry.exit.lock() {
            *slot = Some(exit.clone());
        }
        tracing::info!(
            proc_id = %proc_id,
            status = ?exit.status,
            signalled = exit.signalled,
            "pty child exited"
        );
        let _ = entry.broadcast_tx.send(DataFrame::Exited(exit));
    });
}

async fn await_ready_phase(spawned: Spawned) -> Result<u32, EnsureProcFailure> {
    let Spawned {
        proc_id,
        pid,
        pipe_child,
        ready_reader,
        ready_timeout,
        registry,
    } = spawned;
    let Some(child) = pipe_child else {
        return Ok(pid);
    };
    let Some(ready_reader) = ready_reader else {
        return Ok(pid);
    };
    let readiness = await_readiness(&proc_id, child.clone(), ready_reader, ready_timeout).await;
    if let Err(err) = readiness {
        registry
            .inner
            .lock()
            .map(|mut entries| entries.remove(&proc_id))
            .ok();
        if !err.child_already_reaped {
            tokio::spawn(async move {
                let _ = child.lock().await.wait().await;
            });
        }
        return Err(err);
    }

    if registry.reap_children {
        let registry_for_wait = registry.clone();
        let proc_id_for_wait = proc_id;
        tokio::spawn(async move {
            let _ = tokio::task::spawn_blocking(move || waitpid(pid)).await;
            registry_for_wait
                .inner
                .lock()
                .map(|mut entries| entries.remove(&proc_id_for_wait))
                .ok();
        });
    }
    Ok(pid)
}

async fn existing_live_pid(registry: &ProcRegistry, proc_id: &str) -> Option<u32> {
    let entry = {
        let entries = registry.inner.lock().ok()?;
        entries.get(proc_id).cloned()
    }?;
    match &entry.runtime {
        ProcRuntime::Pipe { child } => match child.lock().await.try_wait() {
            Ok(None) => Some(entry.pid),
            Ok(Some(_)) | Err(_) => {
                registry
                    .inner
                    .lock()
                    .map(|mut entries| entries.remove(proc_id))
                    .ok();
                None
            }
        },
        ProcRuntime::Pty { .. } => {
            if entry
                .exit
                .lock()
                .map(|exit| exit.is_none())
                .unwrap_or(false)
            {
                Some(entry.pid)
            } else {
                None
            }
        }
    }
}

async fn await_readiness(
    proc_id: &str,
    child: Arc<Mutex<Child>>,
    ready_reader: AsyncFd<OwnedFd>,
    timeout: Duration,
) -> Result<(), EnsureProcFailure> {
    let ready_scanner = StdMutex::new(ReadySignalScanner::new());
    tokio::select! {
        ready_res = read_ready_signal(&ready_reader, &ready_scanner) => {
            ready_res.map_err(|e| EnsureProcFailure {
                error: daemon_not_ready(proc_id, e),
                child_already_reaped: false,
            })
        }
        wait_res = async {
            child.lock().await.wait().await
        } => {
            match drain_ready_signal_now(&ready_reader, &ready_scanner) {
                Ok(true) => Ok(()),
                Ok(false) => match wait_res {
                    Ok(status) => Err(EnsureProcFailure {
                        error: daemon_not_ready(proc_id, format_args!("exited before ready: {status}")),
                        child_already_reaped: true,
                    }),
                    Err(e) => Err(EnsureProcFailure {
                        error: daemon_not_ready(proc_id, format_args!("failed to observe child exit: {e}")),
                        child_already_reaped: true,
                    }),
                },
                Err(e) => Err(EnsureProcFailure {
                    error: daemon_not_ready(proc_id, format_args!("read ready fd after child exit: {e}")),
                    child_already_reaped: true,
                }),
            }
        }
        _ = tokio::time::sleep(timeout) => {
            Err(EnsureProcFailure {
                error: daemon_not_ready(proc_id, format_args!("ready-fd backstop after {timeout:?}")),
                child_already_reaped: false,
            })
        }
    }
}

fn daemon_not_ready(proc_id: &str, reason: impl std::fmt::Display) -> String {
    format!("daemon for terminal {proc_id} did not become ready ({reason})")
}

fn sock_arg(args: &[String]) -> Option<PathBuf> {
    args.windows(2)
        .find(|pair| pair[0] == "--sock")
        .map(|pair| PathBuf::from(&pair[1]))
}

fn replace_ready_fd_arg(args: &mut [String], ready_fd: i32) -> io::Result<()> {
    let Some(index) = args.iter().position(|arg| arg == "--ready-fd") else {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "daemon argv missing --ready-fd",
        ));
    };
    let Some(value) = args.get_mut(index + 1) else {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "daemon argv missing --ready-fd value",
        ));
    };
    *value = ready_fd.to_string();
    Ok(())
}

fn exit_sidecar_path(sock: &Path) -> PathBuf {
    let mut s = sock.as_os_str().to_owned();
    s.push(".exit");
    PathBuf::from(s)
}

fn set_fd_nonblocking(fd: i32) -> io::Result<()> {
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if flags == -1 {
        return Err(io::Error::last_os_error());
    }
    if unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } == -1 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(all(unix, not(target_os = "linux")))]
fn set_fd_cloexec(fd: i32, cloexec: bool) -> io::Result<()> {
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
    if flags == -1 {
        return Err(io::Error::last_os_error());
    }
    let next = if cloexec {
        flags | libc::FD_CLOEXEC
    } else {
        flags & !libc::FD_CLOEXEC
    };
    if unsafe { libc::fcntl(fd, libc::F_SETFD, next) } == -1 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

fn create_cloexec_pipe() -> io::Result<[OwnedFd; 2]> {
    let mut fds = [0; 2];
    #[cfg(target_os = "linux")]
    {
        if unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_CLOEXEC) } == -1 {
            return Err(io::Error::last_os_error());
        }
    }
    #[cfg(all(unix, not(target_os = "linux")))]
    {
        if unsafe { libc::pipe(fds.as_mut_ptr()) } == -1 {
            return Err(io::Error::last_os_error());
        }
    }

    let read_fd = unsafe { OwnedFd::from_raw_fd(fds[0]) };
    let write_fd = unsafe { OwnedFd::from_raw_fd(fds[1]) };
    #[cfg(all(unix, not(target_os = "linux")))]
    {
        set_fd_cloexec(read_fd.as_raw_fd(), true)?;
        set_fd_cloexec(write_fd.as_raw_fd(), true)?;
    }
    Ok([read_fd, write_fd])
}

fn ready_pipe() -> io::Result<(AsyncFd<OwnedFd>, OwnedFd)> {
    let [read_fd, write_fd] = create_cloexec_pipe()?;
    set_fd_nonblocking(read_fd.as_raw_fd())?;
    Ok((AsyncFd::new(read_fd)?, write_fd))
}

struct ReadySignalScanner {
    buf: Vec<u8>,
}

impl ReadySignalScanner {
    fn new() -> Self {
        Self {
            buf: Vec::with_capacity(16),
        }
    }

    fn push(&mut self, bytes: &[u8]) -> io::Result<bool> {
        let scan_from = self
            .buf
            .len()
            .saturating_sub(DAEMON_READY_SIGNAL.len().saturating_sub(1));
        self.buf.extend_from_slice(bytes);
        if self.buf[scan_from..]
            .windows(DAEMON_READY_SIGNAL.len())
            .any(|w| w == DAEMON_READY_SIGNAL)
        {
            return Ok(true);
        }
        if self.buf.len() > DAEMON_READY_MAX_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "ready fd did not contain ready signal",
            ));
        }
        Ok(false)
    }
}

async fn read_ready_signal(
    reader: &AsyncFd<OwnedFd>,
    scanner: &StdMutex<ReadySignalScanner>,
) -> io::Result<()> {
    let mut chunk = [0_u8; 16];
    loop {
        let mut guard = reader.readable().await?;
        let n =
            match guard.try_io(|inner| read_ready_chunk(inner.get_ref().as_raw_fd(), &mut chunk)) {
                Ok(result) => result?,
                Err(_would_block) => continue,
            };
        if n == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "ready fd closed before ready signal",
            ));
        }
        if with_ready_scanner(scanner, |scanner| scanner.push(&chunk[..n]))? {
            return Ok(());
        }
    }
}

fn drain_ready_signal_now(
    reader: &AsyncFd<OwnedFd>,
    scanner: &StdMutex<ReadySignalScanner>,
) -> io::Result<bool> {
    let mut chunk = [0_u8; 16];
    loop {
        match read_ready_chunk(reader.get_ref().as_raw_fd(), &mut chunk) {
            Ok(0) => return Ok(false),
            Ok(n) => {
                if with_ready_scanner(scanner, |scanner| scanner.push(&chunk[..n]))? {
                    return Ok(true);
                }
            }
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => return Ok(false),
            Err(e) => return Err(e),
        }
    }
}

fn with_ready_scanner<T>(
    scanner: &StdMutex<ReadySignalScanner>,
    f: impl FnOnce(&mut ReadySignalScanner) -> io::Result<T>,
) -> io::Result<T> {
    let mut scanner = scanner
        .lock()
        .map_err(|_| io::Error::other("ready scanner mutex poisoned"))?;
    f(&mut scanner)
}

fn read_ready_chunk(fd: i32, chunk: &mut [u8]) -> io::Result<usize> {
    loop {
        let n = unsafe { libc::read(fd, chunk.as_mut_ptr().cast(), chunk.len()) };
        if n >= 0 {
            return Ok(n as usize);
        }
        let err = io::Error::last_os_error();
        if err.kind() != io::ErrorKind::Interrupted {
            return Err(err);
        }
    }
}

fn waitpid(pid: u32) -> io::Result<()> {
    let mut status = 0;
    loop {
        let waited = unsafe { libc::waitpid(pid as libc::pid_t, &mut status, 0) };
        if waited >= 0 {
            return Ok(());
        }
        let err = io::Error::last_os_error();
        if err.kind() != io::ErrorKind::Interrupted {
            return Err(err);
        }
    }
}

pub mod test_support {
    use super::*;
    use tempfile::TempDir;

    pub struct InProcessProcSupervisor {
        sock: PathBuf,
        _temp: TempDir,
        registry: ProcRegistry,
        shutdown: Option<oneshot::Sender<()>>,
        task: tokio::task::JoinHandle<()>,
    }

    impl InProcessProcSupervisor {
        pub async fn start() -> anyhow::Result<Self> {
            let temp = tempfile::tempdir()?;
            let sock = temp.path().join("proc-supervisor.sock");
            let registry = ProcRegistry::without_reaper();
            let serve_registry = registry.clone();
            let (shutdown_tx, shutdown_rx) = oneshot::channel();
            // Bind the listener synchronously here so the socket is
            // reachable the moment start() returns — no listen-race
            // window against the spawned serve task, which has been
            // a flake source under heavy parallel test load.
            let listener = bind_control_listener(&sock)?;
            let serve_sock = sock.clone();
            let task = tokio::spawn(async move {
                let _ =
                    serve_with_listener(listener, serve_sock, serve_registry, shutdown_rx).await;
            });
            Ok(Self {
                sock,
                _temp: temp,
                registry,
                shutdown: Some(shutdown_tx),
                task,
            })
        }

        pub fn sock(&self) -> &Path {
            &self.sock
        }
    }

    impl Drop for InProcessProcSupervisor {
        fn drop(&mut self) {
            self.registry.terminate_all_process_groups_sync();
            if let Some(shutdown) = self.shutdown.take() {
                let _ = shutdown.send(());
            }
            self.task.abort();
        }
    }
}
