//! Boot-only pre-C4 upgrade cleanup. Never used by runtime provider or control paths.
use std::collections::HashSet;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicU8, AtomicUsize, Ordering};
use std::time::Duration;

use tokio::time::Instant;

use sha2::{Digest, Sha256};

use crate::error::{CalmError, Result};
use crate::proc_identity::{parse_proc_stat_fields, proc_entry_vanished, read_proc_start_time};

const TERM_GRACE: Duration = Duration::from_secs(5);
const KILL_WAIT: Duration = Duration::from_secs(10);
/// The boot upgrade sweep gives up this long after it starts.
const STOP_BOUND: Duration = Duration::from_secs(15);
/// Under a tight deadline the SIGTERM grace ends this long before it, so SIGKILL still goes out.
const KILL_RESERVE: Duration = Duration::from_secs(1);
const POLL: Duration = Duration::from_millis(50);

/// The calm instance a marker belongs to: a short hash of the canonical `data_dir`, so another
/// calm-server on the host never matches this one's processes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MarkerInstance {
    value: String,
    key: &'static str,
}

impl MarkerInstance {
    pub fn for_data_dir(data_dir: &Path, key: &'static str) -> Result<Self> {
        let canonical = data_dir.canonicalize()?;
        let digest = Sha256::digest(canonical.as_os_str().as_encoded_bytes());
        Ok(Self {
            value: hex::encode(&digest[..6]),
            key,
        })
    }

    /// The marker value for one worker session.
    pub fn marker(&self, worker_session_id: &str) -> String {
        format!("{}:{worker_session_id}", self.value)
    }
}

/// Sweep pre-C4 native processes once at boot, bounded by the shared scan deadline.
pub(crate) async fn sweep_pre_c4(
    instance: &MarkerInstance,
    worker_session_ids: &[&str],
) -> Result<()> {
    let until = Instant::now() + STOP_BOUND;
    let markers: Arc<HashSet<String>> = Arc::new(
        worker_session_ids
            .iter()
            .map(|id| format!("{}={}", instance.key, instance.marker(id)))
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
        "planner stop: marked processes outlived SIGTERM; sending SIGKILL"
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
        "planner processes still alive when the stop gave up: {left:?}"
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
/// hung, the other provider's boot scan fails closed at once instead of leaking another blocking
/// thread. Wrapping arithmetic: the abandoned
/// scan's own decrement may land before the abandoning increment.
static HUNG_SCANS: AtomicUsize = AtomicUsize::new(0);
const SCAN_RUNNING: u8 = 0;
const SCAN_DONE: u8 = 1;
const SCAN_ABANDONED: u8 = 2;

/// A scan reads every process's `stat` and `environ` on the blocking pool, bounded by `until`. A
/// same-user process can make an `environ` read block (its mm lock held); past `until` the scan is
/// an `Err` (fail closed) and its blocking thread is leaked until the read returns; until then no
/// other scan starts (see [`HUNG_SCANS`]).
async fn scan_off_thread(
    proc_root: &Path,
    markers: &Arc<HashSet<String>>,
    until: Instant,
) -> Result<Vec<Member>> {
    if HUNG_SCANS.load(Ordering::SeqCst) != 0 {
        return Err(CalmError::Conflict(
            "planner stop: an earlier /proc scan is still hung".into(),
        ));
    }
    let markers = Arc::clone(markers);
    let proc_root = proc_root.to_path_buf();
    let phase = Arc::new(AtomicU8::new(SCAN_RUNNING));
    let scan_phase = Arc::clone(&phase);
    let scan = tokio::task::spawn_blocking(move || {
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
        Ok(joined) => {
            joined.map_err(|error| CalmError::Internal(format!("planner stop scan: {error}")))?
        }
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
                "planner stop: a /proc scan did not finish in time".into(),
            ))
        }
    }
}

/// A process carrying one of the markers, with the `start_time` read BEFORE its environ: if the
/// pid changed hands in between, the signal-time check rejects it and the next scan re-reads it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Member {
    pid: i32,
    start_time: u64,
}

/// One pass over `proc_root`. A listing that cannot be read is an `Err`, never an empty (and so
/// "stopped") scan. Per pid: a process that vanished mid-scan (`ENOENT`/`ESRCH`) is skipped; one
/// whose `stat` cannot be read or parsed for another reason is an `Err` only when its environ
/// carries one of the markers, because a member without a `start_time` can be neither signalled
/// nor proven gone, while a non-member is not this sweep's business.
fn scan_in(proc_root: &Path, markers: &HashSet<String>) -> Result<Vec<Member>> {
    let entries = std::fs::read_dir(proc_root).map_err(|error| {
        CalmError::Conflict(format!(
            "planner stop cannot list {}: {error}",
            proc_root.display()
        ))
    })?;
    let self_pid = std::process::id() as i32;
    let mut members = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|error| {
            CalmError::Conflict(format!(
                "planner stop cannot list {}: {error}",
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
                    "planner process {pid} carries the marker but its stat is {reason}"
                )));
            }
            Err(_) => {}
        }
    }
    Ok(members)
}

