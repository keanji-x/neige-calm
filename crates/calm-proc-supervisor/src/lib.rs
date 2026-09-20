mod child_environment;

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
use std::sync::Condvar;
use std::sync::Mutex as StdMutex;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::time::Duration;
use tokio::io::unix::AsyncFd;
use tokio::net::{UnixListener, UnixStream};
use tokio::process::{Child, Command};
use tokio::sync::{Mutex, broadcast, oneshot};

const DAEMON_READY_SIGNAL: &[u8] = b"ready\n";
const DAEMON_READY_MAX_BYTES: usize = 64;

/// How long an exited pty entry stays whole in the registry (sticky exit + last-screen replay for a reconnect); a later attach gets `UnknownProc`.
const PTY_RECLAIM_GRACE: Duration = Duration::from_secs(60);

/// Sweep period, derived from the grace so a millisecond-level test grace still sweeps promptly.
const PTY_SWEEP_MIN: Duration = Duration::from_millis(10);
const PTY_SWEEP_MAX: Duration = Duration::from_secs(1);

#[derive(Clone)]
pub struct ProcRegistry {
    inner: Arc<StdMutex<HashMap<String, Arc<ProcEntry>>>>,
    reap_children: bool,
    pty_reclaim_grace: Duration,
    /// Production always leaves this at `PTY_DRAIN_GRACE`; only tests move it.
    pty_drain_grace: Duration,
    /// Un-reaped pty leader `Child` handles this registry owns. Registry-scoped, never a crate-level static (one test binary's tests share a process);
    /// paired with the `Option<Box<dyn Child>>` (`+1` on install, `-1` on `take()`), not with observed exits.
    pin_count: Arc<AtomicUsize>,
    /// Pty leaders this registry has lost the pin on (the kernel answered `ECHILD`).
    pin_lost_count: Arc<AtomicUsize>,
}

struct ProcEntry {
    /// Never a signal target: route through `pgid_lease::group_target`.
    pid: u32,
    io_mode: IoMode,
    runtime: ProcRuntime,
    byte_ring: StdMutex<ByteRing>,
    cursor_tail: AtomicU64,
    cursor_head: AtomicU64,
    exit: StdMutex<Option<ProcExit>>,
    /// Pty only: set the instant the waiter observes the leader's exit (`WNOWAIT`, not reaped), before the drain grace and the sticky `exit` write.
    /// Liveness probes must consult it, otherwise an exited child looks alive for the whole grace window.
    exit_observed: AtomicBool,
    /// The pty waiter did not run to completion; set by `WaiterCompletion::drop`, which also seals a degraded exit and schedules removal.
    waiter_degraded: AtomicBool,
    /// Earliest instant this entry may be swept; `None` = not exited yet.
    remove_after: StdMutex<Option<std::time::Instant>>,
    broadcast_tx: broadcast::Sender<DataFrame>,
    /// This entry's clone of its registry's pin counter; Pipe entries carry it but never touch it.
    pin_count: Arc<AtomicUsize>,
}

/// The crate's only reap of a pty leader; while any `Arc<ProcEntry>` lives the leader stays a zombie and its pid/pgid stay ours.
/// `Drop::drop` runs before the fields drop, so this `try_wait()` (`WNOHANG`, never blocks) precedes `UnixMasterWriter::drop`'s blocking write.
impl Drop for ProcEntry {
    fn drop(&mut self) {
        // Pipe is an explicit arm: its child is owned by tokio, and gating only on `exit_observed`/`leader.is_some()` becomes a double reap the day something sets those for Pipe.
        let ProcRuntime::Pty { leader, .. } = &self.runtime else {
            return;
        };
        let taken = leader
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take();
        let Some(mut child) = taken else {
            return;
        };
        // The only place the count goes down, on every path below including the two fail-loud ones.
        self.pin_count.fetch_sub(1, Ordering::SeqCst);
        match child.try_wait() {
            Ok(Some(_)) => {}
            Ok(None) => tracing::error!(
                pid = self.pid,
                "entry dropped while its pty leader is still running; the leader is now \
                 unreapable by this process"
            ),
            Err(e) => tracing::error!(pid = self.pid, %e, "reaping the pty leader failed"),
        }
    }
}

impl ProcEntry {
    /// Whether the pty child is still running. `false` as soon as the child is
    /// reaped, without waiting for the drain grace / sticky `exit` write.
    fn pty_running(&self) -> bool {
        !self.exit_observed.load(Ordering::SeqCst)
            && self.exit.lock().map(|exit| exit.is_none()).unwrap_or(false)
    }

    /// Only ever moves earlier: `Cleanup` may pull it to "now" and a later waiter must not push the grace back.
    fn schedule_removal(&self, at: std::time::Instant) {
        let mut slot = self
            .remove_after
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        *slot = Some(match *slot {
            Some(prev) => prev.min(at),
            None => at,
        });
    }

    /// Removable = scheduled and due, sticky exit recorded, and master EOF reached.
    /// The EOF condition is the safety gate: dropping the entry drops `UnixMasterWriter`, whose `Drop` writes `\n`+VEOF to the master — lethal to a grandchild still holding the slave. `PtyDrainGate::is_drained()` is not a substitute (it also fires on read error / panic).
    fn removable(&self, now: std::time::Instant) -> bool {
        let due = self
            .remove_after
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .is_some_and(|at| at <= now);
        if !due {
            return false;
        }
        // Only pty entries reach the sweeper; pipe reclaim happens in `await_ready_phase`, so fail closed here.
        let ProcRuntime::Pty { eof_reached, .. } = &self.runtime else {
            return false;
        };
        let exit_recorded = self
            .exit
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .is_some();
        exit_recorded && eof_reached.load(Ordering::SeqCst)
    }
}

