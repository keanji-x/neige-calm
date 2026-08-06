use calm_proc_supervisor::test_support::InProcessProcSupervisor;
use calm_session::control::{
    AttachRequest, ControlMsg, ControlReply, EnsureProcRequest, IoMode, ProcSignal, SignalRequest,
};
use calm_session::{read_frame, write_frame};
use std::path::Path;
use std::time::Duration;
use tokio::net::UnixStream;

/// Liveness upper bound for "read the next frame / wait for the expected
/// state". Anti-hang guard only — no case here claims the supervisor reacts
/// within this budget, so a slow-but-correct run must still pass. Costs
/// nothing on the happy path (each wait returns as soon as its frame lands).
/// The 1-2s budgets this replaces are the same shape that flaked on CI's
/// 2-core runner under `retries = 0`. 120s is the `slow-timeout` of nextest
/// `profile.ci`; the local `profile.default` warns at 60s. Both are warn-only,
/// so neither kills the test — past this point nextest's slow-test report is the
/// signal, not a hand-picked deadline.
/// To assert promptness, measure elapsed and assert on it instead.
const LIVENESS_BUDGET: Duration = Duration::from_secs(120);

#[tokio::test]
async fn signal_terminates_pty_child() {
    let supervisor = InProcessProcSupervisor::start()
        .await
        .expect("start supervisor");
    let proc_id = "pty-signal";
    // The sleep must outlast `LIVENESS_BUDGET`. `assert!(signalled)` below keeps
    // a natural exit from turning into a false green, but a child that reaps
    // itself inside the budget would make a broken signal path take the full
    // sleep to report — and the failure would read as a timeout rather than as
    // "the signal never arrived".
    ensure_pty(supervisor.sock(), proc_id, "/bin/sleep", &["600"]).await;

    let mut attach = UnixStream::connect(supervisor.sock())
        .await
        .expect("connect attach");
    write_frame(
        &mut attach,
        &ControlMsg::Attach(AttachRequest {
            proc_id: proc_id.into(),
            from_cursor: None,
            reader_id: "test".into(),
        }),
    )
    .await
    .expect("write attach");
    match read_frame(&mut attach).await.expect("read attach ok") {
        ControlReply::AttachOk(_) => {}
        other => panic!("unexpected attach reply: {other:?}"),
    }

    let mut control = UnixStream::connect(supervisor.sock())
        .await
        .expect("connect control");
    write_frame(
        &mut control,
        &ControlMsg::Signal(SignalRequest {
            proc_id: proc_id.into(),
            sig: ProcSignal::Kill,
        }),
    )
    .await
    .expect("write signal");
    match timeout_read(&mut control).await {
        ControlReply::SignalOk => {}
        other => panic!("expected SignalOk, got {other:?}"),
    }

    loop {
        match timeout_read(&mut attach).await {
            ControlReply::Exited { signalled, .. } => {
                assert!(signalled, "expected signal-killed exit");
                break;
            }
            ControlReply::Output { .. } => {}
            other => panic!("unexpected attach frame before exit: {other:?}"),
        }
    }
}

