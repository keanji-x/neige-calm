//! The one stop primitive for Claude Planner processes (design #1791 §5.1, D12, D16, D23, D26).
//!
//! Every process neige starts for a Claude Planner carries the environ marker
//! `NEIGE_CLAUDE_PLANNER=<instance>:<worker_session_id>`, and every descendant inherits it, including
//! the Bash commands the CLI runs in their own sessions (P-K). A stop is one `/proc` pass: a process
//! is a member only when its environ is readable and carries one of the exact markers asked for;
//! an unreadable or foreign environ is never signalled nor waited on. Members get SIGTERM, then after
//! a grace period a fresh scan's members get SIGKILL, and the stop succeeds only once a scan finds
//! no member. Every signal is preceded by a `start_time` check, so a recycled pid is never hit.

use std::collections::HashSet;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicU8, AtomicUsize, Ordering};
use std::time::Duration;

use tokio::time::Instant;

use sha2::{Digest, Sha256};

use crate::error::{CalmError, Result};
use crate::proc_identity::{parse_proc_stat_fields, read_proc_start_time};
use calm_worker_runtime::proc_entry_vanished;

/// The environ key every Claude Planner process carries.
pub const MARKER_KEY: &str = "NEIGE_CLAUDE_PLANNER";

const TERM_GRACE: Duration = Duration::from_secs(5);
const KILL_WAIT: Duration = Duration::from_secs(10);
/// A stop given no tighter deadline gives up this long after it starts.
pub const STOP_BOUND: Duration = Duration::from_secs(15);
/// Under a tight deadline the SIGTERM grace ends this long before it, so SIGKILL still goes out.
const KILL_RESERVE: Duration = Duration::from_secs(1);
const POLL: Duration = Duration::from_millis(50);

/// The calm instance a marker belongs to: a short hash of the canonical `data_dir`, so another
/// calm-server on the host never matches this one's processes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MarkerInstance(String);

impl MarkerInstance {
    pub fn for_data_dir(data_dir: &Path) -> Result<Self> {
        let canonical = data_dir.canonicalize()?;
        let digest = Sha256::digest(canonical.as_os_str().as_encoded_bytes());
        Ok(Self(hex::encode(&digest[..6])))
    }

    /// The marker value for one worker session.
    pub fn marker(&self, worker_session_id: &str) -> String {
        format!("{}:{worker_session_id}", self.0)
    }
}

/// Whether a sweep honours the fixtures-only stop-failure seam. The boot sweep never does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SeamPolicy {
    Consult,
    Ignore,
}

/// Stop every process carrying this worker session's marker. `Ok` means a scan found none left.
pub async fn stop(instance: &MarkerInstance, worker_session_id: &str) -> Result<()> {
    stop_by(instance, worker_session_id, Instant::now() + STOP_BOUND).await
}

/// [`stop`] that gives up at `until`: `Err` unless a scan found no member by then.
pub async fn stop_by(
    instance: &MarkerInstance,
    worker_session_id: &str,
    until: Instant,
) -> Result<()> {
    sweep_by(instance, &[worker_session_id], SeamPolicy::Consult, until).await
}

/// One sweep over the markers of a set of worker sessions, one `/proc` pass per scan.
pub async fn sweep(
    instance: &MarkerInstance,
    worker_session_ids: &[&str],
    seam: SeamPolicy,
) -> Result<()> {
    sweep_by(
        instance,
        worker_session_ids,
        seam,
        Instant::now() + STOP_BOUND,
    )
    .await
}