enum ProcRuntime {
    Pipe {
        child: Arc<Mutex<Child>>,
    },
    Pty {
        master: Arc<StdMutex<Box<dyn MasterPty + Send>>>,
        writer: Arc<StdMutex<Box<dyn io::Write + Send>>>,
        /// Master read EOF, i.e. no fd holds the slave any more; set only by the reader on `read() == Ok(0)`.
        eof_reached: Arc<AtomicBool>,
        /// The leader's `Child` handle and its only owner: `Some` = not reaped, `None` = reaped; `Option::take()` in `Drop` makes "reaped at most once" a type property.
        leader: StdMutex<Option<Box<dyn portable_pty::Child + Send + Sync>>>,
        /// `waitid` answered `ECHILD`: the pin is proven broken (something made `SIGCHLD` auto-reaping).
        /// Best-effort detection, not fail-closed: the kernel freed the number at exit, strictly before this flag. Monotonic and per-entry.
        pin_lost: AtomicBool,
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
    /// Set exactly once, by the pty waiter, in the same critical section that publishes `DataFrame::Exited`;
    /// once sealed the reader must neither append nor broadcast, so `Exited` is provably the last frame.
    sealed: bool,
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
            sealed: false,
        }
    }

    /// Closes the ring for further writes and returns its final `cursor_tail`.
    /// Callers must hold the ring mutex and publish `Exited` before releasing
    /// it — that is what makes the seal and the exit frame atomic.
    fn seal(&mut self) -> u64 {
        self.sealed = true;
        self.cursor_tail
    }

    fn is_sealed(&self) -> bool {
        self.sealed
    }

    fn append(&mut self, bytes: Vec<u8>) -> (u64, u64) {
        debug_assert!(!self.sealed, "append after seal (#993)");
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

/// Entry snapshot for test assertions.
#[doc(hidden)]
#[derive(Debug, Clone, Copy)]
pub struct EntryDebugStats {
    pub buffered_bytes: usize,
    /// The sticky `exit` slot has been stamped.
    pub exit_recorded: bool,
    /// The waiter has observed the leader's exit — earlier than `exit_recorded`. The leader is then a retained zombie,
    /// so `kill(leader, 0) == 0` forever: tests needing "the exit has happened" must poll this bit, never pid liberation.
    pub exit_observed: bool,
    /// Pty only: the kernel answered `ECHILD`, so this entry's pin is proven broken and it refuses to be a group signal target.
    pub pin_lost: bool,
    /// The pty waiter did not run to completion.
    pub waiter_degraded: bool,
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
            pty_reclaim_grace: PTY_RECLAIM_GRACE,
            pty_drain_grace: PTY_DRAIN_GRACE,
            pin_count: Arc::new(AtomicUsize::new(0)),
            pin_lost_count: Arc::new(AtomicUsize::new(0)),
        }
    }

    fn without_reaper() -> Self {
        Self {
            reap_children: false,
            ..Self::new()
        }
    }

    /// Test-only: shorten the exit → removal grace.
    #[doc(hidden)]
    pub fn with_pty_reclaim_grace(mut self, grace: Duration) -> Self {
        self.pty_reclaim_grace = grace;
        self
    }

    /// TEST-ONLY: widen the reap → sticky-exit drain window so "inside the drain window" is a state to establish, not a 50ms race.
    #[doc(hidden)]
    pub fn with_pty_drain_grace(mut self, grace: Duration) -> Self {
        self.pty_drain_grace = grace;
        self
    }

    #[doc(hidden)]
    pub fn debug_entry_count(&self) -> usize {
        self.inner.lock().map(|entries| entries.len()).unwrap_or(0)
    }

    /// `None` once the entry has been reclaimed.
    #[doc(hidden)]
    pub fn debug_entry_stats(&self, proc_id: &str) -> Option<EntryDebugStats> {
        let entry = self.inner.lock().ok()?.get(proc_id).cloned()?;
        Some(EntryDebugStats {
            buffered_bytes: entry
                .byte_ring
                .lock()
                .map(|ring| ring.buffered_len())
                .unwrap_or(0),
            exit_recorded: entry
                .exit
                .lock()
                .map(|exit| exit.is_some())
                .unwrap_or(false),
            exit_observed: entry.exit_observed.load(Ordering::SeqCst),
            pin_lost: match &entry.runtime {
                ProcRuntime::Pty { pin_lost, .. } => pin_lost.load(Ordering::SeqCst),
                ProcRuntime::Pipe { .. } => false,
            },
            waiter_degraded: entry.waiter_degraded.load(Ordering::SeqCst),
        })
    }

    /// Reads the counter, not the registry map: an entry displaced by a same-`proc_id` respawn has left the map but still owns its handle.
    #[doc(hidden)]
    pub fn debug_pin_count(&self) -> usize {
        self.pin_count.load(Ordering::SeqCst)
    }

    /// How many pty leaders this registry has lost the pin on (`waitid` answered `ECHILD`).
    #[doc(hidden)]
    pub fn debug_pin_lost_count(&self) -> usize {
        self.pin_lost_count.load(Ordering::SeqCst)
    }

    /// Test-only fault injection: set the pty master `O_NONBLOCK` so the reader's next `read()` fails with `EAGAIN` — the real read-error exit path (drain gate falls, `eof_reached` stays false).
    /// The reader is probably blocked in `read()`, so the caller must then make the master readable (e.g. `WriteStdin`). Returns `false` if the proc is missing, not a pty, or the fd op failed.
    #[doc(hidden)]
    pub fn debug_force_pty_reader_error(&self, proc_id: &str) -> bool {
        let Some(entry) = self
            .inner
            .lock()
            .ok()
            .and_then(|entries| entries.get(proc_id).cloned())
        else {
            return false;
        };
        let ProcRuntime::Pty { master, .. } = &entry.runtime else {
            return false;
        };
        let master = master
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let Some(fd) = master.as_raw_fd() else {
            return false;
        };
        unsafe {
            let flags = libc::fcntl(fd, libc::F_GETFL);
            if flags < 0 {
                return false;
            }
            libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) == 0
        }
    }

    fn sweep_interval(&self) -> Duration {
        (self.pty_reclaim_grace / 4).clamp(PTY_SWEEP_MIN, PTY_SWEEP_MAX)
    }

    /// The only reclaim path: remove every `removable` entry whole; Rust ownership then frees ring, channel, master and writer.
    /// Destruction strictly outside the lock: dropping the last `Arc` runs `UnixMasterWriter::drop`'s blocking `write_all` on the master fd, so `doomed` is dropped after `drop(entries)`.
    fn sweep_expired_entries(&self) -> usize {
        let now = std::time::Instant::now();
        let mut entries = match self.inner.lock() {
            Ok(entries) => entries,
            Err(poisoned) => poisoned.into_inner(),
        };
        let mut doomed: Vec<Arc<ProcEntry>> = Vec::new();
        entries.retain(|proc_id, entry| {
            if entry.removable(now) {
                tracing::debug!(proc_id = %proc_id, "pty entry expired; removing from registry");
                doomed.push(entry.clone());
                false
            } else {
                true
            }
        });
        drop(entries);
        let removed = doomed.len();
        drop(doomed);
        removed
    }

    pub async fn terminate_all_process_groups(&self) {
        self.terminate_all_process_groups_sync();
    }

    /// Group-SIGTERM every registered proc on the way out. Pipe entries are in scope and must stay in scope: there is no PDEATHSIG, and this is the only mechanism that kills a pipe daemon when the supervisor dies.
    /// No `exit.is_none()` filter: under the pin an exited leader's pgid is still ours and skipping it leaks grandchildren; a `pin_lost` entry is excluded because its pgid is proven not ours.
    pub fn terminate_all_process_groups_sync(&self) {
        let entries: Vec<Arc<ProcEntry>> = self
            .inner
            .lock()
            .map(|entries| entries.values().cloned().collect())
            .unwrap_or_default();
        let targets: Vec<pgid_lease::GroupSignalTarget<'_>> = entries
            .iter()
            .filter_map(|entry| pgid_lease::group_target(entry).ok())
            .collect();
        for target in &targets {
            let _ = pgid_lease::kill_group(target, libc::SIGTERM);
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

/// Binds synchronously so the test fixture's start can return with the socket already reachable (no listen-race window).
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
    let mut sweep = tokio::time::interval(registry.sweep_interval());
    sweep.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            _ = &mut shutdown => {
                break;
            }
            _ = sweep.tick() => {
                registry.sweep_expired_entries();
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

/// Single-shot variant of the two-phase connection path (try_spawn + await_ready_phase), for tests.
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
    let res = {
        let master = master
            .lock()
            .map_err(|_| anyhow::anyhow!("pty master mutex poisoned"))?;
        master.resize(PtPtySize {
            cols: request.cols,
            rows: request.rows,
            pixel_width: request.pixel_w,
            pixel_height: request.pixel_h,
        })
    };
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
    let reply = signal_group_reply(&entry, &request.proc_id, sig);
    write_frame(stream, &reply).await?;
    Ok(())
}

/// The only place that may compute a group signal target and the only `libc::kill(-pgid, ..)`; the number is private to this module (E0616 outside).
/// Regulations: take a lease only from a cloned `Arc<ProcEntry>`, never from the registry guard (that puts a `kill` inside the global lock); never return `GroupSignalTarget<'static>`;
/// `group_target` must never return `Err` for Pipe (shutdown uses `filter_map(..ok())`); never destructure and re-wrap a target — that relabels the lifetime and is review-enforced only.
mod pgid_lease {
    use super::{Ordering, ProcEntry, ProcRuntime};
    use std::io;
    use std::marker::PhantomData;

    /// A struct, not an enum field: variant fields inherit the enum's visibility, so only a newtype with a private field can be "kind public, number private".
    /// No `Copy`, `Clone`, `Display`/`Debug`, or accessor — a `Display` that prints the pgid is an accessor spelled in text.
    pub(super) struct Pgid(libc::pid_t);

    impl Pgid {
        /// Deliberately module-private (not `pub(super)`): this is the one
        /// place the number is legible, and `kill_group` is its only caller.
        fn raw(&self) -> libc::pid_t {
            self.0
        }
    }

    /// A signal target computed from — and borrowing — one `&ProcEntry`.
    pub(super) enum GroupSignalTarget<'a> {
        /// The leader's pgid (numerically `entry.pid`), pinned by the leader zombie for as long as any `Arc<ProcEntry>` is alive.
        Leader(Pgid, PhantomData<&'a ProcEntry>),
        /// The pipe daemon's pgid (`try_spawn_pipe` does `process_group(0)`). Never pinned — the child belongs to tokio;
        /// only shutdown may use it, `handle_signal` must refuse it.
        PipeBestEffort(Pgid, PhantomData<&'a ProcEntry>),
    }

    impl GroupSignalTarget<'_> {
        /// The variant name, for diagnostics. Never the number.
        pub(super) fn kind(&self) -> &'static str {
            match self {
                GroupSignalTarget::Leader(..) => "Leader",
                GroupSignalTarget::PipeBestEffort(..) => "PipeBestEffort",
            }
        }

        fn pgid(&self) -> &Pgid {
            match self {
                GroupSignalTarget::Leader(pgid, _) | GroupSignalTarget::PipeBestEffort(pgid, _) => {
                    pgid
                }
            }
        }
    }

    /// Rendered by `super::group_signal_error_reply` — proc_id and target kind, never a decimal pgid.
    pub(super) enum GroupSignalError {
        /// Produced only by `require_addressable_by_signal_rpc`.
        PipeNotSignalable,
        /// Produced only by `group_target`'s Pty branch once `pin_lost` is set; must stay textually distinguishable from `Kill(ESRCH)`.
        PinLost,
        /// `kill_group`'s `libc::kill` returned -1.
        Kill(io::Error),
    }

    /// The only constructor. `Err` is only possible on the Pty branch; the Pipe branch must always be `Ok(PipeBestEffort)`.
    pub(super) fn group_target(
        entry: &ProcEntry,
    ) -> Result<GroupSignalTarget<'_>, GroupSignalError> {
        match &entry.runtime {
            ProcRuntime::Pipe { .. } => Ok(GroupSignalTarget::PipeBestEffort(
                Pgid(entry.pid as libc::pid_t),
                PhantomData,
            )),
            ProcRuntime::Pty { pin_lost, .. } => {
                if pin_lost.load(Ordering::SeqCst) {
                    return Err(GroupSignalError::PinLost);
                }
                Ok(GroupSignalTarget::Leader(
                    Pgid(entry.pid as libc::pid_t),
                    PhantomData,
                ))
            }
        }
    }

    /// The `Signal` RPC's admission gate: pipe procs are group-terminated only through supervisor shutdown.
    pub(super) fn require_addressable_by_signal_rpc(
        target: &GroupSignalTarget<'_>,
    ) -> Result<(), GroupSignalError> {
        match target {
            GroupSignalTarget::PipeBestEffort(..) => Err(GroupSignalError::PipeNotSignalable),
            GroupSignalTarget::Leader(..) => Ok(()),
        }
    }

    /// The crate's only `libc::kill(-pgid, ..)`. Only this module can read
    /// `Pgid`'s field.
    pub(super) fn kill_group(target: &GroupSignalTarget<'_>, sig: libc::c_int) -> io::Result<()> {
        let rc = unsafe { libc::kill(-target.pgid().raw(), sig) };
        if rc == 0 {
            Ok(())
        } else {
            Err(io::Error::last_os_error())
        }
    }
}

