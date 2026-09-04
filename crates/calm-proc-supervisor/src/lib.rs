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

/// #996: 一个 pty 进程退出后，整条 entry（`ByteRing`，默认 1 MiB/终端；pty
/// master/writer fd；broadcast 通道）还要在 registry 里保留这么久，给"退出后
/// 立刻重连、拿 sticky exit 与最后一屏 replay"留窗口。窗口内 entry **完整**
/// 保留，不降级、不半死；窗口过后整条移除，资源由 Rust 所有权一次性释放。
///
/// 期满后晚到的 attach 拿到 `UnknownProc`。这不丢信息：终端的退出状态由
/// `calm-server` 在退出当时落库（`terminal_set_exit`），数据库才是权威记录，
/// registry 只是"活着的进程 + 短暂的 replay 缓存"。
const PTY_RECLAIM_GRACE: Duration = Duration::from_secs(60);

/// #996: 清扫周期。由宽限期推导，避免多一个旋钮：默认 60s 宽限 → 1s 一扫，
/// 测试把宽限期调到毫秒级时也能及时清扫。一次扫描只是对 registry 的
/// `HashMap::retain`，锁本来就有。
const PTY_SWEEP_MIN: Duration = Duration::from_millis(10);
const PTY_SWEEP_MAX: Duration = Duration::from_secs(1);

#[derive(Clone)]
pub struct ProcRegistry {
    inner: Arc<StdMutex<HashMap<String, Arc<ProcEntry>>>>,
    reap_children: bool,
    pty_reclaim_grace: Duration,
    /// How long the pty waiter waits for the reader to drain after the child is
    /// reaped. Production always leaves this at `PTY_DRAIN_GRACE`; only tests
    /// move it (see `with_pty_drain_grace`), so that "act inside the drain
    /// window" is a state to be established rather than a 50ms race to win.
    pty_drain_grace: Duration,
    /// #1013 (PR-B): how many un-reaped pty leader `Child` handles this
    /// registry currently owns — i.e. how many pids it is pinning.
    ///
    /// **It is registry-scoped on purpose, and must not become a crate-level
    /// `static`** (design C2). `cargo test` runs the test functions of one
    /// integration binary as threads of a *single process*, and
    /// `pty_entry_reclaim.rs` alone has five pty-spawning tests, two of which
    /// deliberately hold a retained entry for their whole body. A process-global
    /// counter would make T1 (`== 1`), T2 (`-> 0`), T4 (`-> 0`) and T10 (`== 2`)
    /// redden each other deterministically on a multicore box — not a flake, a
    /// guaranteed failure. **No process-global mutable state is introduced by
    /// this design; any counter a test asserts on hangs off `ProcRegistry`.**
    ///
    /// **Paired with the `Option<Box<dyn Child>>`, not with "observed an exit"**
    /// (design M3): `+1` where `leader = Some(child)` is installed in
    /// `try_spawn_pty`, `-1` wherever a `leader.take()` yields `Some` (today
    /// only `Drop for ProcEntry`). Counting from exit instead would (a) never
    /// decrement on the two fail-loud `Drop` arms, (b) underflow `AtomicUsize`
    /// to `usize::MAX` on the `Unexpected` waiter arm, and (c) measure the wrong
    /// quantity, since a *running* leader also consumes an `RLIMIT_NPROC` slot.
    ///
    /// It is deliberately **not** derived from the registry `HashMap` (design
    /// D2): `try_spawn_pty` overwrites same-`proc_id` entries, and the displaced
    /// entry keeps its own clone of this `Arc` until its own `Drop` — so an
    /// orphaned, permanently-retained entry is still counted. T10 locks that.
    pin_count: Arc<AtomicUsize>,
    /// #1013 (PR-B): how many pty leaders this registry has *lost the pin on*
    /// (the kernel answered `ECHILD`, §2.4). Registry-scoped for the same
    /// reason as `pin_count`.
    pin_lost_count: Arc<AtomicUsize>,
}

struct ProcEntry {
    /// #1013: with `live_pids()` gone, this number has exactly two readers in
    /// the crate — `existing_live_pid` (whose only destination is the
    /// cross-process `ControlReply::Spawned { pid }`, out of scope for
    /// INV-1013-PTY, §5.3) and `pgid_lease` internals. Adding a third reader
    /// that turns it into a signal target is the defect #1013 is about: route
    /// it through `pgid_lease::group_target` instead.
    pid: u32,
    io_mode: IoMode,
    runtime: ProcRuntime,
    byte_ring: StdMutex<ByteRing>,
    cursor_tail: AtomicU64,
    cursor_head: AtomicU64,
    exit: StdMutex<Option<ProcExit>>,
    /// Pty only: set by the waiter the instant it *observes* the leader's exit,
    /// i.e. *before* the drain grace and the sticky `exit` write. Liveness
    /// probes must consult it, otherwise an exited child looks alive for the
    /// whole grace window and `EnsureProc` hands out a dead pid (issue #993 R4).
    ///
    /// **#1013 (PR-B) renamed this from `pty_reaped`, and the rename is the
    /// point**: the waiter now uses `waitid(P_PID, .., WEXITED | WNOWAIT)`,
    /// which observes the exit *without reaping*. Reaping happens exactly once,
    /// in `Drop for ProcEntry`. So "observed" and "reaped" are two different
    /// instants for the first time and the old name would now be a lie. The
    /// semantics of the bit itself are unchanged — same instant, same readers.
    exit_observed: AtomicBool,
    /// #1013 (PR-B, design M2): the pty waiter did not run to completion — it
    /// panicked or returned early between observing the exit and its `disarm()`.
    /// Set by `WaiterCompletion::drop`, which also seals/publishes a degraded
    /// exit and schedules removal so the pinned leader can still be reaped.
    /// Visible in `debug_entry_stats` so the degraded state is diagnosable.
    waiter_degraded: AtomicBool,
    /// #996: 这条 entry 最早可以在什么时刻被清扫掉。`None` = 尚未退出。
    /// 唯一的回收簿记 —— 没有墓碑位、没有字段级降级开关。
    remove_after: StdMutex<Option<std::time::Instant>>,
    broadcast_tx: broadcast::Sender<DataFrame>,
    /// #1013 (PR-B): this entry's clone of its registry's pin counter. See
    /// `ProcRegistry::pin_count` for why it is registry-scoped and why it is
    /// paired with the `Option<Box<dyn Child>>` rather than with an exit.
    /// Pipe entries carry it but never touch it — they install no leader.
    pin_count: Arc<AtomicUsize>,
}

