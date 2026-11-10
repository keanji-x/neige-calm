//! Exercise the real termination boundary with only this test's owned children.
use super::*;
use crate::proc_identity::verify_owned_pid;
use tokio::io::{AsyncBufReadExt, BufReader};

struct ProtectedChild {
    child: Child,
    pid: i32,
}

impl ProtectedChild {
    async fn spawn(pgid: i32, ignore_term: bool) -> Self {
        let mut command = tokio::process::Command::new("/bin/sh");
        command
            .arg("-c")
            .arg(if ignore_term {
                "trap '' TERM; printf 'ready\\n'; exec sleep 60"
            } else {
                "printf 'ready\\n'; exec sleep 60"
            })
            .process_group(pgid)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null());
        let child = command.spawn().expect("spawn protected child");
        let pid = child.id().expect("unreaped child pid") as i32;
        let mut guarded = Self { child, pid };
        let mut line = String::new();
        BufReader::new(guarded.child.stdout.take().unwrap())
            .read_line(&mut line)
            .await
            .expect("read readiness");
        assert_eq!(line, "ready\n");
        eprintln!(
            "termination_identity owned pid={pid} pgid={} start={:?}",
            unsafe { libc::getpgid(pid) },
            read_proc_start_time(pid)
        );
        guarded
    }

    async fn assert_alive(&mut self) {
        // Let a mistakenly delivered signal become observable without reimplementing termination.
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert!(
            self.child
                .try_wait()
                .expect("probe protected child")
                .is_none(),
            "termination signaled protected pid {}",
            self.pid
        );
    }
}

impl Drop for ProtectedChild {
    fn drop(&mut self) {
        // Only the owned, unreaped PID; these shells exec sleep and spawn no descendants.
        // Synchronous try_wait also works during panic on a current-thread Tokio runtime.
        self.child
            .start_kill()
            .unwrap_or_else(|error| panic!("CLEANUP FAILED owned pid={}: {error}", self.pid));
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        loop {
            match self.child.try_wait() {
                Ok(Some(status)) => {
                    eprintln!(
                        "termination_identity reaped pid={} status={status}",
                        self.pid
                    );
                    return;
                }
                Ok(None) if std::time::Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(10));
                }
                result => {
                    eprintln!("CLEANUP FAILED pid={}: {result:?}", self.pid);
                    panic!("could not reap protected child");
                }
            }
        }
    }
}

async fn reject_identity(child: &mut ProtectedChild, pid: i32, start: u64, boot: &str) {
    let pgid = unsafe { libc::getpgid(child.pid) };
    assert!(pgid > 1);
    let paths = VerifiedRuntimePaths::for_fixture(Arc::new(BackendPaths::new(
        Path::new("/fixture"),
        &crate::agent_backend_pool::identity::default_id(),
        Path::new("/fixture/home"),
        Path::new("/fixture/logs"),
    )));
    #[cfg(target_os = "macos")]
    assert!(
        !reap_verified_process_group(&paths, pid, pgid, start, boot, Duration::from_millis(100))
            .await
    );
    #[cfg(not(target_os = "macos"))]
    let outcome = terminate_group_with_grace(
        &paths,
        SignalScope::Group { pgid },
        ExitWait::VerifiedIdentity {
            pid,
            start_time: start,
            boot_id: boot,
        },
        Duration::from_millis(100),
    )
    .await;
    child.assert_alive().await;
    #[cfg(not(target_os = "macos"))]
    assert_eq!(
        outcome,
        if boot.is_empty() {
            LeaderExit::Unknown
        } else {
            LeaderExit::FullyGone
        }
    );
}

#[tokio::test]
async fn termination_identity_wrong_start_preserves_group() {
    let boot = read_boot_id().expect("native boot UUID required; no substitute identity");
    let mut child = ProtectedChild::spawn(0, false).await;
    let pid = child.pid;
    let start = read_proc_start_time(pid).expect("native start");
    assert!(verify_owned_pid(pid, start, &boot));
    reject_identity(&mut child, pid, start ^ 1, &boot).await;
}