/// Compile-time negative sample: this module exists in order to fail to compile (`cargo check --features pgid-escape-probe` must fail with E0616).
/// It cannot be a trybuild case or `compile_fail` doctest — those compile as an external crate where `pgid_lease` is not nameable. This crate can never be built with `--all-features`.
#[cfg(feature = "pgid-escape-probe")]
mod pgid_escape_probe {
    pub(super) fn read_the_number(t: &super::pgid_lease::GroupSignalTarget<'_>) -> libc::pid_t {
        let super::pgid_lease::GroupSignalTarget::Leader(p, _) = t else {
            return 0;
        };
        p.0 // ← must be error[E0616]: field `0` of struct `Pgid` is private
    }
}

/// Exhaustive on purpose: `handle_signal` must never `?` a `GroupSignalError` into `anyhow`, because that writes no frame and the client just sees the connection close.
/// These messages are an asserted interface, not log wording.
fn group_signal_error_reply(
    proc_id: &str,
    kind: Option<&'static str>,
    err: pgid_lease::GroupSignalError,
) -> ControlReply {
    match err {
        pgid_lease::GroupSignalError::PipeNotSignalable => ControlReply::Error {
            kind: ControlErrorKind::WrongState,
            message: format!(
                "pipe runtime is not group-signalable via the Signal RPC: proc {proc_id}"
            ),
        },
        pgid_lease::GroupSignalError::PinLost => ControlReply::Error {
            kind: ControlErrorKind::Internal,
            message: format!(
                // Generic on purpose: `pin_lost` is set on two waiter arms, only one of which is `ECHILD`.
                "pty leader pin lost for proc {proc_id} (kernel reported ECHILD or waitid failed); refusing to use its pgid as a signal target"
            ),
        },
        pgid_lease::GroupSignalError::Kill(e) => ControlReply::Error {
            kind: ControlErrorKind::Internal,
            message: format!(
                "signal proc {proc_id} ({} target): {e}",
                kind.unwrap_or("unresolved")
            ),
        },
    }
}

