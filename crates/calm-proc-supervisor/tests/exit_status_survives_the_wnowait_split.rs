//! #1013 T3b — an end-to-end pass through the new
//! `siginfo -> (status, signalled)` conversion, for three real exit shapes.
//!
//! # Read this before treating it as a gate: it is not one
//!
//! The hard lock on the conversion table is the `--lib` case
//! `wnowait_tests::proc_exit_parts_from_siginfo_maps_every_wexited_code`. It is
//! hermetic and enumerates every `si_code`. This file exists for a different,
//! narrower reason: to run the *whole* production path — `waitid(.., WNOWAIT)`
//! → `proc_exit_parts_from_siginfo` → seal → `Exited` frame — at least once per
//! exit shape, so that a waiter which stops calling the conversion function is
//! caught by something other than reading the diff.
//!
//! **The third arm is deliberately weak and must not be mistaken for a lock on
//! the core-dump row.** From inside the test process we cannot observe
//! `si_code`; it lives in the code under test. All the arm can assert is
//! `{status: None, signalled: true}`, and under a *correct* implementation
//! `CLD_KILLED` and `CLD_DUMPED` produce exactly that. So it cannot distinguish
//! "the environment produced no core dump" from "the code is right". If you
//! want the core-dump row locked, the `--lib` case is where it is locked.
//!
//! **Always runs, never `#[ignore]`, never reddens the suite for environment
//! reasons.** The third arm checks `RLIMIT_CORE` and
//! `/proc/sys/kernel/core_pattern` at runtime and, if dumps are disabled,
//! skips *loudly* on stderr and passes. A silently skipped arm and a
//! `#[ignore]`d one are the same fake gate wearing different clothes; a loud
//! one at least tells you what you did not run.
//!
//! What it does catch, and this is the main reason it exists: a waiter that
//! bypasses `proc_exit_parts_from_siginfo` (e.g. `(Some(si_status), false)`)
//! reddens the second and third arms, and the second arm needs no core-dump
//! support at all.

use calm_proc_supervisor::test_support::InProcessProcSupervisor;
use calm_session::control::{AttachRequest, ControlMsg, ControlReply, EnsureProcRequest, IoMode};
use calm_session::{read_frame, write_frame};
use std::path::Path;
use std::time::Duration;
use tokio::net::UnixStream;

const LIVENESS_BUDGET: Duration = Duration::from_secs(120);

#[tokio::test]
async fn exit_status_survives_the_wnowait_split() {
    let supervisor = InProcessProcSupervisor::start()
        .await
        .expect("start supervisor");

    // Arm 1: a normal exit keeps its code.
    assert_eq!(
        run_and_await_exit(supervisor.sock(), "wnowait-exit", "exit 42", None).await,
        (Some(42), false),
        "a normal exit must survive the WNOWAIT split with its code intact"
    );

    // Arm 2: a fatal signal is reported as signalled, with no code. This arm
    // needs nothing from the environment, and it is the one that catches a
    // waiter that stops routing through the conversion function.
    assert_eq!(
        run_and_await_exit(supervisor.sock(), "wnowait-killed", "kill -9 $$", None).await,
        (None, true),
        "a SIGKILLed child must be reported as signalled"
    );

    // Arm 3: a core dump is *also* signalled (WIFSIGNALED = 1), not an exit
    // with code 6. Weak, per the module doc.
    match core_dumps_disabled_because() {
        Some(reason) => {
            eprintln!(
                "third arm skipped: core dumps disabled by {reason}; the CLD_DUMPED row is \
                 locked by the --lib case proc_exit_parts_from_siginfo_maps_every_wexited_code, \
                 not by this arm"
            );
        }
        None => {
            // cwd is a tempdir because this host's `core_pattern` is a bare
            // filename, i.e. the dump lands in the process' working directory.
            let scratch = tempfile::tempdir().expect("tempdir");
            let got = run_and_await_exit(
                supervisor.sock(),
                "wnowait-dumped",
                "ulimit -c unlimited; kill -ABRT $$",
                Some(scratch.path()),
            )
            .await;
            assert_eq!(
                got,
                (None, true),
                "an aborting child must be reported as signalled, not as an exit with code 6"
            );
        }
    }
}

/// `Some(reason)` when this host cannot produce a core dump, so the third arm
/// can say what it skipped rather than skipping silently.
fn core_dumps_disabled_because() -> Option<String> {
    let mut limit: libc::rlimit = unsafe { std::mem::zeroed() };
    // SAFETY: `limit` is a live, owned `rlimit`; `getrlimit` only writes to it.
    if unsafe { libc::getrlimit(libc::RLIMIT_CORE, &mut limit) } != 0 {
        return Some("RLIMIT_CORE (getrlimit failed)".into());
    }
    // The fixture raises the soft limit itself (`ulimit -c unlimited`), so only
    // a zero *hard* limit is disqualifying.
    if limit.rlim_max == 0 {
        return Some("RLIMIT_CORE (hard limit is 0)".into());
    }
    let pattern = std::fs::read_to_string("/proc/sys/kernel/core_pattern").unwrap_or_default();
    let pattern = pattern.trim();
    if pattern.is_empty() {
        return Some("core_pattern (empty)".into());
    }
    // A `|handler` pattern pipes the dump to a userspace program, which may or
    // may not exist here. The dump is still produced by the kernel and the
    // child still gets CLD_DUMPED, so this is not disqualifying — noted so the
    // next reader does not "fix" it into a skip.
    None
}

async fn run_and_await_exit(
    sock: &Path,
    proc_id: &str,
    script: &str,
    cwd: Option<&Path>,
) -> (Option<i32>, bool) {
    let mut stream = UnixStream::connect(sock).await.expect("connect ensure");
    write_frame(
        &mut stream,
        &ControlMsg::EnsureProc(EnsureProcRequest {
            proc_id: proc_id.into(),
            program: "/bin/sh".into(),
            args: vec!["-c".into(), script.into()],
            envs: Vec::new(),
            cwd: cwd.unwrap_or(Path::new("/tmp")).display().to_string(),
            ready_timeout_ms: 0,
            io_mode: IoMode::Pty { cols: 80, rows: 24 },
            replay_bytes: 1024 * 1024,
        }),
    )
    .await
    .expect("write ensure");
    match read_frame(&mut stream).await.expect("read spawned") {
        ControlReply::Spawned { .. } => {}
        other => panic!("unexpected first reply: {other:?}"),
    }
    match read_frame(&mut stream).await.expect("read ready") {
        ControlReply::Ready => {}
        other => panic!("unexpected second reply: {other:?}"),
    }

    let mut attach = UnixStream::connect(sock).await.expect("connect attach");
    write_frame(
        &mut attach,
        &ControlMsg::Attach(AttachRequest {
            proc_id: proc_id.into(),
            from_cursor: Some(0),
            reader_id: "wnowait-test".into(),
        }),
    )
    .await
    .expect("write attach");
    match read_frame(&mut attach).await.expect("read attach ok") {
        ControlReply::AttachOk(_) => {}
        other => panic!("unexpected attach reply: {other:?}"),
    }
    loop {
        let frame = tokio::time::timeout(LIVENESS_BUDGET, read_frame(&mut attach))
            .await
            .expect("timed out waiting for Exited")
            .expect("read frame");
        match frame {
            ControlReply::Exited {
                status, signalled, ..
            } => return (status, signalled),
            ControlReply::Output { .. } => {}
            other => panic!("unexpected frame before Exited: {other:?}"),
        }
    }
}
