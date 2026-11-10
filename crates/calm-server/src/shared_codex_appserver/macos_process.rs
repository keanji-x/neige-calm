//! Shared-daemon policy for native recovery. Observations live in plugin::proc_identity;
//! neither a numeric PGID nor a fresh scan after leader exit grants signal authority.
use super::read_boot_id;
use plugin::proc_identity::{MacProcess, observe_macos_group};
use std::{io, time::Duration};
use tokio::process::Child;

// One native fact reader, with an owned-PID fault seam for boundary regressions.
fn read_macos_process(pid: i32) -> io::Result<Option<MacProcess>> {
    #[cfg(test)]
    {
        let mut faults = PROCESS_OBSERVATION_ERRORS.lock().unwrap();
        if let Some((_, remaining)) = faults.iter_mut().find(|(target, _)| *target == pid) {
            *remaining = remaining.saturating_sub(1);
            if *remaining == 0 {
                return Err(io::Error::from_raw_os_error(libc::EACCES));
            }
        }
    }
    plugin::proc_identity::read_macos_process(pid)
}

#[cfg(test)]
static PROCESS_OBSERVATION_ERRORS: std::sync::Mutex<Vec<(i32, usize)>> =
    std::sync::Mutex::new(Vec::new());
#[cfg(test)]
pub(super) struct ProcessObservationError(i32);
#[cfg(test)]
impl ProcessObservationError {
    pub(super) fn inject(pid: i32, read: usize) -> Self {
        let mut faults = PROCESS_OBSERVATION_ERRORS.lock().unwrap();
        assert!(!faults.iter().any(|(target, _)| *target == pid));
        faults.push((pid, read));
        Self(pid)
    }
}
#[cfg(test)]
impl Drop for ProcessObservationError {
    fn drop(&mut self) {
        PROCESS_OBSERVATION_ERRORS
            .lock()
            .unwrap()
            .retain(|(pid, _)| *pid != self.0);
    }
}

pub(super) struct ListenerIdentity {
    pub(super) process: MacProcess,
    pub(super) boot_id: String,
}
impl ListenerIdentity {
    #[cfg(test)]
    pub(super) fn new(pid: i32) -> io::Result<Self> {
        let boot_id =
            read_boot_id().ok_or_else(|| io::Error::other("boot identity unavailable"))?;
        let process = read_macos_process(pid)?
            .filter(|process| !process.is_zombie)
            .ok_or_else(|| io::Error::other("listener identity unavailable"))?;
        Ok(Self { process, boot_id })
    }
}

/// Observation only: failure or any live member fences reuse, including after leader death.
/// The observer certifies all-zombie results around one complete enumeration; do not
/// infer absence from a captured signal list or independently read member deaths.
pub(super) fn group_empty(pgid: i32) -> bool {
    observe_group(pgid).is_ok_and(|members| members.iter().all(|m| m.is_zombie))
}

// Inject a native enumeration error for one owned group, across cleanup and retry.
// The guard keeps concurrently running tests' observations independent.
#[cfg(test)]
static GROUP_OBSERVATION_ERRORS: std::sync::Mutex<Vec<i32>> = std::sync::Mutex::new(Vec::new());

#[cfg(test)]
pub(super) struct GroupObservationError(i32);
#[cfg(test)]
impl GroupObservationError {
    pub(super) fn inject(pgid: i32) -> Self {
        let mut groups = GROUP_OBSERVATION_ERRORS.lock().unwrap();
        assert!(!groups.contains(&pgid));
        groups.push(pgid);
        Self(pgid)
    }
}
#[cfg(test)]
impl Drop for GroupObservationError {
    fn drop(&mut self) {
        GROUP_OBSERVATION_ERRORS
            .lock()
            .unwrap()
            .retain(|pgid| *pgid != self.0);
    }
}

fn observe_group(pgid: i32) -> io::Result<Vec<MacProcess>> {
    #[cfg(test)]
    if GROUP_OBSERVATION_ERRORS.lock().unwrap().contains(&pgid) {
        return Err(io::Error::other(
            "injected native group observation failure",
        ));
    }
    observe_macos_group(pgid)
}