/// Synchronous so the lease never crosses an `.await`; the lease comes from a cloned `Arc<ProcEntry>`, never a registry guard.
fn signal_group_reply(entry: &ProcEntry, proc_id: &str, sig: libc::c_int) -> ControlReply {
    let target = match pgid_lease::group_target(entry) {
        Ok(target) => target,
        Err(err) => return group_signal_error_reply(proc_id, None, err),
    };
    if let Err(err) = pgid_lease::require_addressable_by_signal_rpc(&target) {
        return group_signal_error_reply(proc_id, Some(target.kind()), err);
    }
    match pgid_lease::kill_group(&target, sig) {
        Ok(()) => ControlReply::SignalOk,
        Err(e) => group_signal_error_reply(
            proc_id,
            Some(target.kind()),
            pgid_lease::GroupSignalError::Kill(e),
        ),
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
    // Pty: `pty_running` is false from the moment the exit is observed, so a cleanup inside the drain grace does not bounce with WrongState.
    let still_running = match &entry.runtime {
        ProcRuntime::Pty { .. } => entry.pty_running(),
        ProcRuntime::Pipe { .. } => entry.exit.lock().map(|exit| exit.is_none()).unwrap_or(true),
    };
    if still_running {
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
    // `Cleanup` pulls the removal instant to "now" and sweeps once; it skips the grace, not the safety gate (`removable` still requires sticky exit + master EOF).
    // So `CleanupOk` means "scheduled", not "removed": no waiting here — a grandchild may hold the slave indefinitely.
    entry.schedule_removal(std::time::Instant::now());
    let removed = registry.sweep_expired_entries();
    if removed == 0 {
        tracing::debug!(
            proc_id = %request.proc_id,
            "cleanup scheduled removal but the safety gate is not satisfied yet; \
             the periodic sweeper will remove the entry once the pty master reaches EOF"
        );
    }
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
            supervisor_version: calm_session::SUPERVISOR_CONTROL_VERSION,
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

    // `EnsureProcRequest.cwd` is INTENTIONALLY NOT APPLIED here: the cwd reaches the PTY child via the `--cwd` argv flag,
    // and applying it as the daemon's own cwd breaks callers that name a directory the daemon will create.
    let _intentionally_unused_at_supervisor = &request.cwd;
    let mut cmd = Command::new(&request.program);
    cmd.args(&args)
        .env_clear()
        .envs(child_environment::allowed_environment())
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

    // Hard-fail rather than `unwrap_or_default()`: a pid of 0 becomes `Pgid(0)` and shutdown would `kill(-0, SIGTERM)` the supervisor's own process group. Unreachable on unix today.
    let pid = match child.id() {
        Some(pid) if pid != 0 => pid,
        observed => {
            // Deliberately no kill here: for `None` tokio already reaped the child, and for `Some(0)` `Child::kill()` bottoms out in `kill(0, SIGKILL)` — the supervisor's own process group.
            // `child_already_reaped` is therefore only true for `None`.
            return Err(EnsureProcFailure {
                error: format!(
                    "pipe child for {} reported no usable pid ({observed:?}); refusing to register an entry whose group signal target would be 0",
                    request.proc_id
                ),
                child_already_reaped: observed.is_none(),
            });
        }
    };
    let child = Arc::new(Mutex::new(child));
    let (broadcast_tx, _) = broadcast::channel(2048);
    // `insert` may displace a Pty entry still in its reclaim grace; as a bare statement the returned `Arc` would drop inside the registry lock,
    // running `UnixMasterWriter::drop`'s blocking write under the lock. Bind it, release the lock, then drop.
    let displaced = {
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
                exit_observed: AtomicBool::new(false),
                waiter_degraded: AtomicBool::new(false),
                remove_after: StdMutex::new(None),
                broadcast_tx,
                pin_count: registry.pin_count.clone(),
            }),
        )
    };
    drop(displaced);
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
    cmd.env_clear();
    for (key, value) in child_environment::allowed_environment() {
        cmd.env(key, value);
    }
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

    // Hard-fail rather than `unwrap_or_default()`: a pid of 0 makes `kill(-0, sig)` signal the supervisor's own process group. Unreachable on unix today.
    // No `child.kill()` before bailing: `ChildKiller` does `kill(stored_pid, ..)`, and for 0 that is again the supervisor's own group.
    let pid = match child.process_id() {
        Some(pid) if pid != 0 => pid,
        observed => {
            return Err(EnsureProcFailure {
                error: format!(
                    "pty child for {} reported no usable pid ({observed:?}); refusing to register an entry whose group signal target would be 0",
                    request.proc_id
                ),
                child_already_reaped: observed.is_none(),
            });
        }
    };
    let master = Arc::new(StdMutex::new(pair.master));
    let writer = Arc::new(StdMutex::new(writer));
    let drain_gate = Arc::new(PtyDrainGate::new());
    let eof_reached = Arc::new(AtomicBool::new(false));
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
            eof_reached: eof_reached.clone(),
            // The handle moves into the entry at spawn; the waiter only ever gets the bare `pid` and can never reap.
            leader: StdMutex::new(Some(child)),
            pin_lost: AtomicBool::new(false),
        },
        byte_ring: StdMutex::new(ByteRing::new(replay_bytes)),
        cursor_tail: AtomicU64::new(0),
        cursor_head: AtomicU64::new(0),
        exit: StdMutex::new(None),
        exit_observed: AtomicBool::new(false),
        waiter_degraded: AtomicBool::new(false),
        remove_after: StdMutex::new(None),
        broadcast_tx: broadcast_tx.clone(),
        pin_count: registry.pin_count.clone(),
    });
    // Paired with installing the handle above; the only decrement is the matching `leader.take()` in `Drop for ProcEntry`.
    registry.pin_count.fetch_add(1, Ordering::SeqCst);
    // A same-`proc_id` respawn inside the reclaim grace makes the displaced `Arc` the last one; drop it outside the registry lock (its writer's `Drop` does a blocking write).
    let displaced = {
        let mut entries = registry.inner.lock().map_err(|_| EnsureProcFailure {
            error: "proc registry mutex poisoned".into(),
            child_already_reaped: false,
        })?;
        entries.insert(request.proc_id.clone(), entry.clone())
    };
    drop(displaced);
    spawn_pty_reader_task(
        request.proc_id.clone(),
        entry.clone(),
        reader,
        drain_gate.clone(),
        eof_reached,
    );
    spawn_pty_waiter(
        request.proc_id.clone(),
        entry,
        pid,
        drain_gate,
        registry.pty_reclaim_grace,
        registry.pty_drain_grace,
        registry.pin_lost_count.clone(),
    );

    Ok(Spawned {
        proc_id: request.proc_id,
        pid,
        pipe_child: None,
        ready_reader: None,
        ready_timeout: Duration::from_millis(request.ready_timeout_ms),
        registry,
    })
}