/// Readable environ with an exact `key=value` environment entry for one of `markers`. An
/// unreadable environ (EACCES, a vanished process) and a zombie's empty environ are not members.
fn carries_marker(proc_dir: &Path, markers: &HashSet<String>) -> bool {
    let Ok(environ) = std::fs::read(proc_dir.join("environ")) else {
        return false;
    };
    environ.split(|&byte| byte == 0).any(|entry| {
        std::str::from_utf8(entry)
            .ok()
            .is_some_and(|entry| markers.contains(entry))
    })
}

/// Signal each member whose live `start_time` still equals the scanned one; returns the signalled
/// pids. The check-then-kill window is the same accepted epsilon as `sigkill_verified_members`.
fn signal_verified(members: &[Member], signal: libc::c_int) -> Vec<i32> {
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

/// Fixtures only: test cleanup kills through the same `start_time`-verified signal as the boot sweep, so a
/// cleanup can never hit a pid that was recycled after the test captured it. `true` when signalled.
#[cfg(feature = "fixtures")]
pub fn sigkill_verified_for_test(pid: i32, start_time: u64) -> bool {
    !signal_verified(&[Member { pid, start_time }], libc::SIGKILL).is_empty()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pre_c4_marker_match_is_an_exact_environment_entry() {
        let root = tempfile::tempdir().unwrap();
        let markers = HashSet::from(["KEY=instance:session".to_owned()]);
        for (entry, expected) in [
            ("KEY=instance:session", true),
            ("KEY=instance:session-extra", false),
            ("OTHER_KEY=instance:session", false),
            ("PREFIX=KEY=instance:session", false),
        ] {
            std::fs::write(root.path().join("environ"), format!("OTHER=x\0{entry}\0")).unwrap();
            assert_eq!(carries_marker(root.path(), &markers), expected, "{entry}");
        }
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn pre_c4_recycled_pid_identity_is_not_signalled() {
        let child = std::process::Command::new("/bin/sleep")
            .arg("30")
            .spawn()
            .unwrap();
        let mut child = child;
        let pid = child.id() as i32;
        let start_time = read_proc_start_time(pid).unwrap();
        let signalled = signal_verified(
            &[Member {
                pid,
                start_time: start_time + 1,
            }],
            libc::SIGKILL,
        );
        let still_alive = child.try_wait().unwrap().is_none();
        child.kill().unwrap();
        child.wait().unwrap();
        assert!(signalled.is_empty());
        assert!(still_alive);
    }

    #[tokio::test]
    async fn pre_c4_unlistable_proc_fails_closed() {
        let root = tempfile::tempdir().unwrap();
        let markers = Arc::new(HashSet::from(["KEY=instance:session".into()]));
        let error = scan_off_thread(
            &root.path().join("missing"),
            &markers,
            Instant::now() + Duration::from_secs(1),
        )
        .await
        .unwrap_err();
        assert!(error.to_string().contains("cannot list"), "{error}");
    }

    #[tokio::test]
    async fn pre_c4_scan_deadline_fails_closed() {
        use std::os::unix::ffi::OsStrExt;
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join((std::process::id() + 1).to_string());
        std::fs::create_dir(&dir).unwrap();
        std::fs::write(dir.join("stat"), "invalid stat").unwrap();
        // A marked process with unreadable stat still reads environ to determine membership.
        let fifo = dir.join("environ");
        let path = std::ffi::CString::new(fifo.as_os_str().as_bytes()).unwrap();
        // SAFETY: a valid NUL-terminated path inside the private test directory.
        assert_eq!(unsafe { libc::mkfifo(path.as_ptr(), 0o600) }, 0);
        let (release, wait) = std::sync::mpsc::channel();
        let writer = std::thread::spawn(move || {
            use std::os::unix::fs::OpenOptionsExt;
            let deadline = std::time::Instant::now() + Duration::from_secs(2);
            loop {
                match std::fs::OpenOptions::new()
                    .write(true)
                    .custom_flags(libc::O_NONBLOCK)
                    .open(&fifo)
                {
                    Ok(writer) => {
                        let _ = wait.recv_timeout(Duration::from_secs(2));
                        drop(writer);
                        break;
                    }
                    Err(error) if error.raw_os_error() == Some(libc::ENXIO) => {
                        assert!(std::time::Instant::now() < deadline);
                        std::thread::sleep(Duration::from_millis(1));
                    }
                    Err(error) => panic!("{error}"),
                }
            }
        });
        let markers = Arc::new(HashSet::from(["KEY=instance:session".into()]));
        let result = scan_off_thread(
            root.path(),
            &markers,
            Instant::now() + Duration::from_millis(50),
        )
        .await;
        // Unblock the abandoned scanner before asserting (also under a broken timeout mutation).
        let _ = release.send(());
        writer.join().unwrap();
        let error = result.unwrap_err();
        assert!(
            error.to_string().contains("did not finish in time"),
            "{error}"
        );
    }
}
