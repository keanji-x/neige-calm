//! When the kernel auto-reaps our pty child, the supervisor detects the lost pin, still publishes `Exited`, and refuses to use that pgid.
//! THIS FILE MUST CONTAIN EXACTLY ONE `#[test]`: the case ignores `SIGCHLD`, which is process-global, and cargo runs one integration file per process.

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

    // Assertion 1: the terminal still terminates. If the `ECHILD` arm fell into the `EINTR` retry arm the waiter would spin forever; the timeout turns that into a red.
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
    // The degraded shape: we never learned how it died.
    assert_eq!(
        (status, signalled),
        (None, false),
        "a lost pin must publish the degraded exit shape"
    );

    // Degeneracy self-check: the child really was auto-reaped, so this case exercised the path it claims to.
    assert!(
        poll_until(Duration::from_secs(5), || !std::path::Path::new(&format!(
            "/proc/{leader}"
        ))
        .exists()),
        "leader {leader} is still present, so the kernel did not auto-reap it and \
         this case is not exercising the ECHILD path at all"
    );

    // Assertion 2: the entry recorded the loss.
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

    // Assertion 3: and refuses to signal that pgid, distinguishably by message prefix (ESRCH also maps to `Internal`).
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

/// Sets the process' `SIGCHLD` disposition so the kernel auto-reaps children, and restores the previous one on drop.
/// The kernel reads the disposition when the child *exits*, so flipping it after the supervisor is running still takes effect. `SIG_IGN` is `1` on Linux; the literal is not spelled here because the wildcard-wait scan forbids it under `src/`.
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