/// How long the waiter waits for the pty reader to reach EOF after the child exits before sealing the ring and publishing `Exited` anyway.
/// Not what makes `Exited` the last frame (the seal is); it only bites when a grandchild holds the slave. `terminal_renderer::EXIT_PERSIST_GRACE` const-asserts against this constant.
pub const PTY_DRAIN_GRACE: Duration = Duration::from_millis(50);

/// Handshake between the pty reader and waiter threads: how the waiter learns the reader will produce nothing more.
/// It fires on EOF, read error and panic alike, so it does not mean "the slave is closed" — use `eof_reached` for that.
struct PtyDrainGate {
    drained: StdMutex<bool>,
    signal: Condvar,
}

impl PtyDrainGate {
    fn new() -> Self {
        Self {
            drained: StdMutex::new(false),
            signal: Condvar::new(),
        }
    }

    /// Marks the reader as finished (EOF, read error, or panic) and wakes the
    /// waiter.
    fn mark_drained(&self) {
        let mut guard = self
            .drained
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        *guard = true;
        drop(guard);
        self.signal.notify_all();
    }

    /// Returns `true` when the reader really finished (no further `Output` frame can be broadcast), `false` when `grace` expired.
    fn wait_for_drain(&self, grace: Duration) -> bool {
        let guard = self
            .drained
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let (guard, _) = self
            .signal
            .wait_timeout_while(guard, grace, |drained| !*drained)
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        *guard
    }
}

/// Signals the drain gate from `Drop`, so the reader cannot leave the waiter hanging on any exit path, panic included.
struct DrainGuard(Arc<PtyDrainGate>);

impl Drop for DrainGuard {
    fn drop(&mut self) {
        self.0.mark_drained();
    }
}

fn spawn_pty_reader_task(
    proc_id: String,
    entry: Arc<ProcEntry>,
    mut reader: Box<dyn io::Read + Send>,
    gate: Arc<PtyDrainGate>,
    eof_reached: Arc<AtomicBool>,
) {
    std::thread::spawn(move || {
        let _drain_guard = DrainGuard(gate);
        let mut buf = [0_u8; 8192];
        // Bytes read after the seal and thrown away. Also the "already warned"
        // flag: only the first post-seal chunk logs, the rest are counted.
        let mut discarded_after_seal: u64 = 0;
        loop {
            match reader.read(&mut buf) {
                Ok(0) => {
                    // The only path that sets `eof_reached`: `Ok(0)` is master EOF (portable-pty maps the master's EIO to `Ok(0)`).
                    // Read error / panic do not set it — the slave may still be held by a grandchild, and removing the entry would write `\n`+VEOF into its stdin.
                    eof_reached.store(true, Ordering::SeqCst);
                    break;
                }
                Ok(n) => {
                    // Append AND broadcast inside the ring critical section, or the waiter's seal + `Exited` could slip between them.
                    let mut ring = match entry.byte_ring.lock() {
                        Ok(ring) => ring,
                        Err(poisoned) => poisoned.into_inner(),
                    };
                    if ring.is_sealed() {
                        drop(ring);
                        // After the seal nothing may follow `Exited`, so this chunk is dropped — but we must KEEP READING: a surviving grandchild holds the slave,
                        // and an unread master would fill the kernel tty queue and wedge it inside `write()` forever.
                        if discarded_after_seal == 0 {
                            tracing::warn!(
                                proc_id = %proc_id,
                                "pty output arrived after the exit seal; draining and \
                                 discarding until EOF (slave fd still held by a surviving \
                                 grandchild)"
                            );
                        }
                        discarded_after_seal += n as u64;
                        continue;
                    }
                    let bytes = buf[..n].to_vec();
                    let (start, tail) = ring.append(bytes.clone());
                    let (head, _) = ring.window();
                    entry.cursor_head.store(head, Ordering::SeqCst);
                    entry.cursor_tail.store(tail, Ordering::SeqCst);
                    let _ = entry.broadcast_tx.send(DataFrame::Output {
                        cursor: start,
                        bytes,
                    });
                }
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(e) => {
                    // Do not set `eof_reached`: we do not know whether the slave is closed; keeping the entry beats injecting `\n`+VEOF into a live grandchild.
                    tracing::warn!(
                        proc_id = %proc_id,
                        error = %e,
                        "pty read error; stopping reader without marking EOF — the registry \
                         entry is kept because the slave may still be held"
                    );
                    break;
                }
            }
        }
        if discarded_after_seal > 0 {
            tracing::info!(
                proc_id = %proc_id,
                discarded_bytes = discarded_after_seal,
                "pty reader finished; discarded post-seal output"
            );
        }
    });
}

/// The degraded `(status, signalled)` pair: the process is over, we could not learn how.
const DEGRADED_EXIT_PARTS: (Option<i32>, bool) = (None, false);

/// What one `waitid(P_PID, pid, WEXITED | WNOWAIT)` told us.
enum PtyExitObservation {
    /// The child terminated and is still waitable — a zombie we own, which is what pins its pid and pgid.
    Observed { si_code: i32, si_status: i32 },
    /// `ECHILD`: the kernel says this pid is not our child — something made `SIGCHLD` auto-reaping, so its number was released the instant it exited.
    PinLost,
    /// Any other error. Not retried and not swallowed.
    Unexpected(io::Error),
}

/// Observes the pty leader's exit without reaping it: `WEXITED | WNOWAIT` leaves the child a zombie, the only mechanism that keeps its pid from being recycled.
/// `ECHILD` must not fall into the `EINTR` retry arm — `waitid` would answer `ECHILD` forever and the terminal would never publish `Exited`.
fn observe_pty_exit(pid: u32) -> PtyExitObservation {
    loop {
        // SAFETY: `info` is a live, zeroed `siginfo_t` we own for the duration
        // of the call; `waitid` only writes into it.
        let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
        let rc = unsafe {
            libc::waitid(
                libc::P_PID,
                pid as libc::id_t,
                &mut info,
                libc::WEXITED | libc::WNOWAIT,
            )
        };
        if rc == 0 {
            // `si_code` is a plain public field on gnu targets; `si_status()`
            // reads a union and is therefore an `unsafe fn`.
            return PtyExitObservation::Observed {
                si_code: info.si_code,
                si_status: unsafe { info.si_status() },
            };
        }
        let err = io::Error::last_os_error();
        match err.raw_os_error() {
            Some(libc::EINTR) => continue,
            Some(libc::ECHILD) => return PtyExitObservation::PinLost,
            _ => return PtyExitObservation::Unexpected(err),
        }
    }
}