/// Missing PGID means unknown recovery scope, not a PID-only ownership claim.
/// Invalid metadata cannot prove a different boot or a different incarnation.
pub(super) fn persisted_absent(
    pid: i32,
    pgid: Option<i32>,
    start: Option<u64>,
    boot: Option<&str>,
) -> bool {
    let valid_boot = boot.and_then(|boot| uuid::Uuid::parse_str(boot).ok());
    let current_boot = read_boot_id().and_then(|boot| uuid::Uuid::parse_str(&boot).ok());
    if let (Some(saved), Some(current)) = (valid_boot, current_boot)
        && saved != current
    {
        return true;
    }
    let Some(group) = pgid else {
        // A peer PID cannot reconstruct the group it occupied before capture failed.
        return false;
    };
    let verified_start = start
        .filter(|start| *start != 0)
        .filter(|_| valid_boot.is_some() && valid_boot == current_boot);
    let leader_absent = match read_macos_process(pid) {
        Ok(None) => true,
        Ok(Some(process)) => {
            process.is_zombie || verified_start.is_some_and(|start| process.start_time != start)
        }
        Err(_) => false,
    };
    leader_absent && group_empty(group)
}

#[cfg(test)]
#[derive(Clone, Copy, Default)]
pub(super) enum CaptureFault {
    #[default]
    None,
    Boot,
    Process,
}

/// Preserve every readable fact, including terminal identities, before asking whether
/// the observation can authorize a signal. A failed process read leaves scope unknown.
pub(super) fn observe_identity(
    pid: i32,
    #[cfg(test)] fault: CaptureFault,
) -> (Option<MacProcess>, Option<String>) {
    #[cfg(test)]
    let process = if matches!(fault, CaptureFault::Process) {
        None
    } else {
        read_macos_process(pid).ok().flatten()
    };
    #[cfg(not(test))]
    let process = read_macos_process(pid).ok().flatten();
    #[cfg(test)]
    let boot = if matches!(fault, CaptureFault::Boot) {
        None
    } else {
        read_boot_id()
    };
    #[cfg(not(test))]
    let boot = read_boot_id();
    (process, boot)
}

fn signal_member(member: MacProcess, signal: i32) -> io::Result<()> {
    let Some(current) = read_macos_process(member.pid)? else {
        return Ok(());
    };
    // A different birth or a process that actively left the group is outside our authority.
    if current.start_time != member.start_time || current.pgid != member.pgid || current.is_zombie {
        return Ok(());
    }
    if member.pid <= 1 || member.pid == std::process::id() as i32 {
        return Err(io::Error::other("refusing unsafe member PID"));
    }
    // SAFETY: positive PID with birth and membership revalidated immediately before kill.
    // The same bounded verify-then-signal race applies as to other external identity callers.
    if unsafe { libc::kill(member.pid, signal) } != 0 {
        let error = io::Error::last_os_error();
        if error.raw_os_error() != Some(libc::ESRCH) {
            return Err(error);
        }
    }
    Ok(())
}

pub(super) async fn terminate_identity(
    pid: i32,
    pgid: Option<i32>,
    start: u64,
    boot: &str,
    grace: Duration,
) -> io::Result<()> {
    terminate_identity_with_listener(pid, pgid, start, boot, grace, None).await
}

async fn terminate_identity_with_listener(
    pid: i32,
    pgid: Option<i32>,
    start: u64,
    boot: &str,
    grace: Duration,
    listener: Option<MacProcess>,
) -> io::Result<()> {
    if read_boot_id().as_deref() != Some(boot) {
        return Err(io::Error::other("original boot identity unavailable"));
    }
    let leader = read_macos_process(pid)?;
    let Some(leader) = leader.filter(|p| p.start_time == start && !p.is_zombie) else {
        return if persisted_absent(pid, pgid, Some(start), Some(boot)) {
            Ok(())
        } else {
            Err(io::Error::other("leader absent with unresolved group"))
        };
    };
    let members = capture_members(leader, pgid, listener)?;
    terminate_members(leader, pgid, &members, grace).await
}

/// The caller proves authority over this leader. External callers require a live
/// identity; an owned Child may also pin a terminal leader until it is reaped.
fn capture_members(
    leader: MacProcess,
    pgid: Option<i32>,
    listener: Option<MacProcess>,
) -> io::Result<Vec<MacProcess>> {
    let members = match pgid {
        Some(group) if group == leader.pid && leader.pgid == group => observe_group(group)?,
        Some(_) => return Err(io::Error::other("leader group identity mismatch")),
        None => vec![leader],
    };
    // Capture while the original leader still proves authority, then validate it again.
    // Later scans may certify emptiness, but can never add signal targets.
    if read_macos_process(leader.pid)? != Some(leader) || !members.contains(&leader) {
        return Err(io::Error::other("leader changed during member capture"));
    }
    if let Some(listener) = listener
        && (!members.contains(&listener) || read_macos_process(listener.pid)? != Some(listener))
    {
        return Err(io::Error::other("listener changed during member capture"));
    }
    Ok(members)
}

