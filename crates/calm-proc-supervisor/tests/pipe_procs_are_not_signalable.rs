//! #1013 T9 — a Pipe proc is not group-addressable through the `Signal` RPC.
//!
//! **What this proves**: `handle_signal` runs every target through
//! `pgid_lease::require_addressable_by_signal_rpc`, and that gate refuses
//! `PipeBestEffort` with a `WrongState` frame (not a dropped connection, and
//! not `Internal` — `Internal` is reserved for genuine internal degradation
//! such as PR-B's `pin_lost`).
//!
//! **What it does NOT prove**:
//!
//! * It does not prove the check→kill race of #1013 is gone for Pipe. It only
//!   removes one *destination*.
//! * It only covers the `Signal` RPC. `terminate_all_process_groups_sync`
//!   **still** group-SIGTERMs Pipe entries — that is the #388 "supervisor death
//!   drops procs" payload and it must stay. The gate for *that* rule is the
//!   `--lib` test
//!   `pgid_lease_tests::pipe_target_is_ok_kind_readable_and_refused_by_the_signal_rpc`,
//!   **not** `server_restart_survives.rs`: when `group_target` wrongly returns
//!   `Err` for Pipe, `server_restart_survives` **hangs rather than fails**,
//!   because the supervisor's `#[tokio::main]` runtime drop blocks on the
//!   `reap_children` `waitpid` and so cannot exit before the pipe child's own
//!   30s self-exit. (`server_restart_survives` now also asserts elapsed, which
//!   makes it a real secondary gate.)
//!   The pgid used on that path is still unpinned; that is a knowingly
//!   accepted gap, not an oversight.
//!
//! This is a protocol-visible behavior change: `SignalRequest` is defined in
//! `calm-session` and exposed over the UDS without any `IoMode` restriction, so
//! a workspace grep can only show that no in-repo client sends it to a Pipe
//! proc, not that no external client can.

use calm_proc_supervisor::test_support::InProcessProcSupervisor;
use calm_session::control::{
    ControlErrorKind, ControlMsg, ControlReply, EnsureProcRequest, IoMode, ProcSignal,
    SignalRequest,
};
use calm_session::{read_frame, write_frame};
use std::path::{Path, PathBuf};
use std::time::Duration;
use tokio::net::UnixStream;

/// Liveness upper bound for "read the next frame". Anti-hang guard only — no
/// assertion here claims the supervisor reacts within this budget, so a
/// slow-but-correct run still passes.
const LIVENESS_BUDGET: Duration = Duration::from_secs(120);

#[tokio::test]
async fn pipe_procs_are_not_signalable_via_the_signal_rpc() {
    let temp = tempfile::tempdir().expect("tempdir");
    let supervisor = InProcessProcSupervisor::start()
        .await
        .expect("start supervisor");
    let proc_id = "pipe-signal";

    let mut stream = UnixStream::connect(supervisor.sock())
        .await
        .expect("connect ensure");
    write_frame(
        &mut stream,
        &ControlMsg::EnsureProc(EnsureProcRequest {
            proc_id: proc_id.into(),
            program: locate_bin("proc-supervisor-ready-sleeper")
                .display()
                .to_string(),
            args: vec![
                "--id".into(),
                proc_id.into(),
                "--sock".into(),
                temp.path().join("session.sock").display().to_string(),
                "--ready-fd".into(),
                "0".into(),
            ],
            envs: Vec::new(),
            cwd: temp.path().display().to_string(),
            // Upper bound only (same shape as LIVENESS_BUDGET): the ready
            // handshake returns as soon as the fixture writes "ready\n".
            ready_timeout_ms: LIVENESS_BUDGET.as_millis() as u64,
            io_mode: IoMode::Pipe,
            replay_bytes: 0,
        }),
    )
    .await
    .expect("write ensure");
    match timeout_read(&mut stream).await {
        ControlReply::Spawned { .. } => {}
        other => panic!("unexpected first reply: {other:?}"),
    }
    match timeout_read(&mut stream).await {
        ControlReply::Ready => {}
        other => panic!("unexpected second reply: {other:?}"),
    }

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

    match timeout_read(&mut control).await {
        ControlReply::Error { kind, message } => {
            assert_eq!(
                kind,
                ControlErrorKind::WrongState,
                "pipe refusal is a wrong-state answer, not an internal failure \
                 (Internal is reserved for pin loss); message was: {message}"
            );
            assert!(
                message.starts_with("pipe runtime is not group-signalable via the Signal RPC"),
                "stable message prefix, got: {message}"
            );
        }
        ControlReply::SignalOk => panic!(
            "the Signal RPC group-addressed a pipe proc: \
             require_addressable_by_signal_rpc is not on the path"
        ),
        other => panic!("unexpected signal reply: {other:?}"),
    }
}

async fn timeout_read(stream: &mut UnixStream) -> ControlReply {
    tokio::time::timeout(LIVENESS_BUDGET, read_frame(stream))
        .await
        .expect("timed out reading reply")
        .expect("read reply")
}

fn locate_bin(name: &str) -> PathBuf {
    let env_key = format!("CARGO_BIN_EXE_{name}");
    if let Ok(path) = std::env::var(env_key) {
        return PathBuf::from(path);
    }
    let me = std::env::current_exe().expect("current_exe");
    let target_profile: &Path = me
        .parent()
        .and_then(|p| p.parent())
        .expect("test bin parent");
    let candidate = target_profile.join(name);
    if candidate.exists() {
        return candidate;
    }
    panic!("{name} binary not found at {}", candidate.display());
}