/// #1013 (PR-B): the crate's **only** reap of a pty leader.
///
/// The waiter observes the exit with `WNOWAIT` and never reaps, so the leader
/// stays a zombie — and therefore its pid, and the pgid numerically equal to
/// it, stay allocated to us — for as long as any `Arc<ProcEntry>` lives
/// (INV-1013-PTY). This `Drop` is where that pin is finally released.
///
/// **Ordering is implicit and load-bearing**: `Drop::drop` runs *before* the
/// struct's fields are dropped, so this `try_wait()` is guaranteed to run
/// before `UnixMasterWriter::drop`'s blocking `write_all` on the master fd
/// (portable-pty-0.9/src/unix.rs:393-405). Correct, but not visible without
/// this comment.
///
/// `try_wait()` is `waitpid(pid, WNOHANG)`: it **never blocks**, at any of the
/// eight `Drop` trigger points enumerated on `sweep_expired_entries` —
/// including the one that is still inside the registry lock and the ones on a
/// tokio worker.
impl Drop for ProcEntry {
    fn drop(&mut self) {
        // **Pipe is an explicit arm, not an accident** (design D6). A Pipe
        // entry is the same struct, and its child is owned by tokio (plus the
        // by-pid blocking `waitpid` in `await_ready_phase`). Gating only on
        // `exit_observed`/`leader.is_some()` happens to be safe today purely
        // because nothing sets those for Pipe; the day someone does, this
        // becomes the double reap §2.3 exists to prevent.
        let ProcRuntime::Pty { leader, .. } = &self.runtime else {
            return;
        };
        let taken = leader
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take();
        // Idempotence is ownership, not bookkeeping: `Option::take` is what
        // makes "reap at most once" a property of the type system rather than
        // of an `AtomicBool` someone can forget to check.
        let Some(mut child) = taken else {
            return;
        };
        // Structurally paired with the `take()` above, per design M3 — this is
        // the *only* place the count can go down, and it goes down on every
        // path below including the two fail-loud ones.
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

    /// #996: 安排回收时刻。只会前移不会后退 —— `Cleanup`（"我不要这条了"）可以
    /// 把它提前到"立刻"，而随后到达的 waiter 不得再把宽限期加回去。
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

    /// #996: 这条 entry 现在可以整条从 registry 移除吗？三个条件缺一不可：
    ///
    /// 1. **已安排回收**且宽限期已过 —— 宽限期内 entry 完整保留，刚断开的
    ///    客户端重连仍拿得到 sticky exit 与最后一屏 replay。
    /// 2. **sticky exit 已落定** —— 否则移除会连"进程怎么死的"一起丢掉。
    /// 3. **master 读到过 EOF**（`eof_reached`）—— "没人持有 pty slave"的
    ///    可判定形式。
    ///
    ///    为什么要这一条：`portable-pty` 的 `UnixMasterWriter::drop` 会往
    ///    master 写 `['\n', VEOF]`（portable-pty-0.9/src/unix.rs:393-405）。
    ///    还有人攥着 slave 时，那两个字节就是孙子进程 stdin 上的换行 +
    ///    Ctrl-D，足以把它直接踢死 —— 正是 #993 花一整轮保护的对象。等到
    ///    master EOF 之后再 drop，写 master 只会拿到 EIO，无害。
    ///
    ///    为什么**不是** `PtyDrainGate::is_drained()`：那个信号说的是"reader
    ///    线程不再产出 `Output`"，#993 有意让它在 EOF / read 错误 / panic 三
    ///    条路径上都触发（`DrainGuard::drop`），这样 `Exited` 永不丢失。但
    ///    "reader 结束"⊅"slave 全关"：孙子进程还攥着 slave 时 reader 若因
    ///    read 错误或 panic 退出，闸门照样落下，用它做移除判据就会 drop
    ///    writer、把 `\n`+VEOF 打进活着的孙子进程。所以 #996 用一个独立的、
    ///    只在 `read() == Ok(0)` 时置位的 `eof_reached`。
    ///
    ///    在 Linux 上这不牺牲任何回收：`portable-pty` 的 `impl Read for PtyFd`
    ///    把 master 的 `EIO` 翻译成 `Ok(0)`（"EIO indicates that the slave pty
    ///    has been closed"），也就是说**正常的 slave 全关在这里就是 `Ok(0)`**，
    ///    `Err(e)` 分支留给真正的异常。
    ///
    ///    注意这里有两层保护，互相独立：这个谓词管住"registry 什么时候撒手"，
    ///    而 writer 真正被 drop 的时机由所有权决定 —— reader 线程自己也持一份
    ///    `Arc<ProcEntry>`，所以只要 reader 还在跑，即便 registry 提前撒手
    ///    writer 也不会归零。两层都失效才会注入 —— 而"reader 已死 + slave 仍
    ///    被持有"正是这样一个场景，第 3 条就是为它设的。
    ///
    /// 推论：**孙子进程持有 pty 时这条 entry 不会被移除**。这不是泄漏，是正确的
    /// 资源追踪 —— 有东西还攥着这个终端，它就该留着（reader 也还在把 master
    /// 排空，见 #993 R3）。孙子进程一走，master EOF，下一次清扫就收掉。
    fn removable(&self, now: std::time::Instant) -> bool {
        let due = self
            .remove_after
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .is_some_and(|at| at <= now);
        if !due {
            return false;
        }
        // 只有 pty entry 走清扫器。pipe entry 的 `exit` 从不被写入，于是
        // `handle_cleanup` 的 `still_running` 恒为 true、永远走不到
        // `schedule_removal`，pipe 的回收由 `await_ready_phase` 里的 waitpid
        // 任务直接 `entries.remove` 完成 —— 这条分支在当前代码里不可达，取
        // fail-closed 的 `false`，绝不让清扫器去 drop 一个它不了解的 runtime。
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
        /// #996: master 是否读到过 EOF —— 即"再没有任何 fd 持有 slave"。只由
        /// reader 在 `read() == Ok(0)` 时置位，是清扫谓词的安全闸
        /// （见 `ProcEntry::removable`）。
        eof_reached: Arc<AtomicBool>,
        /// #1013 (PR-B): the leader's `Child` handle, and its **only** owner.
        /// `Some` = not reaped, `None` = reaped.
        ///
        /// This is not a bookkeeping bit, it *is* the ownership: the waiter
        /// never holds the handle (it gets a bare `u32`), and `Option::take()`
        /// in `Drop for ProcEntry` makes "reaped at most once" a type-system
        /// property. Holding the handle costs nothing — `portable-pty` puts no
        /// `Drop` on it and `std::process::Child` has none either, so dropping
        /// it would not reap and keeping it does not consume anything beyond
        /// the zombie the `WNOWAIT` observation deliberately retains.
        leader: StdMutex<Option<Box<dyn portable_pty::Child + Send + Sync>>>,
        /// #1013 (PR-B, §2.4): `waitid` answered `ECHILD` — the kernel told us
        /// this pid is no longer our child, i.e. the pin is *proven* broken
        /// (something set `SIGCHLD` to be auto-reaping, so the child never
        /// became our zombie).
        ///
        /// **Best-effort detection, NOT fail-closed, and the difference
        /// matters.** The kernel decides auto-reaping at the moment the child
        /// *exits* and frees the number right then; a userspace flag set when
        /// our `waitid` returns is strictly after the fact and cannot close
        /// that window. What it buys: this entry stops offering `entry.pid` as
        /// a group signal target from here on, plus an ERROR to diagnose from.
        /// What it does not buy: the release→flag gap (§6.4), which no
        /// userspace mechanism can cover.
        ///
        /// Monotonic and per-entry: an already-pinned zombie is *not* taken
        /// away by a later auto-reap setting (measured, design E4c), so only
        /// *future* exits can lose their pin and nothing ever needs undoing.
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
    /// Set exactly once, by the pty waiter, in the same critical section that
    /// publishes `DataFrame::Exited` (issue #993). Once sealed the ring is
    /// immutable: the reader thread must neither append nor broadcast, so
    /// `Exited` is provably the last frame and `exit.cursor` is provably the
    /// final `cursor_tail` — on the drain-timeout path too.
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

/// #996: 只给测试断言用的 entry 快照。
#[doc(hidden)]
#[derive(Debug, Clone, Copy)]
pub struct EntryDebugStats {
    pub buffered_bytes: usize,
    /// The sticky `exit` slot has been stamped. **#1013 (PR-B) renamed this
    /// from `exited`**: with `exit_observed` next to it, one unqualified
    /// "exited" for two genuinely different instants is exactly the confusion
    /// the rename exists to remove.
    pub exit_recorded: bool,
    /// The waiter has *observed* the leader's exit (`waitid(.., WNOWAIT)`
    /// returned) — earlier than `exit_recorded`, which additionally waits out
    /// the drain grace. Under the #1013 pin the leader is a retained zombie at
    /// this point, so `kill(leader, 0) == 0` forever: tests that need "the exit
    /// has happened" must poll this bit, never pid liberation.
    pub exit_observed: bool,
    /// Pty only: the kernel answered `ECHILD`, so this entry's pin is proven
    /// broken and it refuses to be a group signal target (§2.4).
    pub pin_lost: bool,
    /// The pty waiter did not run to completion (design M2).
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

    /// #996: 测试用 —— 缩短退出到移除之间的宽限期。
    #[doc(hidden)]
    pub fn with_pty_reclaim_grace(mut self, grace: Duration) -> Self {
        self.pty_reclaim_grace = grace;
        self
    }

    /// 测试专用 —— 拉长 reap 到 sticky-exit 之间的排空窗口，让"仍在排空宽限期
    /// 内"成为一个可建立的状态，而不是一场 50ms 的竞速。
    ///
    /// TEST-ONLY. Production never calls this, so the effective drain grace in
    /// production is always the `PTY_DRAIN_GRACE` constant. Note that
    /// `terminal_renderer::EXIT_PERSIST_GRACE`'s const-assert pins that
    /// **constant**, not this field — widening the field in a test therefore
    /// does not, and must not be read as, relaxing that invariant.
    #[doc(hidden)]
    pub fn with_pty_drain_grace(mut self, grace: Duration) -> Self {
        self.pty_drain_grace = grace;
        self
    }

    /// #996: registry 当前持有的 entry 数。
    #[doc(hidden)]
    pub fn debug_entry_count(&self) -> usize {
        self.inner.lock().map(|entries| entries.len()).unwrap_or(0)
    }

    /// #996: 供测试断言"宽限期内 replay 仍完好 / 退出状态已落定"。entry 一旦
    /// 到期就整条消失，所以返回 `None` 本身就是"已回收"的断言。
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

    /// #1013 (PR-B): how many un-reaped pty leader handles **this registry**
    /// owns — i.e. how many pids it is currently pinning. See the field's doc
    /// for why it is registry-scoped (C2) and why it counts handles rather than
    /// observed exits (M3).
    ///
    /// Deliberately reads the counter and not the registry map: an entry that
    /// was displaced by a same-`proc_id` respawn has left the map but still
    /// owns its handle, and T10 exists to keep that visible.
    #[doc(hidden)]
    pub fn debug_pin_count(&self) -> usize {
        self.pin_count.load(Ordering::SeqCst)
    }

    /// #1013 (PR-B): how many pty leaders **this registry** has lost the pin on
    /// (`waitid` answered `ECHILD`, §2.4).
    #[doc(hidden)]
    pub fn debug_pin_lost_count(&self) -> usize {
        self.pin_lost_count.load(Ordering::SeqCst)
    }

    /// #996: 测试用故障注入 —— 把 pty master 置为 `O_NONBLOCK`，于是 reader 的
    /// 下一次 `read()` 拿到 `EAGAIN`（`Err`）而不是 `Ok(0)`，走的是生产代码里
    /// 那条真实的"read 错误"退出路径：`DrainGuard` 照常落闸（#993 有意为之），
    /// 但 `eof_reached` 保持 false。
    ///
    /// 这是集成测试里唯一能真实制造"reader 线程已死 + slave 仍被孙子进程持有"
    /// 的手段，也就是 `ProcEntry::removable` 第 3 条唯一可证伪的场景：把那一条
    /// 换回 `PtyDrainGate::is_drained()`，entry 就会被清扫、writer 归零、
    /// `\n`+VEOF 打进活着的孙子进程。没有它，那条断言只是空转。
    ///
    /// reader 此刻多半正阻塞在 `read()` 上，改标志不会把它叫醒 —— 调用方必须
    /// 随后制造一次 master 可读事件（例如 `WriteStdin`，行规程的回显就够）。
    ///
    /// 返回 `false` 表示 proc 不存在、不是 pty，或 fd 操作失败。
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

    /// #996: 清扫周期 —— 由宽限期推导，不额外开旋钮。
    fn sweep_interval(&self) -> Duration {
        (self.pty_reclaim_grace / 4).clamp(PTY_SWEEP_MIN, PTY_SWEEP_MAX)
    }

    /// #996: 唯一的回收路径 —— 把所有满足 `ProcEntry::removable` 的 entry 整条
    /// 从 registry 移除。没有字段级降级、没有墓碑：最后一个 `Arc<ProcEntry>`
    /// 归零时，ring、broadcast 通道、pty master 与 writer 由 Rust 所有权一次性
    /// 释放。返回本轮移除的条数（供日志/测试）。
    ///
    /// 一个周期性任务扫全表，而不是每条退出记录起一个定时线程：零件从 N 降到
    /// 1，且 registry 的锁本来就有。
    ///
    /// **析构严格在锁外**：本函数跑在 `serve_with_listener` 的 select 循环里，
    /// 而移除的 entry 常态下就是最后一个 `Arc` 持有者，drop 它会连带 drop
    /// `UnixMasterWriter`，后者的 `Drop` 对 master fd 做**阻塞式**
    /// `write_all(&[b'\n', eot])`（portable-pty-0.9/src/unix.rs:393-405）。
    /// slave 已关时它拿 EIO 立刻返回；但只要那个写有可能阻塞，在锁内 drop 就是
    /// 攥着 registry 全局锁做同步 I/O —— 整个 supervisor 陪葬。所以：retain 时
    /// 只把被移除的 `Arc` 收进 `doomed`，先 `drop(entries)` 放锁，再让 `doomed`
    /// 离开作用域。
    ///
    /// **#1013 (D7)：这条"析构严格在锁外"的约束并非在所有 `Drop` 触发点上都成立，
    /// 完整枚举如下，供下一个人核对，不要以为只有本函数需要小心：**
    ///
    /// | # | 触发点 | 线程 | 锁 |
    /// |---|---|---|---|
    /// | 1 | 本函数的 `drop(doomed)` | serve select 循环 | 锁外（先 `drop(entries)`） |
    /// | 2 | `try_spawn_pty` 同名 `proc_id` 覆盖插入 | 请求 handler | **曾在锁内**；#1013 PR-A 把 `insert` 的返回值绑定出来后再 drop |
    /// | 2b | `try_spawn_pipe` 同名 `proc_id` 覆盖插入 | 请求 handler | **与 #2 同形状，同样曾在锁内**。被覆盖的旧 entry 可以是 **Pty**：`existing_live_pid` 的 Pty 分支对"已退出但还在宽限期"的 entry 故意不就地移除、直接返回 `None`，随后一次 `io_mode: Pipe` 的 `EnsureProc` 就会走到这里把它顶掉，于是 `UnixMasterWriter::drop` 的阻塞写落在 registry 锁内。同样绑定返回值后再 drop |
    /// | 3 | `existing_live_pid` 的 Pipe 分支 `remove` | 请求 handler | **结构上安全**：`.map(\|mut entries\| entries.remove(..))` 把 guard **move 进闭包**，闭包体结束时 guard 先析构，被移除的 `Arc` 作为返回值离开闭包后才在语句末尾析构 —— 天然锁外。不是 #2 那个形状（早先的表把它写成"与 #2 同形状"，是错的） |
    /// | 4a | `await_ready_phase` 的 **readiness 失败** `remove` | 请求 handler 任务（`await_ready_phase` 自己） | 同 #3：`.map(\|mut entries\| entries.remove(..))`，guard 在闭包内先析构，结构上锁外 |
    /// | 4b | `await_ready_phase` 里 **`reap_children` 那个 `tokio::spawn`** 的 `remove` | 被 spawn 出来的 tokio 任务（不是 handler） | 同 #3 的形状，结构上锁外。**单列一行**：早先的表把 4a/4b 合成一行，线程列写的是 4b 的线程、位置指的却是两处 —— 这张表宣称"完整枚举"，这一行已经因为同样的合并被纠正过两次 |
    /// | 5 | `handle_cleanup` → `sweep_expired_entries` | tokio worker | 锁外 |
    /// | 6 | 各 handler 里 `lookup_proc` 克隆出去的那份 `Arc` | tokio worker | 锁外，**但在 tokio worker 上**：`UnixMasterWriter::drop` 的阻塞写今天就可能落在 worker 线程上。既有缺陷，本次不修（Q8） |
    /// | 7 | reader / waiter 线程结束 | 各自的 OS 线程 | 锁外 |
    /// | 8 | `Drop for InProcessProcSupervisor` / 进程退出 | 测试线程 | 锁外 |
    ///
    /// **同一个理由派生出 `pgid_lease` 的规约 1**（M8）：signal lease 只能从
    /// **克隆出来的 `Arc<ProcEntry>`** 上取，绝不能从 registry 的 `MutexGuard`
    /// 上取 —— 否则那次 `libc::kill` 就跑在 registry 全局锁里，与本段要避免的
    /// 阻塞式 `write_all` 是同一类事故。
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

    /// #388's "supervisor death drops procs": group-SIGTERM every registered
    /// proc on the way out. **Pipe entries are in scope and must stay in
    /// scope** — `try_spawn_pipe` does `cmd.process_group(0)`, there is no
    /// PDEATHSIG, and this is the *only* mechanism that kills a pipe daemon
    /// when the supervisor dies.
    ///
    /// **The gate is the `--lib` test
    /// `pgid_lease_tests::pipe_target_is_ok_kind_readable_and_refused_by_the_signal_rpc`**,
    /// not `server_restart_survives.rs`. If `group_target` starts returning
    /// `Err` for Pipe, `server_restart_survives` **hangs rather than fails**:
    /// `src/main.rs` is `#[tokio::main]`, so runtime drop blocks on the
    /// in-flight `spawn_blocking(move || waitpid(pid))` that `reap_children`
    /// started, which means the supervisor process cannot exit before the pipe
    /// child does. The test's `child should die when supervisor exits`
    /// assertion is therefore near-tautological on the happy path, and on the
    /// violating path it just waits out the child. Measured on the PR-A branch
    /// before the elapsed assertion existed: baseline `ok, 0.23s`; with the
    /// Pipe-`Err` mutation still `ok`, at `30.03s` — i.e. exactly the fixture's
    /// own 30s self-exit, which is the tell that nothing killed the child.
    /// `server_restart_survives` now also asserts *elapsed* after the SIGTERM
    /// so that it is a real (if secondary) gate — see the comment there.
    ///
    /// The collect-then-kill shape is kept, but the collected type is now
    /// `Vec<GroupSignalTarget<'_>>`, which **borrows** `entries`. Before
    /// #1013 this loop was safe only because `entries`' drop scope happened to
    /// reach the end of the function; now the borrow checker enforces it
    /// (inserting `drop(entries)` before the kill loop is `E0505`).
    ///
    /// Per `pgid_lease` regulation 1 the leases come from cloned
    /// `Arc<ProcEntry>`s, never from the registry guard: that guard is released
    /// before a single `kill` runs.
    ///
    /// # #1013 PR-B: the `exit.is_none()` filter was **deleted**, not replaced
    ///
    /// Until PR-B this loop skipped every entry whose sticky exit had already
    /// been stamped, because in that state the leader had already been reaped
    /// and its pgid was recyclable — signalling it was the #1013 defect. Under
    /// the pin the leader is a retained zombie for the entry's whole registry
    /// lifetime, so that state is no longer dangerous, and skipping it leaked
    /// grandchildren that outlived a recorded exit. **T5b
    /// (`terminate_all_after_exit_recorded.rs`) is the lock**: it establishes
    /// exactly the newly-covered state and its mutation is putting the filter
    /// back. This deletion is only safe *with* the pin, which is why it could
    /// not ship in PR-A.
    ///
    /// **Do not "replace it with a narrower predicate".** Twice in review the
    /// proposal was some form of `Pty && !pin_lost`; both times that would drop
    /// **Pipe** out of the shutdown group-SIGTERM and break #388, because Pipe
    /// is not Pty. After the deletion the only filtering left is
    /// `filter_map(|e| group_target(e).ok())`, and each of its arms is already
    /// what we want: `Err(PinLost)` drops entries whose pgid the kernel has
    /// proven is no longer ours, and `Ok(PipeBestEffort)` keeps Pipe in
    /// verbatim.
    ///
    /// Every Pty target is the pinned leader pgid. The tty's foreground job
    /// group is deliberately irrelevant to the Signal API: callers are
    /// addressing the child this supervisor spawned, not whichever job that
    /// child has temporarily placed in the foreground.
    ///
    /// Registered consequence (deliberate, and a narrowing versus pre-PR-B):
    /// a `pin_lost` entry is now excluded here too, so its grandchildren get no
    /// group SIGTERM on shutdown. That pgid has been proven by the kernel not
    /// to be ours any more; signalling it *is* #1013.
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
    // #996: 回收器就是这个 accept 循环里的一根定时器分支 —— 不新造监督结构，
    // 生命周期与关停跟着 `shutdown` 走，进程退出时它自然消失。
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

/// #1013 §1.2 — the **only** place in this crate that may compute a "group
/// signal target", and the **only** place that calls `libc::kill(-pgid, ..)`.
///
/// Three claims, three different enforcement mechanisms. All three are stated
/// here because the weaker prose versions of them were broken three times in
/// review:
///
/// * **The number is unreadable — enforced by the compiler.** The pgid lives in
///   `struct Pgid(pid_t)`, whose field is private to this module; reading it
///   from outside is `error[E0616]`. The negative sample that proves this is
///   `mod pgid_escape_probe` (T11), gated behind the `pgid-escape-probe`
///   feature: `cargo check --features pgid-escape-probe` must fail with E0616.
/// * **The computation is borrow-checked — for as long as the value keeps its
///   original lifetime.** A `GroupSignalTarget<'a>` produced by `group_target`
///   borrows the `&'a ProcEntry` it was derived from, so *that value* cannot
///   outlive the borrow. It is **not** true that the target "cannot be stashed
///   in anything longer-lived": the parent module can destructure and re-wrap
///   it, and doing so relabels the lifetime without touching the number. See
///   regulation 5 — that seam is lint/review-enforced, not borrow-checked.
/// * **"Do not bypass the computation" is lint-enforced only.** Nothing here
///   stops someone from writing another `libc::kill(-n, sig)`, or from
///   conjuring a number out of `Spawned { pid }` / a `/proc` scan / the pid in
///   the database. Note this list enumerates only *fresh-number* bypasses; the
///   re-wrap seam of regulation 5 needs no new number at all. That layer is T7
///   plus code review, and nothing more.
///
/// ## Module regulations (five; do not delete them, each one has a scar)
///
/// 1. **A lease may only be taken from a cloned `Arc<ProcEntry>`, never from
///    the registry's `MutexGuard`** (M8). Taking one from the guard is the
///    shortest way to satisfy borrowck, and it puts a `kill` syscall inside the
///    global registry lock — exactly the hazard `sweep_expired_entries` spends
///    a paragraph avoiding. Both consumers comply today: `lookup_proc` clones
///    and then releases the lock, and `terminate_all_process_groups_sync`
///    collects a `Vec<Arc<_>>` before dropping the guard.
/// 2. **No function here may return `GroupSignalTarget<'static>`** (M11).
///    `Box::leak(Box::new(entry))` yields a `&'static ProcEntry` and hence a
///    lease that never expires. It happens to be harmless today (a leaked entry
///    is never dropped, so the pin really is eternal), but that is a
///    coincidence, not an argument.
/// 3. **Only `Leader` is covered by INV-1013-PTY** (and only once PR-B lands).
///    See the per-variant docs.
/// 4. **`group_target` must NEVER return `Err` for a Pipe entry.**
///    `terminate_all_process_groups_sync` collects targets with
///    `filter_map(|e| group_target(e).ok())`, so any `Err` escaping from the
///    Pipe branch silently removes Pipe entries from the shutdown path — which
///    is the #388 "supervisor death drops procs" breakage. **The gate that
///    goes red is the `--lib` test
///    `pgid_lease_tests::pipe_target_is_ok_kind_readable_and_refused_by_the_signal_rpc`.**
///    `server_restart_survives` does *not* fail on this violation — it
///    **hangs**: `src/main.rs` is `#[tokio::main]`, so runtime drop waits on
///    the in-flight `spawn_blocking(move || waitpid(pid))` that `reap_children`
///    started, and the supervisor therefore cannot exit before the pipe child
///    does. Measured: baseline `ok, 0.23s`, mutated `ok, 30.03s` (the fixture's
///    own 30s self-exit — the tell that nothing killed the child). That
///    test now carries an elapsed assertion so it is at least a secondary gate.
///    Pipe is excluded from the **`Signal` RPC only**, and `PipeNotSignalable`
///    may only ever be produced by `require_addressable_by_signal_rpc`.
/// 5. **Do not move a `GroupSignalTarget` out of its variant and re-wrap it**
///    (#1013 review). Because enum variant fields inherit the enum's
///    visibility, the parent module can write
///    `match t { GroupSignalTarget::Leader(p, _) => GroupSignalTarget::Leader(p, PhantomData), .. }`
///    and get a `GroupSignalTarget<'static>` carrying the same, possibly
///    already-stale, pgid past the entry's drop — no new number required, and
///    it compiles. `PhantomData` pins nothing on its own, so this seam is
///    **lint- and review-enforced only**, exactly like regulation 2's spirit
///    but not reachable by the same `'static`-in-a-signature ban.
mod pgid_lease {
    use super::{Ordering, ProcEntry, ProcRuntime};
    use std::io;
    use std::marker::PhantomData;

    /// A **struct, not an enum field**: enum variants and their fields always
    /// inherit the enum's visibility (`error[E0449]: visibility qualifiers are
    /// not permitted here`), so a variant field can never be "kind public,
    /// number private". Only a newtype with a private field can.
    /// **Do not flatten this back into the variants** — that restores the
    /// original defect in one line (`let GroupSignalTarget::Leader(pgid, ..)`
    /// would copy the `i32` straight out of the borrow).
    ///
    /// No `Copy`, no `Clone`, no `Display`/`Debug`, no accessor. A `Display`
    /// that prints a decimal pgid is an `i32` accessor spelled in text.
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
        /// Target = the leader's pgid (numerically `entry.pid`). **Pinned by
        /// the leader zombie** (INV-1013-PTY): the waiter observes the exit
        /// with `WNOWAIT` and only `Drop for ProcEntry` reaps, so while any
        /// `Arc<ProcEntry>` is alive this number is still allocated to us and
        /// cannot have been handed to an unrelated process group.
        Leader(Pgid, PhantomData<&'a ProcEntry>),
        /// Target = the pipe daemon's pgid (`try_spawn_pipe` does
        /// `cmd.process_group(0)`, so the daemon leads its own group).
        /// **Never pinned** — the child belongs to tokio. Only
        /// `terminate_all_process_groups_sync` may use it (#388 payload);
        /// `handle_signal` must refuse it, see
        /// `require_addressable_by_signal_rpc`.
        PipeBestEffort(Pgid, PhantomData<&'a ProcEntry>),
    }

    impl GroupSignalTarget<'_> {
        /// The variant name, for diagnostics. **Never the number** (R6).
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

    /// The stable error shape the tests assert on. Rendered by
    /// `super::group_signal_error_reply` — proc_id and target *kind*, never a
    /// decimal pgid.
    pub(super) enum GroupSignalError {
        /// Produced **only** by `require_addressable_by_signal_rpc` — see
        /// regulation 4.
        PipeNotSignalable,
        /// Produced only by `group_target`'s Pty branch, once the waiter has
        /// recorded `pin_lost` (the kernel answered `ECHILD`, §2.4). It must
        /// stay textually distinguishable from `Kill(ESRCH)`: T6 can only be
        /// reddened by its own mutation because those two render differently.
        PinLost,
        /// `kill_group`'s `libc::kill` returned -1.
        Kill(io::Error),
    }

    /// The only constructor. `Err` is only ever possible on the **Pty** branch
    /// (`PinLost`). See regulation 4: the Pipe branch must
    /// always be `Ok(PipeBestEffort)`.
    pub(super) fn group_target(
        entry: &ProcEntry,
    ) -> Result<GroupSignalTarget<'_>, GroupSignalError> {
        match &entry.runtime {
            ProcRuntime::Pipe { .. } => Ok(GroupSignalTarget::PipeBestEffort(
                Pgid(entry.pid as libc::pid_t),
                PhantomData,
            )),
            ProcRuntime::Pty { pin_lost, .. } => {
                // #1013 PR-B §2.4. Deliberately one deletable line: T6's second
                // mutation is deleting it, and T6 then has to notice by message
                // prefix that it got `Kill(ESRCH)` instead of `PinLost`.
                // Deliberately NOT `anyhow::ensure!` — this function returns
                // `Result<_, GroupSignalError>` and `ensure!` always produces
                // `anyhow::Error`, so that spelling does not compile (E0308).
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

    /// The `Signal` RPC's admission gate. Its own function so that T9's
    /// mutation is exactly one line at the call site.
    ///
    /// Pipe procs are not group-signalable via the `Signal` RPC; group
    /// termination for pipe procs happens only through supervisor shutdown
    /// (`terminate_all_process_groups_sync`, the #388 payload).
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

/// T11's compile-time negative sample. **This module exists in order to fail
/// to compile.** The gate asserts that
/// `cargo check -p calm-proc-supervisor --features pgid-escape-probe`
/// exits non-zero *and* prints `E0616`.
///
/// **What a green (i.e. failing-to-compile) gate does and does not claim.** The
/// probe reads exactly one thing, `p.0`, so T11 pins exactly one property: *the
/// `Pgid` tuple field is not readable outside `pgid_lease`*. It does **not**
/// catch an added `pub(super) fn raw2()` or an added `#[derive(Debug)]` — both
/// leave `p.0` private and the gate stays green. Flattening the number back
/// into the variants is caught only *indirectly*: `p.0` then applies to a bare
/// `libc::pid_t` and rustc emits `E0609`, not `E0616`, so CI fails with the
/// misleading "the probe is broken, not the invariant" message. Red is red, but
/// do not read that message literally without checking the variants first.
///
/// **Where the real guarantee comes from: the type system, not this gate and
/// not the grep next to it.** Two review channels wrote and *compiled* seven
/// distinct escape attempts against this design. Only three succeeded: an
/// `unsafe` transmute, the acknowledged destructure/re-wrap seam (regulation 5
/// above), and simply writing a fresh `libc::kill(-n, sig)` from a number
/// obtained elsewhere. Everything else was a compile error.
///
/// **The grep ratchet next to the T11 step in CI is defense-in-depth against
/// accidental drift, not a proof.** It catches the shapes enumerated in its own
/// comment — a second or `pub` method on `impl Pgid`, any `pub(super) fn` in
/// the module returning a `pid_t` (free function or method on another type), a
/// trait impl with `Pgid` on either side of `for`, and any `derive` — and a
/// determined author can still route the number out past it, because a text
/// scan can never be complete. Do **not** rewrite this paragraph into "the
/// accessor and derive cases are covered by the grep ratchet": that sentence
/// was written twice and a reviewer defeated it twice, both times with CI
/// green.
///
/// **This crate can never be built with `--all-features`**: the whole point of
/// the `pgid-escape-probe` feature is that enabling it makes the crate fail to
/// compile, so `cargo check/test --all-features` necessarily fails at this
/// module. No workflow uses `--all-features` today; if you type it by hand,
/// this is why. Enable features explicitly.
///
/// It cannot be a trybuild case or a `compile_fail` doctest: both compile the
/// sample as an **external** crate, where `pgid_lease` (all `pub(super)`) is
/// not even nameable, so the sample would "fail" on an unresolved path and the
/// gate would pass vacuously. The property under test is crate-internal,
/// module-external visibility, so the sample must live inside the crate.
#[cfg(feature = "pgid-escape-probe")]
mod pgid_escape_probe {
    pub(super) fn read_the_number(t: &super::pgid_lease::GroupSignalTarget<'_>) -> libc::pid_t {
        let super::pgid_lease::GroupSignalTarget::Leader(p, _) = t else {
            return 0;
        };
        p.0 // ← must be error[E0616]: field `0` of struct `Pgid` is private
    }
}

/// Renders a `GroupSignalError` into the one `ControlReply` frame the client
/// gets. **Exhaustive on purpose**: `handle_signal` must never `?` a
/// `GroupSignalError` into `anyhow`, because that writes no frame at all and
/// the client just sees the connection close.
///
/// These four messages are an asserted interface, not log wording (§1.2).
/// Note the deliberate narrowing versus the pre-#1013 text: the `Kill` message
/// used to carry the decimal pgid, which was a textual escape hatch for the
/// number; it now names the target *kind*.
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
                // The cause is deliberately generic: `pin_lost` is set on two
                // waiter arms, only one of which is `ECHILD`. Naming ECHILD
                // unconditionally would mis-diagnose an operator reading this
                // reply after a `waitid` failure that proved nothing.
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

/// The `Signal` RPC's whole decision, as a synchronous function so that the
/// lease never crosses an `.await`. Per module regulation 1 the lease is taken
/// from a cloned `Arc<ProcEntry>` (`lookup_proc` already released the registry
/// lock), never from a registry guard.
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
    // Pty: `pty_running` goes false the moment the child is reaped, so a
    // cleanup arriving inside the drain grace no longer bounces with
    // WrongState (issue #993 R4). Pipe: unchanged sticky-exit semantics.
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
    // #996: `Cleanup` = "我不要这条了"，于是把回收时刻提前到"立刻"，然后就地
    // 扫一次。跳过的是宽限期，**不是**安全闸：`ProcEntry::removable` 依然要求
    // sticky exit 已落定、master 已 EOF，所以
    //   * 一个落在 `exit_observed=true` 与 seal 之间的 cleanup 不会把退出状态提前
    //     丢掉（#993 R2/F6 的老坑）——它只会在下一轮清扫时生效；
    //   * 孙子进程还攥着 slave 时不会 drop writer，也就不会注入 `\n`+VEOF。
    //
    // 因此 `CleanupOk` 的语义是**"已排期"**而非"已移除"：安全闸未满足时这一扫
    // 会空手而归，真正的移除落到后续某次周期性清扫。回复里如实说明见
    // `ControlReply::CleanupOk` 的文档；没有等待环节 —— 等下去就是在 async
    // handler 里对一个可能永远不满足的条件阻塞（孙子进程可以活很久）。
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

    // `EnsureProcRequest.cwd` is INTENTIONALLY NOT APPLIED here.
    //
    // Pre-#388 `spawn_daemon_with_parts` never set the daemon process's
    // cwd: the desired cwd is only passed via the `--cwd` argv flag for
    // the daemon to apply to its PTY child. Applying it as the daemon
    // process's own cwd breaks callers that name a directory the daemon
    // will create (or that doesn't need to exist for the supervisor /
    // daemon themselves) — e.g. `track_create_sync_daemon`'s
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

    // #1013: the same hard-fail `try_spawn_pty` does, for the same reason, on
    // the runtime that the group-signal path actually still signals. A pid of 0
    // becomes `Pgid(0)` and `terminate_all_process_groups_sync` would then run
    // `kill(-0, SIGTERM)` — i.e. SIGTERM the supervisor's own process group.
    // Pipe is precisely the variant that path must keep signalling (#388), so
    // leaving `unwrap_or_default()` here while hard-failing the pty branch was
    // asymmetric hardening. On unix `Child::id()` is `Some(nonzero)` until the
    // child is reaped, so this is unreachable today; it is here so the
    // unreachable case cannot silently become a self-inflicted killpg.
    let pid = match child.id() {
        Some(pid) if pid != 0 => pid,
        observed => {
            // Deliberately **no kill here**, and this is the whole point of the
            // guard. `None` means tokio already reaped the child (`id()` is
            // `None` only for `FusedChild::Done`), so there is nothing to
            // signal. And for a stored pid of `0`, `Child::kill()` bottoms out
            // in `libc::kill(self.pid, SIGKILL)` (tokio-1.52.3
            // process/mod.rs:1326 → process/unix/mod.rs:170 → std
            // sys/process/unix/unix.rs:990-1003) — i.e. `kill(0, SIGKILL)`,
            // which signals the **supervisor's own process group**. That is a
            // strictly worse version of the very self-killpg this guard exists
            // to prevent. Leaking a child in an unreachable branch beats
            // SIGKILLing the supervisor.
            //
            // `child_already_reaped` is therefore only true for `None`; for
            // `Some(0)` the child is neither reaped nor killed, and the caller
            // must not be told otherwise.
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
    // #1013 (D7 #2b): the sibling of the `try_spawn_pty` site below, and it is
    // reachable with a **Pty** victim: `existing_live_pid`'s Pty branch
    // deliberately does not remove a dead-but-in-grace entry and returns
    // `None`, so a following `EnsureProc` for the same `proc_id` with
    // `io_mode: Pipe` lands here and displaces that Pty entry. If the registry
    // held the last `Arc`, dropping it drops `UnixMasterWriter`, whose `Drop`
    // does a **blocking** `write_all(&[b'\n', VEOF])` on the master fd. As a
    // bare statement `entries.insert(..)`'s returned `Option<Arc<ProcEntry>>`
    // is a statement temporary that drops *before* the `MutexGuard`, i.e.
    // inside the registry lock. Bind it, release the lock, then drop.
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
                // Pipe entries carry the counter but never touch it: they
                // install no leader handle, so `Drop for ProcEntry` returns on
                // its explicit Pipe arm before any `fetch_sub`.
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

    // #1013: hard-fail instead of `unwrap_or_default()`. A pid of 0 makes
    // `kill(-0, sig)` signal *the supervisor's own process group* — i.e. the
    // supervisor and every proc it owns. On unix `process_id()` is always
    // `Some(nonzero)`, so this is unreachable today; it is here so that the
    // unreachable case cannot silently become a self-inflicted killpg.
    //
    // #1013 PR-B folds in the PR-A follow-up that was left here: the guard used
    // to `child.kill()` before bailing, and `portable_pty`'s `ChildKiller` does
    // `kill(stored_pid, ..)` — for a stored pid of 0 that is `kill(0, SIGKILL)`,
    // i.e. the supervisor's **own process group**, the exact self-inflicted
    // killpg this branch exists to prevent. The pipe guard was fixed the same
    // way: signal nothing, and tell the caller the child was not reaped so the
    // caller does not assume it was. We have no usable pid, so there is nothing
    // safe to signal or wait for.
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
            // #1013 PR-B: the handle moves into the entry **at spawn**, before
            // the waiter exists. The waiter is handed the bare `pid` and can
            // therefore never reap, panic-or-not.
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
    // #1013 PR-B (design M3): the increment is structurally paired with
    // installing the handle above, and the only decrement is the matching
    // `leader.take()` in `Drop for ProcEntry`. Counting from "observed an exit"
    // instead would leave the two fail-loud `Drop` arms never decrementing,
    // could underflow this `AtomicUsize` to `usize::MAX` on the waiter's
    // `Unexpected` arm, and would measure the wrong quantity anyway — a running
    // leader occupies an `RLIMIT_NPROC` slot just as a zombie one does.
    registry.pin_count.fetch_add(1, Ordering::SeqCst);
    // #1013 (D7 #2): `insert` returns the entry it displaced, and a same-`proc_id`
    // respawn inside the reclaim grace makes the registry's `Arc` the *last* one
    // — so dropping the returned value drops `ProcEntry`, and with it
    // `UnixMasterWriter`, whose `Drop` does a **blocking** `write_all` on the
    // master fd. As a bare statement the returned temporary is dropped before
    // the `MutexGuard`, i.e. inside the registry lock: the exact hazard
    // `sweep_expired_entries` documents. Bind it, release the lock, then drop.
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

/// How long the waiter thread waits for the pty reader to reach EOF after the
/// child has been reaped, before sealing the ring and publishing `Exited`
/// anyway.
///
/// This window is **not** what makes `Exited` the last frame — the ring seal
/// is (see `spawn_pty_waiter`). It only decides how much genuinely in-flight
/// output we are willing to wait for before declaring the stream over.
///
/// The happy path never spends it: `portable-pty`'s unix reader maps the
/// master's `EIO` to `Ok(0)` (`portable-pty-0.9/src/unix.rs`), so the moment
/// the last slave fd closes the reader sees EOF and signals the gate. The
/// window only bites when a grandchild inherited the slave fd and outlives its
/// parent — then the master never EOFs and an unbounded wait would leave the
/// terminal stuck on "running" forever.
///
/// 50ms sizing: at reap time the still-unread bytes are bounded by the kernel
/// pty buffer (~64 KiB), which the 8 KiB read loop drains in a handful of
/// syscalls — microseconds of work, and a few scheduler wakeups even on a
/// contended single CPU. 50ms is ~3 orders of magnitude above that, while
/// staying well inside the renderer teardown budget on the `calm-server` side
/// (`terminal_renderer::EXIT_PERSIST_GRACE`, which const-asserts the
/// relationship) so a teardown cannot abort the attach reader before the exit
/// is persisted (issue #993 R1).
pub const PTY_DRAIN_GRACE: Duration = Duration::from_millis(50);

/// Handshake between the pty reader thread and the pty waiter thread.
///
/// The waiter is the *single* publisher of `DataFrame::Exited` (see
/// `spawn_pty_waiter`); this gate is how it learns that the reader has stopped
/// producing `Output` frames, so `Exited` can be published strictly after the
/// process' trailing bytes (issue #993).
///
/// It says **"the reader will produce nothing more"**, deliberately including
/// the read-error and panic paths (`DrainGuard::drop`) so `Exited` can never be
/// lost. It does **not** say "the pty slave is closed" — a grandchild may still
/// hold it while the reader dies. Anything that needs the latter (i.e. #996's
/// removal predicate, because dropping the entry drops the writer and injects
/// `\n`+VEOF) must use `ProcRuntime::Pty::eof_reached` instead.
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

    /// Waits for the reader to finish, at most `grace` (production always
    /// passes `PTY_DRAIN_GRACE`; see `ProcRegistry::with_pty_drain_grace`). Returns
    /// `true` when the reader really finished (so no further `Output` frame
    /// can be broadcast), `false` when the grace window expired.
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

/// Signals the drain gate from `Drop`, so the reader thread cannot leave the
/// waiter hanging on any exit path — including an early `return`/`break` or a
/// panic.
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
                    // #996: 只有这一条路径置位 —— `Ok(0)` 是 master EOF，等价于
                    // "再没有任何 fd 持有 slave"（`portable-pty` 把 master 的
                    // EIO 也翻译成 `Ok(0)`，所以 Linux 上正常的 slave 全关就走
                    // 这里）。read 错误 / panic **不**置位：那时 slave 可能还被
                    // 孙子进程攥着，移除 entry 会把 `\n`+VEOF 打进它的 stdin。
                    // 与之相对，`DrainGuard`（#993）在三条路径上都落闸，因为它
                    // 问的是另一个问题："reader 还会不会再产出 Output"。
                    eof_reached.store(true, Ordering::SeqCst);
                    break;
                }
                Ok(n) => {
                    // Append AND broadcast inside the ring critical section.
                    // Doing the broadcast outside would let the waiter's
                    // seal + `Exited` slip between them, so this `Output`
                    // frame would land after `Exited` even though the cursor
                    // it carries was already accounted for (issue #993 R2).
                    let mut ring = match entry.byte_ring.lock() {
                        Ok(ring) => ring,
                        Err(poisoned) => poisoned.into_inner(),
                    };
                    if ring.is_sealed() {
                        drop(ring);
                        // The waiter already published `Exited`; by contract
                        // nothing may follow it, so this chunk is neither
                        // appended nor broadcast. But we must KEEP READING
                        // (issue #993 R3): the surviving grandchild still
                        // holds the slave fd, the entry still owns the master,
                        // and nothing in production ever sends
                        // `ControlMsg::Cleanup`, so an unread master would fill
                        // the ~64 KiB kernel tty queue and wedge the grandchild
                        // forever inside `write()`. Draining to EOF keeps it
                        // running and lets the fd close on its own.
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
                    // #996: 不置 `eof_reached` —— 我们并不知道 slave 是否已全关。
                    // 代价是这条 entry 留在 registry 里直到进程退出（清扫器不会
                    // 收它），换来的是绝不会把 `\n`+VEOF 打进一个可能还活着的
                    // 孙子进程。宁可留一条 entry，不可踢死一个终端。
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

/// Publishes `DataFrame::Exited` for a pty process — the **only** place that
/// does so for a pty (issue #993).
///
/// `child.wait()` returning does not mean the pty master is drained: the
/// reader thread may still be appending the process' trailing output to the
/// ring. Publishing `Exited` at that moment races the last `Output` frames,
/// and since every consumer treats `Exited` as terminal, those bytes are lost
/// and `exit.cursor` under-reports.
///
/// Two mechanisms, with different jobs:
///
/// 1. **Drain gate** (best effort, bounded by `PTY_DRAIN_GRACE`): wait for the
///    reader to finish (EOF or read error) so genuinely in-flight bytes make
///    it into the ring first.
/// 2. **Ring seal** (the actual invariant): the final `cursor_tail` sample,
///    the sticky `entry.exit` write and the `Exited` broadcast all happen in
///    one `byte_ring` critical section that also flips the ring to sealed. The
///    reader takes the same lock around append+broadcast and bails out when it
///    finds the ring sealed.
///
/// What is guaranteed, exactly:
///
/// * **Always** (both paths, seal): `Exited` is the last frame any attacher
///   can observe, and `exit.cursor` equals the ring's final `cursor_tail`. No
///   `Output` frame can be broadcast after `Exited`, and no byte can be
///   appended past `exit.cursor`.
/// * **Happy path** (reader reached EOF within the grace — every process that
///   held the slave is gone): additionally, *every byte the process wrote*
///   is in the ring before `Exited`. Nothing is lost.
/// * **Degraded path** (grace expired because a surviving grandchild still
///   holds the slave fd): the ordering guarantee above still holds, but
///   completeness does not — bytes written to the pty after the seal are
///   *read and discarded*: never appended, never broadcast, never replayed to
///   a future attacher. The reader deliberately keeps draining the master to
///   EOF instead of stopping (issue #993 R3): the entry keeps the master open
///   for the process' whole lifetime and no production path sends
///   `ControlMsg::Cleanup`, so an unread master would fill the kernel tty
///   queue and block the surviving grandchild inside `write()` forever.
///   Losing that output is a deliberate trade against wedging the terminal on
///   "running" forever; both the waiter and the reader emit a WARN when it
///   happens.
///
/// #1013 PR-B: the degraded `(status, signalled)` pair — "the process is over,
/// we could not learn how". Identical in shape to the pre-#1013 `None` arm that
/// covered `child.wait()` failing, so no reader sees a new shape.
const DEGRADED_EXIT_PARTS: (Option<i32>, bool) = (None, false);

/// What one `waitid(P_PID, pid, WEXITED | WNOWAIT)` told us.
enum PtyExitObservation {
    /// The child terminated and is **still waitable** — i.e. it is now a zombie
    /// we own, which is what pins its pid and pgid (INV-1013-PTY).
    Observed { si_code: i32, si_status: i32 },
    /// `ECHILD`: the kernel says this pid is not our child. Something in this
    /// process made `SIGCHLD` auto-reaping, so the child never became our
    /// zombie and its number was released the instant it exited (§2.4).
    PinLost,
    /// Any other error. Not retried and not swallowed.
    Unexpected(io::Error),
}

/// Observes the pty leader's exit **without reaping it**.
///
/// `WEXITED | WNOWAIT` is the whole mechanism of #1013: `waitid` blocks until
/// the child terminates and fills in `siginfo_t`, but per `man 2 waitid` leaves
/// the child "in a waitable state" — a zombie. A zombie still occupies its pid,
/// and pid numbers are only returned to the allocator when the last task
/// detaches (i.e. on reap), so retaining it is the *only* mechanism that keeps
/// a pid number from being recycled. No pidfd, tty reference or other handle
/// can do this.
///
/// **The four arms are written out one by one on purpose**, and in particular
/// `ECHILD` is its own arm:
///
/// * `Ok` — done, the pin now exists.
/// * `EINTR` — the one and only retry arm.
/// * `ECHILD` — **must not** fall into the retry arm. `waitid` would answer
///   `ECHILD` forever, the waiter would spin forever, and the terminal would
///   never publish `Exited`: a hang, which is strictly worse than the bug this
///   design fixes. This arm is what T6's first mutation attacks.
/// * anything else — also not retried; publish a degraded exit and move on, so
///   the terminal still terminates.
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

/// #1013 PR-B: the **only** conversion from a `siginfo_t` pair to the
/// `(exit status, signalled)` shape the rest of the crate speaks, and
/// `spawn_pty_waiter` is its **only** caller.
///
/// It is one function rather than an inline `match` in the waiter so that T3
/// can assert on production wiring instead of on a re-composition of it. If the
/// test had to rebuild `raw status -> ExitStatusExt::from_raw ->
/// portable_pty::ExitStatus -> (status, signalled)` itself, then reverting the
/// waiter to an inline `si_code` match would leave T3 green while the
/// `CLD_DUMPED` regression came straight back. Consequently the waiter must not
/// mention `si_code`, `ExitStatusExt` or `portable_pty::ExitStatus` anywhere.
///
/// Step 1 (synthesising a `wait(2)` status word) is the only genuinely new
/// logic; steps 2 and 3 are the pre-#1013 mapping, unchanged, so that a
/// `WNOWAIT` observation and a `child.wait()` produce identical `ProcExit`s.
///
/// `CLD_DUMPED` has its own arm and it is load-bearing: a core dump has
/// `WIFSIGNALED = 1` (measured: `abort` under `ulimit -c unlimited` gives
/// `si_code = CLD_DUMPED`, `si_status = 6`, `WIFSIGNALED = 1`). A
/// `CLD_KILLED`-only match would synthesise a *normal exit* word and persist
/// the crash as "exited with code 6".
fn proc_exit_parts_from_siginfo(si_code: i32, si_status: i32) -> (Option<i32>, bool) {
    use std::os::unix::process::ExitStatusExt as _;

    let raw = match si_code {
        libc::CLD_EXITED => (si_status & 0xff) << 8,
        libc::CLD_KILLED => si_status & 0x7f,
        libc::CLD_DUMPED => (si_status & 0x7f) | 0x80,
        other => {
            // `WEXITED` alone can only report termination, so `CLD_STOPPED` /
            // `CLD_CONTINUED` / anything else here means our understanding of
            // the call is wrong. Fail loud rather than synthesise a status word
            // from a code we do not understand.
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

/// #1013 PR-B (design C7): the #993 R2 critical section, extracted verbatim so
/// it has exactly **one** implementation — seal the ring, sample the final
/// cursor, stamp the sticky slot, broadcast `Exited`.
///
/// **The whole crate writes the sticky exit here and broadcasts `Exited` here,
/// nowhere else.** That atomicity is what makes `Exited` provably the last
/// frame: the reader takes the same ring lock around append+broadcast, so a
/// second writer outside this section would leave the ring unsealed (output
/// after the exit), never broadcast `Exited` (attached clients hang instead of
/// degrading), and would have to invent a `cursor` — which `handle_attach`'s
/// fast path then compares against `snapshot_tail`.
///
/// **Stamps only if absent.** A sticky slot that already has a value is
/// returned as-is: no re-stamp, and *no second `Exited` frame*, because no
/// reader has a contract for a duplicate terminal frame. `ByteRing::seal()` is
/// idempotent, so the re-entry from `WaiterCompletion::drop` after a normal
/// path has already run is a safe no-op.
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
    // The sticky slot must hold exactly the broadcast value and must be
    // visible no later than the frame: `handle_attach`'s fast path decides
    // correctness with `exit.cursor <= snapshot_tail`. Lock order here
    // (byte_ring → exit) matches `handle_attach`'s.
    // 中毒也必须写：`ProcEntry::removable` 要求 sticky exit 已落定，
    // 跳过这一次写入就等于让这条 entry 永远不可回收（#996 review E）。
    // 与本文件其它 `exit` 访问同一个风格：`poisoned.into_inner()`。
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

/// #1013 PR-B (design M2): an RAII guard that makes "the pty waiter did not run
/// to completion" a recoverable state instead of a permanently pinned zombie.
///
/// Under the pin, a waiter that observes the exit and then panics (or returns
/// early) before stamping the sticky exit and scheduling removal leaves an
/// entry that `ProcEntry::removable` will *never* accept — so the last `Arc`
/// never drops, `Drop for ProcEntry` never runs, and the leader stays a zombie
/// holding an `RLIMIT_NPROC` slot until the supervisor exits. Before PR-B the
/// same panic was cheaper: `child.wait()` had already reaped.
///
/// The guard therefore fills in exactly the two preconditions `removable`
/// needs, **through the one function that owns them** — never a second write
/// path (see `seal_and_publish_exit` for why a separate degraded write would
/// leave the ring unsealed and never broadcast `Exited`).
///
/// What it buys: the entry becomes reclaimable and the zombie is reaped on the
/// normal schedule. What it does not buy: `removable`'s third condition
/// (`eof_reached`) is untouched, so a grandchild holding the slave keeps the
/// entry alive exactly as it does on the happy path — this downgrades
/// *permanent* retention to *normal* retention, it does not remove retention.
///
/// **Implicit prerequisite**: `panic = "abort"` would skip `Drop` entirely and
/// this guard with it. The workspace uses the default unwind profile; nothing
/// pins that, so it is written down here.
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

/// #1013 PR-B: the **only** way to declare a pty leader's pin lost — the
/// per-entry flag and the registry counter are written here together, never
/// apart.
///
/// It is a function rather than two inline statements because the two waiter
/// arms that reach it (`ECHILD` and `Unexpected`) were written independently
/// and drifted: the `Unexpected` arm set the flag and skipped the counter, so
/// an entry could report `stats.pin_lost == true` while `debug_pin_lost_count()
/// still read 0. T6 asserts the two together and would not have noticed,
/// because it only ever exercises the `ECHILD` arm. Structural pairing is the
/// same remedy design M3 applies to `pin_count`.
///
/// Both arms are deliberately included: `Unexpected` has not *proven* the pin
/// lost (the leader may well still be our unreaped child), but the waiter has
/// lost its ability to say either way, and every downstream reader of this flag
/// is fail-safe in the "refuse to use the pgid" direction. The cost of that
/// conservatism on the `Unexpected` arm is recorded in §6.4.
fn mark_pin_lost(entry: &ProcEntry, pin_lost_count: &AtomicUsize) {
    if let ProcRuntime::Pty { pin_lost, .. } = &entry.runtime {
        pin_lost.store(true, Ordering::SeqCst);
    }
    pin_lost_count.fetch_add(1, Ordering::SeqCst);
}

/// The grace is *not* load-bearing for ordering; shortening or lengthening it
/// only changes how much genuinely in-flight output the degraded path is
/// willing to wait for.
fn spawn_pty_waiter(
    proc_id: String,
    entry: Arc<ProcEntry>,
    pid: u32,
    gate: Arc<PtyDrainGate>,
    reclaim_grace: Duration,
    drain_grace: Duration,
    pin_lost_count: Arc<AtomicUsize>,
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
        // #1013 PR-B (design M2): from here on, every exit path from this
        // thread must either run to `disarm()` or leave the entry in a state
        // where it can still be reclaimed — otherwise the leader zombie is
        // pinned until the supervisor exits. Constructed *before* the
        // observation, so a panic anywhere in this body is covered.
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
        // Published before the grace window so liveness probes stop reporting
        // a child that has exited as running (issue #993 R4). The name says
        // *observed*, not *reaped*, because under `WNOWAIT` those are two
        // different instants — the reap is `Drop for ProcEntry`'s.
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

        // #996: 只登记一个到期时刻，然后线程就结束 —— 不睡、不定时器、不每条
        // 退出记录起一根线程。真正的移除由 serve 循环里那一个周期性清扫器做
        // （`ProcRegistry::sweep_expired_entries`），且要额外等 reader 结束。
        //
        // 严格排在上面 seal 临界区之后：登记不能挤进 seal / sticky / broadcast
        // 之间，否则 `Exited` 不再是最后一帧（#993）。
        //
        // `checked_add`：`Instant + Duration` 溢出会 panic，而这是 waiter 线程的
        // 最后一步 —— panic 掉就再没人登记到期时刻，entry 永久留在 registry。
        // 溢出只可能来自一个荒谬大的宽限期，此时"实际上永不回收"就是它要的语义，
        // 于是退化成不登记，并留一条 WARN 而不是静默。
        let now = std::time::Instant::now();
        match now.checked_add(reclaim_grace) {
            Some(at) => entry.schedule_removal(at),
            None => tracing::warn!(
                proc_id = %proc_id,
                grace_secs = reclaim_grace.as_secs(),
                "pty reclaim grace overflows Instant; entry will not be scheduled for removal"
            ),
        }
        // Ran the whole way through: the entry has a sticky exit and a
        // scheduled removal, so it will become reclaimable and its pinned
        // leader will be reaped by `Drop for ProcEntry`. Only now disarm.
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
            // #996: 与 Pipe 分支不同，这里**故意不**就地移除已退出的 entry ——
            // 那会在宽限期内把 sticky exit 与最后一屏 replay 一起丢掉。移除是
            // 清扫器的事（`sweep_expired_entries`）；同名 proc_id 在宽限期内被
            // 重新拉起时，`try_spawn_pty` 的 insert 直接覆盖旧 entry，旧 entry
            // 的 `remove_after` 随之一起消失，不会污染任何淘汰顺序。
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

        /// 测试专用：拉长 "reap → sticky exit" 之间的排空窗口
        /// （`PTY_DRAIN_GRACE`，生产 50ms），从而确定性地站在窗口内做断言。
        pub async fn start_with_drain_grace(pty_drain_grace: Duration) -> anyhow::Result<Self> {
            Self::start_with_registry(
                ProcRegistry::without_reaper().with_pty_drain_grace(pty_drain_grace),
            )
            .await
        }

        /// #996: 让测试可以缩短"退出 → 整条移除"的宽限期。
        pub async fn start_with_grace(pty_reclaim_grace: Duration) -> anyhow::Result<Self> {
            Self::start_with_registry(
                ProcRegistry::without_reaper().with_pty_reclaim_grace(pty_reclaim_grace),
            )
            .await
        }

        async fn start_with_registry(registry: ProcRegistry) -> anyhow::Result<Self> {
            // #1439: 控制 socket 的目录钉在短基址上 —— `$TMPDIR`
            // 在自托管 runner 上有 49 字节，会把 `sun_path` 吃掉一半。
            let temp = calm_test_sockets::try_socket_dir("ps")?;
            let sock = temp.path().join("proc-supervisor.sock");
            calm_test_sockets::assert_fits(&sock);
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

        /// #996: 暴露 registry 供泄漏断言（entry 数 / ring 字节 / pty fd）。
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

    /// T3 — the siginfo → `(status, signalled)` table, asserted on **the
    /// production function the waiter actually calls**.
    ///
    /// The earlier shape of this test asserted on a `raw_wait_status` helper and
    /// re-composed `from_raw` → `portable_pty::ExitStatus` → `(status,
    /// signalled)` itself. That proves the helper, not the wiring: reverting the
    /// waiter to an inline `si_code` match leaves such a test green while the
    /// `CLD_DUMPED` regression is back. So the whole chain is one function, the
    /// waiter is its only caller, and this case asserts the function's output.
    ///
    /// Hermetic: no process, no fixture, no environment dependency. That is why
    /// this — not the integration case in
    /// `exit_status_survives_the_wnowait_split.rs` — is the load-bearing lock
    /// for the core-dump row.
    #[test]
    fn proc_exit_parts_from_siginfo_maps_every_wexited_code() {
        // The triples are measured, not assumed: `exit 42`, `SIGKILL` and
        // `abort` under `ulimit -c unlimited` produce exactly these on
        // Linux 6.1 / glibc.
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
        // The row that pays for this test. A core dump has WIFSIGNALED = 1, so
        // it must land in the signalled column. A CLD_KILLED-only match
        // synthesises a normal-exit status word instead, and the crash gets
        // persisted as "exited with code 6".
        assert_eq!(
            proc_exit_parts_from_siginfo(libc::CLD_DUMPED, libc::SIGABRT),
            (None, true),
            "a core-dumping child is signalled, not an exit with code 6"
        );
        // `WEXITED` alone cannot report these; if one shows up, our reading of
        // the call is wrong and the function must degrade loudly rather than
        // synthesise a status word from a code it does not understand.
        for code in [libc::CLD_STOPPED, libc::CLD_CONTINUED, libc::CLD_TRAPPED] {
            assert_eq!(
                proc_exit_parts_from_siginfo(code, 19),
                DEGRADED_EXIT_PARTS,
                "si_code {code} is not reachable under WEXITED and must degrade"
            );
        }
    }

    /// T6b — `group_target` refuses to hand out the leader pgid once the pin is
    /// proven lost, and still hands it out otherwise.
    ///
    /// The companion to `pin_lost_on_autoreap.rs`: that one proves the
    /// *production wiring* sets `pin_lost` (no fixture re-implementing the
    /// check), this one proves the decision itself and does not depend on the
    /// environment's `SIGCHLD` disposition.
    ///
    /// Note the positive assertion discriminates the **variant** and never
    /// reads the number — outside `pgid_lease` the number is `E0616`, and
    /// `matches!` is exactly the capability the T11 gate must not have removed.
    #[tokio::test]
    async fn group_target_refuses_the_leader_target_after_pin_loss() {
        // A pty pair whose leader has already exited; the entry is the minimal
        // shape `group_target` needs. Every Pty entry uses the `Leader` branch.
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
                // Already reaped above, deliberately: this case is about
                // `group_target`'s decision, and leaving a handle here would
                // make `Drop for ProcEntry` reap a pid that is already gone.
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

    /// A pty entry with no leader handle installed, for the two cases below.
    ///
    /// No child is spawned: `leader: None` means `Drop for ProcEntry` takes
    /// `None` and reaps nothing, so neither case depends on a process, a pid,
    /// or the environment's `SIGCHLD` disposition. Only `openpty` is needed —
    /// `ProcRuntime::Pty` owns a real master, and `seal_and_publish_exit` /
    /// `WaiterCompletion` never touch it.
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

    /// `seal_and_publish_exit` has **two** callers — the waiter's normal path
    /// and `WaiterCompletion::drop` — so "stamp only if absent" is the entire
    /// reason `Exited` is still provably-once after PR-B. §6.4 previously
    /// claimed this branch was uncovered by construction; it is not, and
    /// removing the guard is a one-line mutation that the rest of the suite
    /// (`--lib` plus all the integration cases) does not notice.
    ///
    /// Mutation: replace the stamp-if-absent `match` with an unconditional
    /// `*slot = Some(exit.clone()); None` → both the return-value assertion and
    /// the "no second frame" assertion go red. That mutant is exactly the #993
    /// duplicate-terminal-frame shape coming back.
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

    /// The `WaiterCompletion` guard (design M2), asserted on the guard itself
    /// rather than on the argument for it. §6.4 previously claimed this path
    /// had no injection point in production code; it does not need one — the
    /// guard is a plain RAII type and a panicking thread is a legitimate
    /// fixture for "the waiter did not run to completion".
    ///
    /// All four post-conditions are load-bearing and each is named in its own
    /// assertion, because a guard that sets only some of them still leaves the
    /// leader pinned forever.
    ///
    /// Mutation: no-op the body of `Drop for WaiterCompletion` → red on the
    /// first of the four.
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

    /// Two properties in one case, both load-bearing for #1013 PR-A:
    ///
    /// 1. **`group_target` never returns `Err` for a Pipe entry**
    ///    (`pgid_lease` regulation 4). If it did,
    ///    `terminate_all_process_groups_sync`'s `filter_map(...ok())` would
    ///    silently drop every Pipe entry from the shutdown path and #388 would
    ///    break. **This case is that rule's primary gate.**
    ///    `server_restart_survives` is not: on this violation it *hangs*
    ///    rather than fails (the supervisor's `#[tokio::main]` runtime drop
    ///    blocks on the `reap_children` `waitpid`, so it cannot exit before the
    ///    pipe child's own 30s self-exit; measured 0.23s → 30.03s, still "ok").
    ///    That test now carries an elapsed assertion, making it a secondary
    ///    gate, but this `--lib` case is the one that goes red deterministically.
    /// 2. **The parent module can still discriminate the target's *kind*
    ///    without reading its number.** `matches!(t, GroupSignalTarget::Leader(..))`
    ///    must keep compiling here: `require_addressable_by_signal_rpc` and
    ///    T9b's shape both depend on it, and it is the property that T11's
    ///    `E0616` gate must *not* have taken away.
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

    /// The stable kind/message distinctions the error table pins down. The
    /// `PinLost` vs `Kill` pair matters most: PR-B's T6 can only be reddened by
    /// its own mutation if those two are distinguishable.
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
                    // Note what the `Kill` prefix pins: the pre-#1013 message
                    // was `signal proc {id} pgid {decimal}: {e}` — a textual
                    // `i32` accessor. The asserted prefix now ends at
                    // `(Leader target):`, so a decimal pgid cannot reappear
                    // before the io::Error without reddening this case.
                }
                other => panic!("expected ControlReply::Error, got {other:?}"),
            }
        }
    }
}