/// SIGTERM → grace (at most [`TERM_GRACE`], ending [`KILL_RESERVE`] before `until`) → rescan and
/// SIGKILL → wait until no member, all by `until`; every scan is bounded by `until` too.
pub async fn sweep_by(
    instance: &MarkerInstance,
    worker_session_ids: &[&str],
    seam: SeamPolicy,
    until: Instant,
) -> Result<()> {
    if seam == SeamPolicy::Consult {
        seam::check(worker_session_ids)?;
    }
    let markers: Arc<HashSet<String>> = Arc::new(
        worker_session_ids
            .iter()
            .map(|id| instance.marker(id))
            .collect(),
    );
    if markers.is_empty() {
        return Ok(());
    }
    let proc_root = Path::new("/proc");
    signal_verified(
        &scan_off_thread(proc_root, &markers, until).await?,
        libc::SIGTERM,
    );
    let term_until = (Instant::now() + TERM_GRACE).min(
        until
            .checked_sub(KILL_RESERVE)
            .unwrap_or(until)
            .max(Instant::now()),
    );
    if wait_empty(&markers, term_until, until).await? {
        return Ok(());
    }
    let survivors = scan_off_thread(proc_root, &markers, until).await?;
    tracing::warn!(
        pids = ?survivors.iter().map(|member| member.pid).collect::<Vec<_>>(),
        "claude planner stop: marked processes outlived SIGTERM; sending SIGKILL"
    );
    signal_verified(&survivors, libc::SIGKILL);
    if wait_empty(&markers, until.min(Instant::now() + KILL_WAIT), until).await? {
        return Ok(());
    }
    let left: Vec<i32> = scan_off_thread(proc_root, &markers, until)
        .await?
        .iter()
        .map(|member| member.pid)
        .collect();
    Err(CalmError::Conflict(format!(
        "claude planner processes still alive when the stop gave up: {left:?}"
    )))
}

/// Poll until a scan finds no member (`true`) or `phase_until` passes (`false`).
async fn wait_empty(
    markers: &Arc<HashSet<String>>,
    phase_until: Instant,
    until: Instant,
) -> Result<bool> {
    loop {
        if scan_off_thread(Path::new("/proc"), markers, until)
            .await?
            .is_empty()
        {
            return Ok(true);
        }
        if Instant::now() >= phase_until {
            return Ok(false);
        }
        tokio::time::sleep(POLL).await;
    }
}

/// Scans abandoned at their deadline whose blocking thread has not returned yet. While one is
/// hung, every new scan fails closed at once instead of leaking one more blocking thread per stop
/// attempt (a `turn_start` retry stops every few seconds). Wrapping arithmetic: the abandoned
/// scan's own decrement may land before the abandoning increment.
static HUNG_SCANS: AtomicUsize = AtomicUsize::new(0);
const SCAN_RUNNING: u8 = 0;
const SCAN_DONE: u8 = 1;
const SCAN_ABANDONED: u8 = 2;

/// A scan reads every process's `stat` and `environ` on the blocking pool, bounded by `until`. A
/// same-user process can make an `environ` read block (its mm lock held); past `until` the scan is
/// an `Err` (fail closed) and its blocking thread is leaked until the read returns; until then no
/// other scan starts (see [`HUNG_SCANS`]).
pub(crate) async fn scan_off_thread(
    proc_root: &Path,
    markers: &Arc<HashSet<String>>,
    until: Instant,
) -> Result<Vec<Member>> {
    if HUNG_SCANS.load(Ordering::SeqCst) != 0 {
        return Err(CalmError::Conflict(
            "claude planner stop: an earlier /proc scan is still hung".into(),
        ));
    }
    let markers = Arc::clone(markers);
    let proc_root = proc_root.to_path_buf();
    let phase = Arc::new(AtomicU8::new(SCAN_RUNNING));
    let scan_phase = Arc::clone(&phase);
    let scan = tokio::task::spawn_blocking(move || {
        #[cfg(feature = "fixtures")]
        seam::wait_if_scan_held();
        let members = scan_in(&proc_root, &markers);
        if scan_phase
            .compare_exchange(SCAN_RUNNING, SCAN_DONE, Ordering::SeqCst, Ordering::SeqCst)
            .is_err()
        {
            HUNG_SCANS.fetch_sub(1, Ordering::SeqCst);
        }
        members
    });
    match tokio::time::timeout_at(until, scan).await {
        Ok(joined) => joined
            .map_err(|error| CalmError::Internal(format!("claude planner stop scan: {error}")))?,
        Err(_) => {
            if phase
                .compare_exchange(
                    SCAN_RUNNING,
                    SCAN_ABANDONED,
                    Ordering::SeqCst,
                    Ordering::SeqCst,
                )
                .is_ok()
            {
                HUNG_SCANS.fetch_add(1, Ordering::SeqCst);
            }
            Err(CalmError::Conflict(
                "claude planner stop: a /proc scan did not finish in time".into(),
            ))
        }
    }
}