#[tokio::test]
/// Proves Signal targets the spawned leader, not the tty's foreground job.
/// Group-wise rather than bare-pid delivery is locked separately by
/// `terminate_all_after_exit_recorded`, through `kill_group`'s negation.
async fn signal_targets_spawned_leader_not_tty_foreground_job() {
    let supervisor = InProcessProcSupervisor::start()
        .await
        .expect("start supervisor");
    let proc_id = "pty-signal-leader-not-foreground";
    let fixture = env!("CARGO_BIN_EXE_proc-supervisor-foreground-job");

    let mut ensure = UnixStream::connect(supervisor.sock())
        .await
        .expect("connect ensure");
    write_frame(
        &mut ensure,
        &ControlMsg::EnsureProc(EnsureProcRequest {
            proc_id: proc_id.into(),
            program: fixture.into(),
            args: Vec::new(),
            envs: Vec::new(),
            cwd: "/tmp".into(),
            ready_timeout_ms: 0,
            io_mode: IoMode::Pty { cols: 80, rows: 24 },
            replay_bytes: 1024 * 1024,
        }),
    )
    .await
    .expect("write ensure");
    let leader_pid = match timeout_read(&mut ensure).await {
        ControlReply::Spawned { pid } => pid,
        other => panic!("expected Spawned, got {other:?}"),
    };
    assert!(matches!(
        timeout_read(&mut ensure).await,
        ControlReply::Ready
    ));

    let mut attach = UnixStream::connect(supervisor.sock())
        .await
        .expect("connect attach");
    write_frame(
        &mut attach,
        &ControlMsg::Attach(AttachRequest {
            proc_id: proc_id.into(),
            from_cursor: Some(0),
            reader_id: "foreground-job-test".into(),
        }),
    )
    .await
    .expect("write attach");
    let replay = match timeout_read(&mut attach).await {
        ControlReply::AttachOk(attached) => attached.replay,
        other => panic!("unexpected attach reply: {other:?}"),
    };

    let mut output = replay;
    let foreground_pid = loop {
        let text = String::from_utf8_lossy(&output);
        if let Some(value) = text
            .split_whitespace()
            .find_map(|word| word.strip_prefix("foreground_pid="))
        {
            break value.parse::<u32>().expect("numeric foreground pid");
        }
        match timeout_read(&mut attach).await {
            ControlReply::Output { bytes, .. } => {
                output.extend_from_slice(&bytes);
            }
            other => panic!("expected fixture output, got {other:?}"),
        }
    };
    let foreground_guard = ExactFixturePid(foreground_pid);

    let leader_stat = proc_stat(leader_pid);
    let foreground_stat = proc_stat(foreground_pid);
    assert_eq!(
        leader_stat.pgrp, leader_pid as i32,
        "leader must lead its group"
    );
    assert_eq!(
        foreground_stat.pgrp, foreground_pid as i32,
        "fixture child must lead a distinct foreground group"
    );
    assert_eq!(
        leader_stat.tty_foreground_pgrp, foreground_stat.pgrp,
        "fixture must install the child group as the tty foreground group"
    );
    assert_ne!(
        leader_stat.tty_foreground_pgrp, leader_stat.pgrp,
        "test must establish differing foreground and leader pgids before Signal"
    );

    let mut control = UnixStream::connect(supervisor.sock())
        .await
        .expect("connect control");
    write_frame(
        &mut control,
        &ControlMsg::Signal(SignalRequest {
            proc_id: proc_id.into(),
            sig: ProcSignal::Term,
        }),
    )
    .await
    .expect("write signal");
    assert!(matches!(
        timeout_read(&mut control).await,
        ControlReply::SignalOk
    ));
    tokio::time::timeout(Duration::from_secs(30), async {
        while proc_stat(leader_pid).state != 'Z' {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("spawned leader did not become a zombie after leader-group SIGTERM");

    loop {
        match timeout_read(&mut attach).await {
            ControlReply::Exited { signalled, .. } => {
                assert!(signalled, "the spawned leader must receive SIGTERM");
                break;
            }
            ControlReply::Output { .. } => {}
            other => panic!("unexpected attach frame before exit: {other:?}"),
        }
    }
    assert!(
        Path::new(&format!("/proc/{foreground_pid}")).exists(),
        "fixture cleanup guard requires the foreground job to remain alive"
    );
    drop(foreground_guard);
}

struct ProcStat {
    state: char,
    pgrp: i32,
    tty_foreground_pgrp: i32,
}

fn proc_stat(pid: u32) -> ProcStat {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).expect("read /proc stat");
    let comm_end = stat.rfind(')').expect("/proc stat comm terminator");
    let fields: Vec<&str> = stat[comm_end + 1..].split_whitespace().collect();
    ProcStat {
        state: fields[0].parse().expect("state"),
        pgrp: fields[2].parse().expect("pgrp"),
        tty_foreground_pgrp: fields[5].parse().expect("tpgid"),
    }
}

struct ExactFixturePid(u32);

impl Drop for ExactFixturePid {
    fn drop(&mut self) {
        let cmdline = std::fs::read(format!("/proc/{}/cmdline", self.0)).unwrap_or_default();
        if cmdline
            .windows(b"proc-supervisor-foreground-job".len())
            .any(|window| window == b"proc-supervisor-foreground-job")
        {
            unsafe {
                libc::kill(self.0 as libc::pid_t, libc::SIGKILL);
            }
        }
    }
}

async fn ensure_pty(sock: &Path, proc_id: &str, program: &str, args: &[&str]) {
    let mut stream = UnixStream::connect(sock).await.expect("connect ensure");
    write_frame(
        &mut stream,
        &ControlMsg::EnsureProc(EnsureProcRequest {
            proc_id: proc_id.into(),
            program: program.into(),
            args: args.iter().map(|arg| (*arg).into()).collect(),
            envs: Vec::new(),
            cwd: "/tmp".into(),
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
}

async fn timeout_read(stream: &mut UnixStream) -> ControlReply {
    tokio::time::timeout(LIVENESS_BUDGET, read_frame(stream))
        .await
        .expect("timed out reading reply")
        .expect("read reply")
}