fn owned_members(child: &Child, pgid: i32) -> io::Result<Option<(MacProcess, Vec<MacProcess>)>> {
    // Child::id becomes None after try_wait/wait has reaped it. A numeric PGID
    // cannot recover the pin or authorize a new snapshot in that case.
    let Some(pid) = child.id().and_then(|pid| i32::try_from(pid).ok()) else {
        return Ok(None);
    };
    let leader = read_macos_process(pid)?
        .ok_or_else(|| io::Error::other("owned child identity unavailable"))?;
    if leader.pid != pgid || leader.pgid != pgid {
        return Err(io::Error::other("owned child group identity mismatch"));
    }
    let members = match capture_members(leader, Some(pgid), None) {
        Ok(members) => members,
        Err(error) => {
            // The Child pin still authorizes its own verified incarnation. A
            // failed group capture grants no authority over any other member.
            tracing::warn!(pid, pgid, %error, "owned group capture failed; only the direct child is authorized");
            vec![leader]
        }
    };
    Ok(Some((leader, members)))
}

/// A synchronous TERM belt shares the same capture/recheck policy. It neither
/// escalates nor claims that cleanup or durable settlement has completed.
pub(super) fn term_owned(child: &mut Child, pgid: i32) -> io::Result<()> {
    let result = owned_members(child, pgid).and_then(|captured| {
        if let Some((_, members)) = captured {
            for member in members {
                signal_member(member, libc::SIGTERM)?;
            }
        }
        Ok(())
    });
    // Reap an already terminal direct child without waiting for a live one.
    let reaped = child.try_wait().map(|_| ());
    result.and(reaped)
}

pub(super) async fn terminate_owned(
    child: &mut Child,
    pgid: i32,
    grace: Duration,
) -> io::Result<()> {
    let result = match owned_members(child, pgid) {
        Ok(Some((leader, members))) => terminate_members(leader, Some(pgid), &members, grace).await,
        Ok(None) if group_empty(pgid) => Ok(()),
        Ok(None) => Err(io::Error::other(
            "reaped child with unresolved original group",
        )),
        Err(error) => Err(error),
    };
    // A failed capture can leave a live child. Never blindly await it or add a
    // group KILL to make wait finish. Tokio retains its normal orphan reaper.
    let reaped = child.try_wait().map(|_| ());
    result.and(reaped)
}