/// A process carrying one of the markers, with the `start_time` read BEFORE its environ: if the
/// pid changed hands in between, the signal-time check rejects it and the next scan re-reads it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Member {
    pub(crate) pid: i32,
    pub(crate) start_time: u64,
}

/// One pass over `proc_root`. A listing that cannot be read is an `Err`, never an empty (and so
/// "stopped") scan. Per pid: a process that vanished mid-scan (`ENOENT`/`ESRCH`) is skipped; one
/// whose `stat` cannot be read or parsed for another reason is an `Err` only when its environ
/// carries one of the markers, because a member without a `start_time` can be neither signalled
/// nor proven gone, while a non-member is not this sweep's business.
pub(crate) fn scan_in(proc_root: &Path, markers: &HashSet<String>) -> Result<Vec<Member>> {
    let entries = std::fs::read_dir(proc_root).map_err(|error| {
        CalmError::Conflict(format!(
            "claude planner stop cannot list {}: {error}",
            proc_root.display()
        ))
    })?;
    let self_pid = std::process::id() as i32;
    let mut members = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|error| {
            CalmError::Conflict(format!(
                "claude planner stop cannot list {}: {error}",
                proc_root.display()
            ))
        })?;
        let Some(pid) = entry
            .file_name()
            .to_str()
            .and_then(|name| name.parse::<i32>().ok())
        else {
            continue;
        };
        if pid <= 1 || pid == self_pid {
            continue;
        }
        let dir = entry.path();
        let fields = match std::fs::read_to_string(dir.join("stat")) {
            Ok(stat) => parse_proc_stat_fields(&stat).ok_or_else(|| "unparseable".to_string()),
            Err(error) if proc_entry_vanished(&error) => continue,
            Err(error) => Err(error.to_string()),
        };
        match fields {
            Ok(fields) if carries_marker(&dir, markers) => members.push(Member {
                pid,
                start_time: fields.start_time,
            }),
            Ok(_) => {}
            Err(reason) if carries_marker(&dir, markers) => {
                return Err(CalmError::Conflict(format!(
                    "claude planner process {pid} carries the marker but its stat is {reason}"
                )));
            }
            Err(_) => {}
        }
    }
    Ok(members)
}

/// Readable environ with an exact `NEIGE_CLAUDE_PLANNER=<marker>` entry for one of `markers`. An
/// unreadable environ (EACCES, a vanished process) and a zombie's empty environ are not members.
fn carries_marker(proc_dir: &Path, markers: &HashSet<String>) -> bool {
    let Ok(environ) = std::fs::read(proc_dir.join("environ")) else {
        return false;
    };
    let prefix = format!("{MARKER_KEY}=");
    environ.split(|&byte| byte == 0).any(|entry| {
        entry
            .strip_prefix(prefix.as_bytes())
            .and_then(|value| std::str::from_utf8(value).ok())
            .is_some_and(|value| markers.contains(value))
    })
}