#[tokio::test]
async fn termination_identity_wrong_boot_preserves_group() {
    let boot = read_boot_id().expect("native boot UUID required; no substitute identity");
    let mut child = ProtectedChild::spawn(0, false).await;
    let pid = child.pid;
    let start = read_proc_start_time(pid).expect("native start");
    assert!(verify_owned_pid(pid, start, &boot));
    reject_identity(&mut child, pid, start, &format!("{boot}-stale")).await;
}

#[tokio::test]
async fn termination_identity_unprovable_preserves_group() {
    let mut child = ProtectedChild::spawn(0, false).await;
    let pid = child.pid;
    let start = read_proc_start_time(pid).expect("native start");
    // Missing persisted boot identity is always unprovable, including when the OS denies reads.
    reject_identity(&mut child, pid, start, "").await;
}

// A missing pre-check must not fall through to the numeric PGID member sweep either.
#[cfg(target_os = "linux")]
#[tokio::test]
async fn termination_identity_unprovable_preserves_members_ignoring_term() {
    let mut child = ProtectedChild::spawn(0, true).await;
    let pid = child.pid;
    let start = read_proc_start_time(pid).expect("native start");
    reject_identity(&mut child, pid, start, "").await;
}

#[tokio::test]
async fn termination_identity_reaped_leader_preserves_old_group() {
    let boot = read_boot_id().expect("native boot UUID required; no substitute identity");
    let mut leader = ProtectedChild::spawn(0, false).await;
    let start = read_proc_start_time(leader.pid).expect("native start");
    let mut member = ProtectedChild::spawn(leader.pid, false).await;
    leader
        .child
        .kill()
        .await
        .expect("kill and wait owned leader");
    assert!(!verify_owned_pid(leader.pid, start, &boot));
    reject_identity(&mut member, leader.pid, start, &boot).await;
}

#[tokio::test]
async fn termination_identity_valid_external_identity_reaps_group() {
    let boot = read_boot_id().expect("native boot UUID required; no substitute identity");
    let mut child = ProtectedChild::spawn(0, false).await;
    let start = read_proc_start_time(child.pid).expect("native start");
    assert!(verify_owned_pid(child.pid, start, &boot));
    assert!(
        reap_verified_process_group(
            &VerifiedRuntimePaths::for_fixture(Arc::new(BackendPaths::new(
                Path::new("/fixture"),
                &crate::agent_backend_pool::identity::default_id(),
                Path::new("/fixture/home"),
                Path::new("/fixture/logs"),
            ))),
            child.pid,
            child.pid,
            start,
            &boot,
            Duration::from_millis(100),
        )
        .await
    );
    assert!(
        child
            .child
            .try_wait()
            .expect("wait terminated leader")
            .is_some()
    );
}

#[tokio::test]
async fn termination_identity_owned_child_reaps_without_boot_identity() {
    let mut child = ProtectedChild::spawn(0, true).await;
    #[cfg(target_os = "macos")]
    macos_process::terminate_owned(&mut child.child, child.pid, Duration::from_millis(100))
        .await
        .unwrap();
    #[cfg(not(target_os = "macos"))]
    terminate_group_with_grace(
        &VerifiedRuntimePaths::for_fixture(Arc::new(BackendPaths::new(
            Path::new("/fixture"),
            &crate::agent_backend_pool::identity::default_id(),
            Path::new("/fixture/home"),
            Path::new("/fixture/logs"),
        ))),
        SignalScope::Group { pgid: child.pid },
        ExitWait::Child(&mut child.child),
        Duration::from_millis(100),
    )
    .await;
    assert!(child.child.try_wait().expect("wait owned child").is_some());
    assert!(
        child.child.id().is_none(),
        "termination itself must reap owned child"
    );
}