async fn terminate_members(
    leader: MacProcess,
    pgid: Option<i32>,
    members: &[MacProcess],
    grace: Duration,
) -> io::Result<()> {
    for member in members {
        signal_member(*member, libc::SIGTERM)?;
    }
    let deadline = tokio::time::Instant::now() + grace;
    loop {
        if read_macos_process(leader.pid)?
            .is_none_or(|p| p.start_time != leader.start_time || p.is_zombie)
        {
            break;
        }
        if tokio::time::Instant::now() >= deadline {
            break;
        }
        tokio::time::sleep(
            Duration::from_millis(10)
                .min(deadline.saturating_duration_since(tokio::time::Instant::now())),
        )
        .await;
    }
    for member in members {
        signal_member(*member, libc::SIGKILL)?;
    }
    let deadline = tokio::time::Instant::now() + Duration::from_millis(500);
    loop {
        let mut settled = true;
        for member in members {
            if read_macos_process(member.pid)?.is_some_and(|p| {
                p.start_time == member.start_time && p.pgid == member.pgid && !p.is_zombie
            }) {
                settled = false;
            }
        }
        if settled && pgid.is_none_or(group_empty) {
            return Ok(());
        }
        if tokio::time::Instant::now() >= deadline {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "original group not proven empty",
            ));
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

pub(super) async fn terminate_listener(
    identity: ListenerIdentity,
    pgid: Option<i32>,
    grace: Duration,
) -> io::Result<()> {
    // A socket peer can be a child of its launcher. Capture the live leader of
    // the peer's observed group before capturing any signal targets. This is
    // listener authority, not permission to repair a corrupt persisted tuple.
    let leader = match pgid {
        Some(group) if group != identity.process.pgid => {
            return Err(io::Error::other("listener group identity mismatch"));
        }
        Some(group) if group != identity.process.pid => read_macos_process(group)?
            .filter(|leader| leader.pgid == group && !leader.is_zombie)
            .ok_or_else(|| io::Error::other("listener group leader unavailable"))?,
        _ => identity.process,
    };
    terminate_identity_with_listener(
        leader.pid,
        pgid,
        leader.start_time,
        &identity.boot_id,
        grace,
        Some(identity.process),
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{os::unix::process::CommandExt, process::Command, time::Instant};

    struct KillGroupOnDrop(MacProcess);
    impl Drop for KillGroupOnDrop {
        fn drop(&mut self) {
            // These tests own the unreaped child. Do not signal its numeric group
            // after wait() released the pin, including on a failed assertion.
            if let Ok(Some(current)) = read_macos_process(self.0.pid)
                && current.start_time == self.0.start_time
                && current.pgid == self.0.pgid
            {
                unsafe { libc::kill(-self.0.pgid, libc::SIGKILL) };
            }
        }
    }

    #[tokio::test]
    async fn terminate_listener_waits_for_grace_before_killing_term_ignoring_process() {
        let mut command = Command::new("/bin/sh");
        command
            .arg("-c")
            .arg("trap '' TERM; while :; do sleep 1; done");
        command.process_group(0);
        let mut child = command.spawn().unwrap();
        let pid = child.id() as i32;
        let _cleanup = KillGroupOnDrop(read_macos_process(pid).unwrap().unwrap());
        let watcher = ListenerIdentity::new(pid).unwrap();
        tokio::time::sleep(Duration::from_millis(50)).await;
        let grace = Duration::from_millis(100);
        let started = Instant::now();
        terminate_listener(watcher, Some(pid), grace).await.unwrap();
        assert!(started.elapsed() >= grace);
        assert!(!child.wait().unwrap().success());
    }

    #[tokio::test]
    async fn terminate_listener_returns_once_a_term_cooperative_listener_exits() {
        let mut command = Command::new("/bin/sh");
        command
            .arg("-c")
            .arg("trap 'exit 0' TERM; while :; do sleep 1; done");
        command.process_group(0);
        let mut child = command.spawn().unwrap();
        let pid = child.id() as i32;
        let _cleanup = KillGroupOnDrop(read_macos_process(pid).unwrap().unwrap());
        let watcher = ListenerIdentity::new(pid).unwrap();
        tokio::time::sleep(Duration::from_millis(50)).await;
        let started = Instant::now();
        terminate_listener(watcher, Some(pid), Duration::from_secs(5))
            .await
            .unwrap();
        assert!(started.elapsed() < Duration::from_secs(2));
        assert!(child.wait().unwrap().success());
    }

    #[tokio::test]
    async fn terminate_listener_observes_an_exit_that_preceded_the_first_signal() {
        let mut command = Command::new("/bin/sh");
        command.arg("-c").arg("exec sleep 60");
        command.process_group(0);
        let mut child = command.spawn().unwrap();
        let pid = child.id() as i32;
        let _cleanup = KillGroupOnDrop(read_macos_process(pid).unwrap().unwrap());
        let watcher = ListenerIdentity::new(pid).unwrap();
        // Reaped before the first signal: verify absence and the empty original group,
        // without using the recycled numeric PID as a signal target.
        // SAFETY: `pid` is this test's own unreaped child.
        assert_eq!(unsafe { libc::kill(pid, libc::SIGKILL) }, 0);
        assert!(!child.wait().unwrap().success());
        let grace = Duration::from_secs(1);
        let started = Instant::now();
        terminate_listener(watcher, Some(pid), grace).await.unwrap();
        assert!(started.elapsed() < grace);
    }

    #[test]
    fn signal_member_rejects_mismatched_birth_and_group() {
        let mut child = Command::new("sleep")
            .arg("60")
            .process_group(0)
            .spawn()
            .unwrap();
        let pid = child.id() as i32;
        let _cleanup = KillGroupOnDrop(read_macos_process(pid).unwrap().unwrap());
        let member = read_macos_process(pid).unwrap().unwrap();
        signal_member(
            MacProcess {
                start_time: member.start_time + 1,
                ..member
            },
            libc::SIGKILL,
        )
        .unwrap();
        std::thread::sleep(Duration::from_millis(20));
        assert!(
            child.try_wait().unwrap().is_none(),
            "mismatched birth must never receive a signal"
        );
        signal_member(
            MacProcess {
                pgid: member.pgid + 1,
                ..member
            },
            libc::SIGKILL,
        )
        .unwrap();
        std::thread::sleep(Duration::from_millis(20));
        assert!(
            child.try_wait().unwrap().is_none(),
            "moved group must never receive a signal"
        );
        child.kill().unwrap();
        child.wait().unwrap();
    }
}