/// Signal each member whose live `start_time` still equals the scanned one; returns the signalled
/// pids. The check-then-kill window is the same accepted epsilon as `sigkill_verified_members`.
pub(crate) fn signal_verified(members: &[Member], signal: libc::c_int) -> Vec<i32> {
    let self_pid = std::process::id() as i32;
    let mut signalled = Vec::new();
    for member in members {
        if member.pid <= 1 || member.pid == self_pid {
            continue;
        }
        if read_proc_start_time(member.pid) != Some(member.start_time) {
            continue;
        }
        // SAFETY: kill(2) on a positive pid whose identity was re-verified just above.
        if unsafe { libc::kill(member.pid, signal) } == 0 {
            signalled.push(member.pid);
        }
    }
    signalled
}

#[cfg(feature = "fixtures")]
mod seam {
    use std::collections::HashSet;
    use std::sync::{LazyLock, Mutex};

    use crate::error::{CalmError, Result};

    static FAILING: LazyLock<Mutex<HashSet<String>>> = LazyLock::new(Default::default);

    pub(super) fn check(worker_session_ids: &[&str]) -> Result<()> {
        let failing = FAILING.lock().expect("claude planner stop seam poisoned");
        match worker_session_ids.iter().find(|id| failing.contains(**id)) {
            Some(id) => Err(CalmError::Conflict(format!(
                "claude planner stop for {id} failed (fixture)"
            ))),
            None => Ok(()),
        }
    }

    pub(super) fn arm(worker_session_id: &str) {
        FAILING
            .lock()
            .expect("claude planner stop seam poisoned")
            .insert(worker_session_id.to_string());
    }

    pub(super) fn clear(worker_session_id: &str) {
        FAILING
            .lock()
            .expect("claude planner stop seam poisoned")
            .remove(worker_session_id);
    }

    /// While held, every `/proc` scan blocks on its blocking thread, as a scan stuck on an
    /// `environ` read does.
    static SCAN_HELD: LazyLock<(Mutex<bool>, std::sync::Condvar)> = LazyLock::new(Default::default);

    pub(super) fn hold_scans(held: bool) {
        let (lock, released) = &*SCAN_HELD;
        *lock.lock().expect("scan hold poisoned") = held;
        released.notify_all();
    }

    pub(super) fn wait_if_scan_held() {
        let (lock, released) = &*SCAN_HELD;
        let mut held = lock.lock().expect("scan hold poisoned");
        while *held {
            held = released.wait(held).expect("scan hold poisoned");
        }
    }
}

#[cfg(not(feature = "fixtures"))]
mod seam {
    pub(super) fn check(_worker_session_ids: &[&str]) -> crate::error::Result<()> {
        Ok(())
    }
}

/// Fixtures only: every later `stop` or consulting sweep that covers `worker_session_id` returns
/// `Err` without signalling anything, until [`clear_claude_planner_stop_failure_for_test`].
#[cfg(feature = "fixtures")]
pub fn fail_claude_planner_stop_for_test(worker_session_id: &str) {
    seam::arm(worker_session_id);
}

/// Fixtures only: test cleanup kills through the same `start_time`-verified signal as `stop`, so a
/// cleanup can never hit a pid that was recycled after the test captured it. `true` when signalled.
#[cfg(feature = "fixtures")]
pub fn sigkill_verified_for_test(pid: i32, start_time: u64) -> bool {
    !signal_verified(&[Member { pid, start_time }], libc::SIGKILL).is_empty()
}

#[cfg(feature = "fixtures")]
pub fn clear_claude_planner_stop_failure_for_test(worker_session_id: &str) {
    seam::clear(worker_session_id);
}

/// Fixtures only: `true` makes every later `/proc` scan hang on its blocking thread until `false`.
#[cfg(feature = "fixtures")]
pub fn hold_claude_planner_scans_for_test(held: bool) {
    seam::hold_scans(held);
}

/// Fixtures only: how many abandoned scans are still hung.
#[cfg(feature = "fixtures")]
pub fn hung_claude_planner_scans_for_test() -> usize {
    HUNG_SCANS.load(Ordering::SeqCst)
}
