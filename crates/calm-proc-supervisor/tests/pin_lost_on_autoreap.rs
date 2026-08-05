//! #1013 T6 — when the kernel auto-reaps our pty child, the supervisor detects
//! the lost pin, still publishes `Exited`, and refuses to use that pgid.
//!
//! # THIS FILE MUST CONTAIN EXACTLY ONE `#[test]`. Do not add a second one.
//!
//! The case sets `SIGCHLD` to be ignored, and a signal disposition is
//! **process-global**. `cargo test` runs the cases of one integration file as
//! threads of one process, so any pty child spawned concurrently by a sibling
//! case in *this binary* would be auto-reaped too — a nondeterministic red
//! somewhere else entirely, which is the exact failure mode this line of work
//! has been burned by repeatedly. cargo compiles and runs each integration
//! **file** as its own process, so one case per file is the isolation.
//! Parallelism across binaries is fine: the disposition is per process.
//!
//! **This is a written-down convention, not a gate.** There is no assertion
//! that can count the `#[test]`s in its own file. Stated plainly rather than
//! dressed up as enforcement.
//!
//! # What this locks, and what it structurally cannot
//!
//! It locks the **steady state**: once `waitid` has answered `ECHILD`, (1) the
//! terminal still terminates — the most important part, because a waiter that
//! spun on `ECHILD` would hang the terminal forever, which is worse than the
//! bug #1013 fixes — (2) the entry records `pin_lost`, and (3) a subsequent
//! `Signal` is refused with the `PinLost` message rather than being sent to a
//! pgid the kernel has already recycled.
//!
//! It **cannot** cover the release→flag gap (§6.4): the kernel frees the number
//! when the child *exits*, and our flag is set when `waitid` returns. A
//! concurrent `Signal` in between aims at a recyclable pgid — #1013, verbatim.
//! This case waits for `pin_lost` before signalling, by construction. No
//! userspace mechanism can close that window, so `pin_lost` is graded as
//! best-effort detection that stops *subsequent* signals, and never as
//! fail-closed. The production answer is the standalone daemon's process
//! invariant (`main.rs`: never ignored, never no-child-wait; a handler is
//! fine); the in-process mode is a documented degraded configuration.
//!
//! Assertion 3 asserts the **message prefix**, not just the error kind, and
//! that is load-bearing. The child has been auto-reaped, so its group is empty
//! and `kill(-pgid, SIGKILL)` returns ESRCH — which `handle_signal` also maps
//! to `Internal`. Asserting only the kind would keep this case green after
//! deleting the `pin_lost` check in `group_target`, which is precisely the
//! wiring this case exists to prove.

use calm_proc_supervisor::test_support::InProcessProcSupervisor;
use calm_session::control::{
    AttachRequest, ControlErrorKind, ControlMsg, ControlReply, EnsureProcRequest, IoMode,
    ProcSignal, SignalRequest,
};
use calm_session::{read_frame, write_frame};
use std::time::{Duration, Instant};
use tokio::net::UnixStream;

const LIVENESS_BUDGET: Duration = Duration::from_secs(120);

