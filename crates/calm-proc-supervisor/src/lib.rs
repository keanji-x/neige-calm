use calm_session::control::{ControlMsg, ControlReply, EnsureProcRequest};
use calm_session::{read_frame, write_frame};
use std::collections::HashMap;
use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::sync::Mutex as StdMutex;
use std::time::Duration;
use tokio::io::unix::AsyncFd;
use tokio::net::{UnixListener, UnixStream};
use tokio::process::{Child, Command};
use tokio::sync::{Mutex, oneshot};

const DAEMON_READY_SIGNAL: &[u8] = b"ready\n";
const DAEMON_READY_MAX_BYTES: usize = 64;

#[derive(Clone)]
pub struct ProcRegistry {
    inner: Arc<Mutex<HashMap<String, ProcEntry>>>,
    reap_children: bool,
}

#[derive(Clone)]
struct ProcEntry {
    pid: u32,
    child: Arc<Mutex<Child>>,
}

#[derive(Debug)]
pub struct EnsureProcFailure {
    pub error: String,
    pub child_already_reaped: bool,
}

impl ProcRegistry {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(HashMap::new())),
            reap_children: true,
        }
    }

    #[cfg(any(test, feature = "test-support"))]
    fn without_reaper() -> Self {
        Self {
            inner: Arc::new(Mutex::new(HashMap::new())),
            reap_children: false,
        }
    }

    pub async fn live_pids(&self) -> Vec<u32> {
        self.inner
            .lock()
            .await
            .values()
            .map(|entry| entry.pid)
            .collect()
    }

    pub async fn terminate_all_process_groups(&self) {
        for pid in self.live_pids().await {
            #[cfg(unix)]
            unsafe {
                let _ = libc::kill(-(pid as libc::pid_t), libc::SIGTERM);
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
    let msg: ControlMsg = read_frame(&mut stream).await?;
    match msg {
        ControlMsg::EnsureProc(request) => {
            // Idempotent fast path: a live proc with this id is already
            // past readiness, so emit Spawned+Ready immediately.
            if let Some(pid) = existing_live_pid(&registry, &request.proc_id).await {
                write_frame(&mut stream, &ControlReply::Spawned { pid }).await?;
                write_frame(&mut stream, &ControlReply::Ready).await?;
                return Ok(());
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
                    write_frame(&mut stream, &ControlReply::Spawned { pid: spawned.pid }).await?;
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

struct Spawned {
    proc_id: String,
    pid: u32,
    child: Arc<Mutex<Child>>,
    ready_reader: AsyncFd<OwnedFd>,
    ready_timeout: Duration,
    registry: ProcRegistry,
}

async fn try_spawn(
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
    // /daemon process cwd; if you find yourself wanting to `cmd
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
        error: format!("spawn calm-session-daemon: {e}"),
        child_already_reaped: false,
    })?;
    drop(ready_writer);
    let pid = child.id().unwrap_or_default();
    let child = Arc::new(Mutex::new(child));
    {
        let mut entries = registry.inner.lock().await;
        entries.insert(
            request.proc_id.clone(),
            ProcEntry {
                pid,
                child: child.clone(),
            },
        );
    }
    Ok(Spawned {
        proc_id: request.proc_id,
        pid,
        child,
        ready_reader,
        ready_timeout: Duration::from_millis(request.ready_timeout_ms),
        registry,
    })
}

async fn await_ready_phase(spawned: Spawned) -> Result<u32, EnsureProcFailure> {
    let Spawned {
        proc_id,
        pid,
        child,
        ready_reader,
        ready_timeout,
        registry,
    } = spawned;
    let readiness = await_readiness(&proc_id, child.clone(), ready_reader, ready_timeout).await;
    if let Err(err) = readiness {
        registry.inner.lock().await.remove(&proc_id);
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
                .await
                .remove(&proc_id_for_wait);
        });
    }
    Ok(pid)
}

async fn existing_live_pid(registry: &ProcRegistry, proc_id: &str) -> Option<u32> {
    let entry = {
        let entries = registry.inner.lock().await;
        entries.get(proc_id).cloned()
    }?;
    match entry.child.lock().await.try_wait() {
        Ok(None) => Some(entry.pid),
        Ok(Some(_)) | Err(_) => {
            registry.inner.lock().await.remove(proc_id);
            None
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

#[cfg(any(test, feature = "test-support"))]
pub mod test_support {
    use super::*;
    use tempfile::TempDir;

    pub struct InProcessProcSupervisor {
        sock: PathBuf,
        _temp: TempDir,
        shutdown: Option<oneshot::Sender<()>>,
        task: tokio::task::JoinHandle<()>,
    }

    impl InProcessProcSupervisor {
        pub async fn start() -> anyhow::Result<Self> {
            let temp = tempfile::tempdir()?;
            let sock = temp.path().join("proc-supervisor.sock");
            let registry = ProcRegistry::without_reaper();
            let (shutdown_tx, shutdown_rx) = oneshot::channel();
            // Bind the listener synchronously here so the socket is
            // reachable the moment start() returns — no listen-race
            // window against the spawned serve task, which has been
            // a flake source under heavy parallel test load.
            let listener = bind_control_listener(&sock)?;
            let serve_sock = sock.clone();
            let task = tokio::spawn(async move {
                let _ = serve_with_listener(listener, serve_sock, registry, shutdown_rx).await;
            });
            Ok(Self {
                sock,
                _temp: temp,
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
            if let Some(shutdown) = self.shutdown.take() {
                let _ = shutdown.send(());
            }
            self.task.abort();
        }
    }
}