#[cfg(target_os = "macos")]
#[tokio::test]
async fn termination_identity_owned_terminal_pin_reaps_captured_members() {
    let mut leader = ProtectedChild::spawn(0, false).await;
    let mut member = ProtectedChild::spawn(leader.pid, true).await;
    leader.child.start_kill().unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let native = plugin::proc_identity::read_macos_process(leader.pid).unwrap();
            if native.is_some_and(|process| process.is_zombie) {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    // Do not call wait/try_wait before cleanup: this terminal Child still pins
    // the leader while the authoritative set is captured.
    macos_process::terminate_owned(&mut leader.child, leader.pid, Duration::from_secs(5))
        .await
        .unwrap();
    assert!(
        leader.child.id().is_none(),
        "cleanup must reap the terminal direct child"
    );
    assert!(member.child.try_wait().unwrap().is_some());
}

#[cfg(target_os = "macos")]
#[tokio::test]
async fn termination_identity_reaped_owned_child_preserves_original_members() {
    let mut leader = ProtectedChild::spawn(0, false).await;
    let mut member = ProtectedChild::spawn(leader.pid, false).await;
    leader.child.kill().await.unwrap();
    assert!(leader.child.id().is_none());
    assert!(
        macos_process::terminate_owned(&mut leader.child, leader.pid, Duration::from_secs(5))
            .await
            .is_err()
    );
    member.assert_alive().await;
}

// Inject the complete identity observation; signals and polling still use the production boundary.
async fn unknown_during_termination(unknown_at: usize) {
    #[cfg(not(target_os = "macos"))]
    let boot = uuid::Uuid::new_v4().to_string();
    #[cfg(target_os = "macos")]
    let boot = read_boot_id().expect("native boot UUID required; no substitute identity");
    let mut child = ProtectedChild::spawn(0, unknown_at != 1).await;
    let start = read_proc_start_time(child.pid).expect("native start");
    #[cfg(not(target_os = "macos"))]
    let paths = VerifiedRuntimePaths::for_fixture(Arc::new(BackendPaths::new(
        Path::new("/fixture"),
        &crate::agent_backend_pool::identity::default_id(),
        Path::new("/fixture/home"),
        Path::new("/fixture/logs"),
    )));
    #[cfg(not(target_os = "macos"))]
    let mut observations = 0;
    #[cfg(not(target_os = "macos"))]
    let outcome = terminate_group_with_grace_observing(
        &paths,
        SignalScope::Group { pgid: child.pid },
        ExitWait::VerifiedIdentity {
            pid: child.pid,
            start_time: start,
            boot_id: &boot,
        },
        Duration::ZERO,
        |_, _, _| {
            observations += 1;
            if observations >= unknown_at {
                OwnedProcessObservation::Unknown
            } else {
                OwnedProcessObservation::Matching { is_zombie: false }
            }
        },
    )
    .await;
    #[cfg(target_os = "macos")]
    let outcome = {
        // Native reads: initial identity, capture recheck, TERM recheck, post-TERM
        // poll, escalation recheck. Inject the same three observation boundaries.
        let read = match unknown_at {
            1 => 1,
            2 => 4,
            3 => 5,
            _ => unreachable!(),
        };
        let _fault = macos_process::ProcessObservationError::inject(child.pid, read);
        macos_process::terminate_identity(child.pid, Some(child.pid), start, &boot, Duration::ZERO)
            .await
    };
    child.assert_alive().await;
    #[cfg(target_os = "macos")]
    assert!(
        outcome.is_err(),
        "unreadable identity cannot prove termination"
    );
    #[cfg(not(target_os = "macos"))]
    assert_eq!(
        outcome,
        LeaderExit::Unknown,
        "unreadable identity cannot prove termination"
    );
}

#[tokio::test]
async fn termination_identity_unknown_before_signal_preserves_group() {
    unknown_during_termination(1).await;
}

#[tokio::test]
async fn termination_identity_unknown_after_term_preserves_group() {
    unknown_during_termination(2).await;
}

#[tokio::test]
async fn termination_identity_unknown_at_escalation_preserves_group() {
    unknown_during_termination(3).await;
}