#[tokio::test]
async fn pin_lost_when_the_child_is_autoreaped() {
    let _restore = IgnoreSigchld::install();

    let supervisor = InProcessProcSupervisor::start()
        .await
        .expect("start supervisor");
    let proc_id = "pty-pin-lost";

    let mut stream = UnixStream::connect(supervisor.sock())
        .await
        .expect("connect ensure");
    write_frame(
        &mut stream,
        &ControlMsg::EnsureProc(EnsureProcRequest {
            proc_id: proc_id.into(),
            program: "/bin/sh".into(),
            args: vec!["-c".into(), "exit 0".into()],
            envs: Vec::new(),
            cwd: "/tmp".into(),
            ready_timeout_ms: 0,
            io_mode: IoMode::Pty { cols: 80, rows: 24 },
            replay_bytes: 1024 * 1024,
        }),
    )
    .await
    .expect("write ensure");
    let leader = match read_frame(&mut stream).await.expect("read spawned") {
        ControlReply::Spawned { pid } => pid,
        other => panic!("unexpected first reply: {other:?}"),
    };
    match read_frame(&mut stream).await.expect("read ready") {
        ControlReply::Ready => {}
        other => panic!("unexpected second reply: {other:?}"),
    }

    let mut attach = UnixStream::connect(supervisor.sock())
        .await
        .expect("connect attach");
    write_frame(
        &mut attach,
        &ControlMsg::Attach(AttachRequest {
            proc_id: proc_id.into(),
            from_cursor: Some(0),
            reader_id: "pin-lost-test".into(),
        }),
    )
    .await
    .expect("write attach");
    match read_frame(&mut attach).await.expect("read attach ok") {
        ControlReply::AttachOk(_) => {}
        other => panic!("unexpected attach reply: {other:?}"),
    }

    // --- Assertion 1: the terminal still terminates. ----------------------
    //
    // The most important one. If the `ECHILD` arm fell into the `EINTR` retry
    // arm, `waitid` would answer `ECHILD` forever, the waiter would spin, and
    // no `Exited` frame would ever be published — a hang, not a degradation.
    // The timeout below is what turns that into a red.
    let (status, signalled) = loop {
        let frame = tokio::time::timeout(LIVENESS_BUDGET, read_frame(&mut attach))
            .await
            .expect(
                "no Exited frame: the waiter never finished. If the ECHILD arm \
                 was folded into the EINTR retry arm, it is spinning forever and \
                 this terminal will never terminate",
            )
            .expect("read frame");
        match frame {
            ControlReply::Exited {
                status, signalled, ..
            } => break (status, signalled),
            ControlReply::Output { .. } => {}
            other => panic!("unexpected frame before Exited: {other:?}"),
        }
    };
    // The degraded shape: we never learned how it died, because the kernel
    // reaped it before we could look. Same shape the pre-#1013 code published
    // when `child.wait()` failed, so no reader sees anything new.
    assert_eq!(
        (status, signalled),
        (None, false),
        "a lost pin must publish the degraded exit shape"
    );

    // Degeneracy self-check: the child really was auto-reaped, i.e. this case
    // exercised the path it claims to. Without it, a run where the disposition
    // did not take effect would still reach the assertions below.
    assert!(
        poll_until(Duration::from_secs(5), || !std::path::Path::new(&format!(
            "/proc/{leader}"
        ))
        .exists()),
        "leader {leader} is still present, so the kernel did not auto-reap it and \
         this case is not exercising the ECHILD path at all"
    );

    // --- Assertion 2: the entry recorded the loss. ------------------------
    let stats = supervisor
        .registry()
        .debug_entry_stats(proc_id)
        .expect("entry still registered inside the reclaim grace");
    assert!(
        stats.pin_lost,
        "the entry must record that the kernel disowned its leader"
    );
    assert_eq!(
        supervisor.registry().debug_pin_lost_count(),
        1,
        "the registry-scoped pin_lost counter must have seen it"
    );

    // --- Assertion 3: and refuses to signal that pgid, distinguishably. ---
    let mut control = UnixStream::connect(supervisor.sock())
        .await
        .expect("connect signal");
    write_frame(
        &mut control,
        &ControlMsg::Signal(SignalRequest {
            proc_id: proc_id.into(),
            sig: ProcSignal::Kill,
        }),
    )
    .await
    .expect("write signal");
    match tokio::time::timeout(LIVENESS_BUDGET, read_frame(&mut control))
        .await
        .expect("timed out reading signal reply")
        .expect("read signal reply")
    {
        ControlReply::Error { kind, message } => {
            assert_eq!(kind, ControlErrorKind::Internal);
            assert!(
                message.starts_with("pty leader pin lost for proc "),
                "the refusal must be the PinLost message, not Kill(ESRCH) — an \
                 auto-reaped child leaves an empty group, so kill(-pgid, ..) also \
                 returns ESRCH and also maps to Internal. Got: {message}"
            );
        }
        other => panic!("expected Error{{Internal}} with the PinLost message, got {other:?}"),
    }
}

fn poll_until(budget: Duration, mut cond: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + budget;
    while Instant::now() < deadline {
        if cond() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    cond()
}

/// Sets the process' `SIGCHLD` disposition so the kernel auto-reaps children,
/// and restores the previous one on drop.
///
/// The decision to auto-reap is taken when the child *exits*, reading the
/// disposition at that moment — not when it is forked. That is why flipping it
/// here, after the supervisor is running, still takes effect on children
/// spawned afterwards, and it is also why a check at spawn time could never be
/// a gate.
///
/// The dangerous handler constant is not spelled here: this crate's
/// `no_wildcard_wait_in_the_supervisor_host` scan forbids those two literals
/// under `src/`, and repeating them in a test file next door invites someone to
/// copy them into production. `SIG_IGN` is `1` on Linux, which is what the
/// kernel's `sig_handler_ignored` compares against.
struct IgnoreSigchld {
    previous: libc::sigaction,
}

impl IgnoreSigchld {
    fn install() -> Self {
        unsafe {
            let mut action: libc::sigaction = std::mem::zeroed();
            action.sa_sigaction = 1 as libc::sighandler_t;
            action.sa_flags = 0;
            libc::sigemptyset(&mut action.sa_mask);
            let mut previous: libc::sigaction = std::mem::zeroed();
            assert_eq!(
                libc::sigaction(libc::SIGCHLD, &action, &mut previous),
                0,
                "could not make SIGCHLD auto-reaping: {}",
                std::io::Error::last_os_error()
            );
            Self { previous }
        }
    }
}

impl Drop for IgnoreSigchld {
    fn drop(&mut self) {
        unsafe {
            libc::sigaction(libc::SIGCHLD, &self.previous, std::ptr::null_mut());
        }
    }
}
