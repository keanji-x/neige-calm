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
use std::time::Duration;

use sha2::{Digest, Sha256};

use crate::error::{CalmError, Result};
use crate::proc_identity::{parse_proc_stat_fields, read_proc_start_time};

/// The environ key every Claude Planner process carries.
pub const MARKER_KEY: &str = "NEIGE_CLAUDE_PLANNER";

const TERM_GRACE: Duration = Duration::from_secs(5);
const KILL_WAIT: Duration = Duration::from_secs(10);
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
    sweep(instance, &[worker_session_id], SeamPolicy::Consult).await
}

/// One sweep over the markers of a set of worker sessions, one `/proc` pass per scan.
pub async fn sweep(
    instance: &MarkerInstance,
    worker_session_ids: &[&str],
    seam: SeamPolicy,
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
    signal_verified(&scan_off_thread(&markers).await?, libc::SIGTERM);
    if wait_empty(&markers, TERM_GRACE).await? {
        return Ok(());
    }
    let survivors = scan_off_thread(&markers).await?;
    tracing::warn!(
        pids = ?survivors.iter().map(|member| member.pid).collect::<Vec<_>>(),
        "claude planner stop: marked processes outlived SIGTERM; sending SIGKILL"
    );
    signal_verified(&survivors, libc::SIGKILL);
    if wait_empty(&markers, KILL_WAIT).await? {
        return Ok(());
    }
    let left: Vec<i32> = scan_off_thread(&markers)
        .await?
        .iter()
        .map(|member| member.pid)
        .collect();
    Err(CalmError::Conflict(format!(
        "claude planner processes still alive after SIGKILL: {left:?}"
    )))
}

async fn wait_empty(markers: &Arc<HashSet<String>>, within: Duration) -> Result<bool> {
    let deadline = tokio::time::Instant::now() + within;
    loop {
        if scan_off_thread(markers).await?.is_empty() {
            return Ok(true);
        }
        if tokio::time::Instant::now() >= deadline {
            return Ok(false);
        }
        tokio::time::sleep(POLL).await;
    }
}

/// A scan reads every process's `stat` and `environ`; it runs on the blocking pool.
async fn scan_off_thread(markers: &Arc<HashSet<String>>) -> Result<Vec<Member>> {
    let markers = Arc::clone(markers);
    tokio::task::spawn_blocking(move || scan(&markers))
        .await
        .map_err(|error| CalmError::Internal(format!("claude planner stop scan: {error}")))?
}

/// A process carrying one of the markers, with the `start_time` read BEFORE its environ: if the
/// pid changed hands in between, the signal-time check rejects it and the next scan re-reads it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Member {
    pub(crate) pid: i32,
    pub(crate) start_time: u64,
}

pub(crate) fn scan(markers: &HashSet<String>) -> Result<Vec<Member>> {
    scan_in(Path::new("/proc"), markers)
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
            Err(error) if vanished(&error) => continue,
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

fn vanished(error: &std::io::Error) -> bool {
    error.kind() == std::io::ErrorKind::NotFound || error.raw_os_error() == Some(libc::ESRCH)
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

#[cfg(test)]
#[path = "stop_tests.rs"]
mod tests;