/// The only conversion from a `siginfo_t` pair to `(exit status, signalled)`; `spawn_pty_waiter` is its only caller, so tests assert on production wiring.
/// `CLD_DUMPED` has its own arm: a core dump has `WIFSIGNALED = 1`, and a `CLD_KILLED`-only match would persist the crash as "exited with code 6".
fn proc_exit_parts_from_siginfo(si_code: i32, si_status: i32) -> (Option<i32>, bool) {
    use std::os::unix::process::ExitStatusExt as _;

    let raw = match si_code {
        libc::CLD_EXITED => (si_status & 0xff) << 8,
        libc::CLD_KILLED => si_status & 0x7f,
        libc::CLD_DUMPED => (si_status & 0x7f) | 0x80,
        other => {
            // `WEXITED` alone can only report termination; anything else here means our reading of the call is wrong, so fail loud.
            tracing::error!(
                si_code = other,
                si_status,
                "waitid returned an si_code that WEXITED should not produce; publishing a \
                 degraded exit"
            );
            return DEGRADED_EXIT_PARTS;
        }
    };
    let status = portable_pty::ExitStatus::from(std::process::ExitStatus::from_raw(raw));
    match status.signal() {
        Some(_) => (None, true),
        None => (Some(status.exit_code() as i32), false),
    }
}

/// The one place that seals the ring, samples the final cursor, stamps the sticky slot and broadcasts `Exited`; that atomicity is what makes `Exited` provably the last frame.
/// Stamps only if absent: a second call returns the existing exit and broadcasts no second `Exited` frame.
fn seal_and_publish_exit(entry: &ProcEntry, proc_id: &str, parts: (Option<i32>, bool)) -> ProcExit {
    let mut ring = match entry.byte_ring.lock() {
        Ok(ring) => ring,
        Err(poisoned) => poisoned.into_inner(),
    };
    let cursor = ring.seal();
    entry.cursor_tail.store(cursor, Ordering::SeqCst);
    let (status, signalled) = parts;
    let exit = ProcExit {
        status,
        signalled,
        cursor,
    };
    // The sticky slot must hold exactly the broadcast value and be visible no later than the frame (`handle_attach` decides with `exit.cursor <= snapshot_tail`); lock order byte_ring → exit matches `handle_attach`.
    // A poisoned lock must still be written: `removable` requires the sticky exit, so skipping it makes the entry unreclaimable forever.
    let already = {
        let mut slot = entry
            .exit
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        match slot.clone() {
            Some(existing) => Some(existing),
            None => {
                *slot = Some(exit.clone());
                None
            }
        }
    };
    if let Some(existing) = already {
        tracing::debug!(
            proc_id = %proc_id,
            "sticky exit already present; not re-stamping and not broadcasting a second \
             Exited frame"
        );
        return existing;
    }
    let _ = entry.broadcast_tx.send(DataFrame::Exited(exit.clone()));
    exit
}

/// RAII guard: a waiter that panics or returns early after observing the exit would leave an entry `removable` never accepts, pinning the leader zombie until the supervisor exits.
/// `Drop` seals a degraded exit through `seal_and_publish_exit` and schedules removal. Prerequisite: `panic = "abort"` would skip `Drop`; nothing pins the unwind profile.
struct WaiterCompletion {
    entry: Arc<ProcEntry>,
    proc_id: String,
    armed: bool,
}

impl WaiterCompletion {
    fn new(entry: Arc<ProcEntry>, proc_id: String) -> Self {
        Self {
            entry,
            proc_id,
            armed: true,
        }
    }

    fn disarm(mut self) {
        self.armed = false;
    }
}

impl Drop for WaiterCompletion {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        tracing::error!(
            proc_id = %self.proc_id,
            "pty waiter did not run to completion (panic or early return); marking the \
             entry degraded and scheduling reclaim so its pinned leader can be reaped"
        );
        self.entry.waiter_degraded.store(true, Ordering::SeqCst);
        seal_and_publish_exit(&self.entry, &self.proc_id, DEGRADED_EXIT_PARTS);
        self.entry.schedule_removal(std::time::Instant::now());
    }
}

/// The only way to declare a pin lost: the per-entry flag and the registry counter are written here together, never apart.
fn mark_pin_lost(entry: &ProcEntry, pin_lost_count: &AtomicUsize) {
    if let ProcRuntime::Pty { pin_lost, .. } = &entry.runtime {
        pin_lost.store(true, Ordering::SeqCst);
    }
    pin_lost_count.fetch_add(1, Ordering::SeqCst);
}

/// The grace is not load-bearing for ordering; it only changes how much in-flight output the degraded path waits for.
fn spawn_pty_waiter(
    proc_id: String,
    entry: Arc<ProcEntry>,
    pid: u32,
    gate: Arc<PtyDrainGate>,
    reclaim_grace: Duration,
    drain_grace: Duration,
    pin_lost_count: Arc<AtomicUsize>,
) {
    // OS thread, NOT `tokio::task::spawn_blocking`: `Runtime::drop` waits for every spawn_blocking future, so a test dropping its runtime
    // while a PTY child is alive would hang forever. The body is sync-only.
    std::thread::spawn(move || {
        // Constructed before the observation so a panic anywhere in this body is covered; every exit path must reach `disarm()` or leave the entry reclaimable.
        let completion = WaiterCompletion::new(entry.clone(), proc_id.clone());
        let parts = match observe_pty_exit(pid) {
            PtyExitObservation::Observed { si_code, si_status } => {
                proc_exit_parts_from_siginfo(si_code, si_status)
            }
            PtyExitObservation::PinLost => {
                mark_pin_lost(&entry, &pin_lost_count);
                tracing::error!(
                    proc_id = %proc_id,
                    pid,
                    "waitid reported ECHILD for the pty leader: the kernel auto-reaped it, \
                     i.e. something in this process made the SIGCHLD disposition one under \
                     which our children never become zombies. Its pid was therefore \
                     released the instant it exited and the #1013 pin is gone. This entry \
                     will refuse to use its pgid as a group signal target from now on. \
                     Detection is after the fact: it cannot close the window between the \
                     kernel freeing the number and this line."
                );
                DEGRADED_EXIT_PARTS
            }
            PtyExitObservation::Unexpected(e) => {
                mark_pin_lost(&entry, &pin_lost_count);
                tracing::error!(
                    proc_id = %proc_id,
                    pid,
                    %e,
                    "waitid on the pty leader failed unexpectedly; publishing a degraded exit \
                     and refusing to use this entry's pgid as a group signal target"
                );
                DEGRADED_EXIT_PARTS
            }
        };
        // Published before the grace window so liveness probes stop reporting an exited child as running.
        entry.exit_observed.store(true, Ordering::SeqCst);
        if !gate.wait_for_drain(drain_grace) {
            tracing::warn!(
                proc_id = %proc_id,
                grace_ms = drain_grace.as_millis() as u64,
                "pty master still open after child exit (slave fd likely held by a \
                 surviving grandchild); sealing the ring and publishing Exited without \
                 a full drain — any further pty output is dropped"
            );
        }
        let exit = seal_and_publish_exit(&entry, &proc_id, parts);
        tracing::info!(
            proc_id = %proc_id,
            status = ?exit.status,
            signalled = exit.signalled,
            "pty child exited"
        );

        // Register one due instant and let the thread end; the periodic sweeper does the removal. Strictly after the seal, or `Exited` is no longer the last frame.
        // `checked_add`: `Instant + Duration` panics on overflow, and this is the waiter's last step — a panic here would leave the entry unreclaimable.
        let now = std::time::Instant::now();
        match now.checked_add(reclaim_grace) {
            Some(at) => entry.schedule_removal(at),
            None => tracing::warn!(
                proc_id = %proc_id,
                grace_secs = reclaim_grace.as_secs(),
                "pty reclaim grace overflows Instant; entry will not be scheduled for removal"
            ),
        }
        // Sticky exit stamped and removal scheduled: only now disarm.
        completion.disarm();
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
            // Unlike Pipe, deliberately not removed in place: that would lose the sticky exit and last-screen replay inside the grace; removal is the sweeper's job.
            if entry.pty_running() {
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
            Self::start_with_registry(ProcRegistry::without_reaper()).await
        }

        /// Test-only: widen the reap → sticky-exit drain window so assertions can stand inside it deterministically.
        pub async fn start_with_drain_grace(pty_drain_grace: Duration) -> anyhow::Result<Self> {
            Self::start_with_registry(
                ProcRegistry::without_reaper().with_pty_drain_grace(pty_drain_grace),
            )
            .await
        }

        pub async fn start_with_grace(pty_reclaim_grace: Duration) -> anyhow::Result<Self> {
            Self::start_with_registry(
                ProcRegistry::without_reaper().with_pty_reclaim_grace(pty_reclaim_grace),
            )
            .await
        }

        async fn start_with_registry(registry: ProcRegistry) -> anyhow::Result<Self> {
            // Control socket dir on a short base: a long `$TMPDIR` eats half of `sun_path`.
            let temp = calm_test_sockets::try_socket_dir("ps")?;
            let sock = temp.path().join("proc-supervisor.sock");
            calm_test_sockets::assert_fits(&sock);
            let serve_registry = registry.clone();
            let (shutdown_tx, shutdown_rx) = oneshot::channel();
            // Bind synchronously so the socket is reachable the moment start() returns (no listen race against the serve task).
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

        pub fn registry(&self) -> &ProcRegistry {
            &self.registry
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

#[cfg(test)]
mod wnowait_tests {
    use super::*;

    #[test]
    fn proc_exit_parts_from_siginfo_maps_every_wexited_code() {
        // The triples are measured (`exit 42`, `SIGKILL`, `abort` under `ulimit -c unlimited` on Linux 6.1 / glibc), not assumed.
        assert_eq!(
            proc_exit_parts_from_siginfo(libc::CLD_EXITED, 42),
            (Some(42), false),
            "a normal exit carries its code and is not signalled"
        );
        assert_eq!(
            proc_exit_parts_from_siginfo(libc::CLD_KILLED, libc::SIGKILL),
            (None, true),
            "a killed child is signalled and has no exit code"
        );
        // A core dump has WIFSIGNALED = 1, so it must land in the signalled column.
        assert_eq!(
            proc_exit_parts_from_siginfo(libc::CLD_DUMPED, libc::SIGABRT),
            (None, true),
            "a core-dumping child is signalled, not an exit with code 6"
        );
        for code in [libc::CLD_STOPPED, libc::CLD_CONTINUED, libc::CLD_TRAPPED] {
            assert_eq!(
                proc_exit_parts_from_siginfo(code, 19),
                DEGRADED_EXIT_PARTS,
                "si_code {code} is not reachable under WEXITED and must degrade"
            );
        }
    }

    /// The positive assertion discriminates the variant and never reads the number (E0616 outside `pgid_lease`).
    #[tokio::test]
    async fn group_target_refuses_the_leader_target_after_pin_loss() {
        let pair = native_pty_system()
            .openpty(PtPtySize {
                rows: 24,
                cols: 80,
                pixel_width: 0,
                pixel_height: 0,
            })
            .expect("openpty");
        let mut cmd = CommandBuilder::new("/bin/sh");
        cmd.arg("-c");
        cmd.arg("exit 0");
        let mut child = pair.slave.spawn_command(cmd).expect("spawn");
        let pid = child.process_id().expect("pid");
        child.wait().expect("wait");
        drop(pair.slave);

        let (broadcast_tx, _) = broadcast::channel(1);
        let entry = ProcEntry {
            pid,
            io_mode: IoMode::Pty { cols: 80, rows: 24 },
            runtime: ProcRuntime::Pty {
                master: Arc::new(StdMutex::new(pair.master)),
                writer: Arc::new(StdMutex::new(Box::new(std::io::sink()))),
                eof_reached: Arc::new(AtomicBool::new(false)),
                // Already reaped above: a handle here would make `Drop for ProcEntry` reap a pid that is already gone.
                leader: StdMutex::new(None),
                pin_lost: AtomicBool::new(false),
            },
            byte_ring: StdMutex::new(ByteRing::new(64)),
            cursor_tail: AtomicU64::new(0),
            cursor_head: AtomicU64::new(0),
            exit: StdMutex::new(None),
            exit_observed: AtomicBool::new(true),
            waiter_degraded: AtomicBool::new(false),
            remove_after: StdMutex::new(None),
            broadcast_tx,
            pin_count: Arc::new(AtomicUsize::new(0)),
        };

        let Ok(target) = pgid_lease::group_target(&entry) else {
            panic!("an intact pin must yield a target")
        };
        assert!(
            matches!(target, pgid_lease::GroupSignalTarget::Leader(..)),
            "an exited pty leader yields the pinned Leader target, kind = {}",
            target.kind()
        );
        // The lease borrows `entry`, so it has to go out of scope before we
        // mutate through `&entry.runtime` below.
        let _ = target;

        let ProcRuntime::Pty { pin_lost: flag, .. } = &entry.runtime else {
            unreachable!("constructed as Pty")
        };
        flag.store(true, Ordering::SeqCst);

        let Err(err) = pgid_lease::group_target(&entry) else {
            panic!("a lost pin must refuse to produce a target")
        };
        match group_signal_error_reply("p", None, err) {
            ControlReply::Error { kind, message } => {
                assert_eq!(kind, ControlErrorKind::Internal);
                assert!(
                    message.starts_with("pty leader pin lost for proc p"),
                    "must be distinguishable from Kill(ESRCH), got: {message}"
                );
            }
            other => panic!("expected ControlReply::Error, got {other:?}"),
        }
    }

    /// No child is spawned: `leader: None` means `Drop` reaps nothing, so these cases depend on no process or `SIGCHLD` disposition.
    fn leaderless_pty_entry(
        exit_observed: bool,
    ) -> (ProcEntry, broadcast::Receiver<DataFrame>, Arc<AtomicUsize>) {
        let pair = native_pty_system()
            .openpty(PtPtySize {
                rows: 24,
                cols: 80,
                pixel_width: 0,
                pixel_height: 0,
            })
            .expect("openpty");
        let (broadcast_tx, rx) = broadcast::channel(8);
        let pin_count = Arc::new(AtomicUsize::new(0));
        let entry = ProcEntry {
            pid: 0,
            io_mode: IoMode::Pty { cols: 80, rows: 24 },
            runtime: ProcRuntime::Pty {
                master: Arc::new(StdMutex::new(pair.master)),
                writer: Arc::new(StdMutex::new(Box::new(std::io::sink()))),
                eof_reached: Arc::new(AtomicBool::new(false)),
                leader: StdMutex::new(None),
                pin_lost: AtomicBool::new(false),
            },
            byte_ring: StdMutex::new(ByteRing::new(64)),
            cursor_tail: AtomicU64::new(0),
            cursor_head: AtomicU64::new(0),
            exit: StdMutex::new(None),
            exit_observed: AtomicBool::new(exit_observed),
            waiter_degraded: AtomicBool::new(false),
            remove_after: StdMutex::new(None),
            broadcast_tx,
            pin_count: pin_count.clone(),
        };
        (entry, rx, pin_count)
    }

    /// `seal_and_publish_exit` has two callers (the waiter's normal path and `WaiterCompletion::drop`), so stamp-only-if-absent is why `Exited` is still provably-once.
    #[test]
    fn seal_and_publish_exit_stamps_and_broadcasts_exactly_once() {
        let (entry, mut rx, _pin_count) = leaderless_pty_entry(true);

        let first = seal_and_publish_exit(&entry, "p", (Some(7), false));
        // The re-entry `WaiterCompletion::drop` performs after a normal path
        // has already sealed: same entry, degraded parts.
        let second = seal_and_publish_exit(&entry, "p", DEGRADED_EXIT_PARTS);

        assert_eq!(
            (second.status, second.signalled),
            (first.status, first.signalled),
            "the re-entry from WaiterCompletion::drop must return the recorded exit, not \
             overwrite it"
        );
        assert_eq!(
            entry
                .exit
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .as_ref()
                .map(|exit| (exit.status, exit.signalled)),
            Some((Some(7), false)),
            "the sticky slot must still hold the first exit"
        );
        assert!(
            matches!(rx.try_recv(), Ok(DataFrame::Exited(e)) if e.status == Some(7)),
            "the first call must broadcast Exited"
        );
        assert!(
            rx.try_recv().is_err(),
            "a second Exited frame was broadcast; no reader has a contract for a duplicate \
             terminal frame"
        );
    }

    #[test]
    fn waiter_completion_guard_makes_a_panicked_waiter_reclaimable() {
        let (entry, mut rx, _pin_count) = leaderless_pty_entry(false);
        let entry = Arc::new(entry);

        let joined = {
            let entry = entry.clone();
            std::thread::spawn(move || {
                let _completion = WaiterCompletion::new(entry, "p".into());
                // Stands in for a panic anywhere between the `waitid`
                // observation and `disarm()`.
                panic!("waiter panicked before disarm");
            })
            .join()
        };

        assert!(joined.is_err(), "the fixture must actually panic");
        assert!(
            entry.waiter_degraded.load(Ordering::SeqCst),
            "must mark the entry degraded"
        );
        assert!(
            entry
                .exit
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .is_some(),
            "must stamp a sticky exit, or `removable` can never accept the entry and the \
             pinned leader is never reaped"
        );
        assert!(
            entry
                .remove_after
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .is_some(),
            "must schedule removal"
        );
        assert!(
            matches!(rx.try_recv(), Ok(DataFrame::Exited(_))),
            "must publish Exited, or attached clients hang instead of degrading"
        );
    }
}

#[cfg(test)]
mod pgid_lease_tests {
    use super::*;

    /// Primary gate for `pgid_lease` regulation 4 (Pipe is never `Err`): `server_restart_survives` hangs rather than fails on that violation.
    /// Also pins that the parent module can discriminate the target's kind via `matches!` without reading its number.
    #[tokio::test]
    async fn pipe_target_is_ok_kind_readable_and_refused_by_the_signal_rpc() {
        let child = Command::new("/bin/sleep")
            .arg("30")
            .kill_on_drop(true)
            .spawn()
            .expect("spawn pipe child");
        let pid = child.id().expect("pipe child pid");
        let (broadcast_tx, _) = broadcast::channel(1);
        let entry = ProcEntry {
            pid,
            io_mode: IoMode::Pipe,
            runtime: ProcRuntime::Pipe {
                child: Arc::new(Mutex::new(child)),
            },
            byte_ring: StdMutex::new(ByteRing::new(64)),
            cursor_tail: AtomicU64::new(0),
            cursor_head: AtomicU64::new(0),
            exit: StdMutex::new(None),
            exit_observed: AtomicBool::new(false),
            waiter_degraded: AtomicBool::new(false),
            remove_after: StdMutex::new(None),
            broadcast_tx,
            pin_count: Arc::new(AtomicUsize::new(0)),
        };

        let target = match pgid_lease::group_target(&entry) {
            Ok(target) => target,
            Err(_) => panic!(
                "group_target must never return Err for a Pipe entry: \
                 terminate_all_process_groups_sync filters on .ok() and would \
                 drop Pipe from the #388 shutdown path"
            ),
        };
        assert!(
            matches!(target, pgid_lease::GroupSignalTarget::PipeBestEffort(..)),
            "pipe entries map to PipeBestEffort"
        );
        assert!(
            !matches!(target, pgid_lease::GroupSignalTarget::Leader(..)),
            "the parent module must still be able to discriminate Leader"
        );
        assert_eq!(target.kind(), "PipeBestEffort");

        let err = pgid_lease::require_addressable_by_signal_rpc(&target)
            .expect_err("the Signal RPC must refuse a Pipe target");
        match group_signal_error_reply("proc-a", Some(target.kind()), err) {
            ControlReply::Error { kind, message } => {
                assert_eq!(kind, ControlErrorKind::WrongState);
                assert!(
                    message.starts_with("pipe runtime is not group-signalable via the Signal RPC"),
                    "stable message prefix, got: {message}"
                );
            }
            other => panic!("expected ControlReply::Error, got {other:?}"),
        }
    }

    /// `PinLost` vs `Kill` must be distinguishable by kind and prefix.
    #[test]
    fn group_signal_error_replies_are_distinguishable_by_kind_and_prefix() {
        let cases = [
            (
                pgid_lease::GroupSignalError::PipeNotSignalable,
                ControlErrorKind::WrongState,
                "pipe runtime is not group-signalable via the Signal RPC: proc p",
            ),
            (
                pgid_lease::GroupSignalError::PinLost,
                ControlErrorKind::Internal,
                "pty leader pin lost for proc p (kernel reported ECHILD or waitid failed);",
            ),
            (
                pgid_lease::GroupSignalError::Kill(io::Error::from_raw_os_error(libc::ESRCH)),
                ControlErrorKind::Internal,
                "signal proc p (Leader target):",
            ),
        ];
        for (err, want_kind, want_prefix) in cases {
            match group_signal_error_reply("p", Some("Leader"), err) {
                ControlReply::Error { kind, message } => {
                    assert_eq!(kind, want_kind, "kind for {want_prefix:?}");
                    assert!(
                        message.starts_with(want_prefix),
                        "want prefix {want_prefix:?}, got {message:?}"
                    );
                    // The asserted `Kill` prefix ends at `(Leader target):`, so a decimal pgid cannot reappear before the io::Error.
                }
                other => panic!("expected ControlReply::Error, got {other:?}"),
            }
        }
    }
}
