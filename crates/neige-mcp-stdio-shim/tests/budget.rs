//! These tests run a budget down for real (30 s and 15 s of wall clock) and are
//! the slow ones in this crate.

#![cfg(unix)]

mod common;

use std::cell::Cell;
use std::time::{Duration, Instant};

use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixListener;

use common::{handshake_then_shut_read, read_stderr_line};

/// The longest a run may take before the test itself gives up.
const EXIT_WAIT: Duration = Duration::from_secs(40);

#[tokio::test]
async fn stalled_replay_handshake_exhausts_the_budget_and_exits_5() {
    let (_tmp, socket_path) = common::socket();
    let listener = common::listen(&socket_path);
    let mut child = common::spawn_shim(&socket_path);
    let mut stdin = child.stdin.take().expect("stdin piped");
    let mut stdout = BufReader::new(child.stdout.take().expect("stdout piped"));
    let mut stderr = BufReader::new(child.stderr.take().expect("stderr piped"));

    let old_stream = handshake_then_shut_read(&listener, &mut stdin, &mut stdout).await;
    let t0 = Instant::now();
    common::write_stdin(&mut stdin, &common::tools_call_line(2)).await;
    let lost = read_stderr_line(&mut stderr).await;
    assert!(lost.contains("connection to kernel lost"), "{lost:?}");
    // The shim only probes stdin while reconnecting, so id 3 stays unread in the pipe.
    common::write_stdin(&mut stdin, &common::tools_call_line(3)).await;

    let mut conn = common::accept(&listener, "reconnect").await;
    drop(old_stream);
    let replayed = conn.read_frame("replayed initialize").await;
    common::assert_replayed_initialize(&replayed, 1);

    let code =
        common::wait_exit_code_within(&mut child, EXIT_WAIT, "after the outage budget").await;
    let elapsed = t0.elapsed();
    assert_eq!(code, 5, "exit code");
    assert!(
        elapsed >= Duration::from_secs(29) && elapsed <= EXIT_WAIT,
        "the 30 s budget must be honoured, not skipped: exited after {elapsed:?}"
    );

    let err = common::read_stdout(&mut stdout, "-32000 for the held request").await;
    assert_eq!(err["id"], serde_json::json!(2), "got {err}");
    assert_eq!(err["error"]["code"], serde_json::json!(-32000), "got {err}");
    let mut rest = String::new();
    let n = stdout.read_line(&mut rest).await.expect("stdout read ok");
    assert_eq!(
        n, 0,
        "id 3 was never read by the shim, so nothing may answer it: {rest:?}"
    );

    let mut stderr_rest = String::new();
    stderr
        .read_to_string(&mut stderr_rest)
        .await
        .expect("stderr read ok");
    assert!(
        stderr_rest.contains("reconnect budget exhausted"),
        "stderr after the lost line: {stderr_rest:?}"
    );
    drop(conn);
    drop(stdin);
}

/// The last attempt lands exactly at the deadline, so EOF on the read and EPIPE on
/// the write are the shim giving up, not stub failures.
async fn always_internal_error(listener: &UnixListener, answered: &Cell<u32>) {
    loop {
        let (stream, _addr) = listener.accept().await.expect("accept ok");
        let (rd, mut wr) = stream.into_split();
        let mut reader = BufReader::new(rd);
        let mut line = String::new();
        let n = reader.read_line(&mut line).await.expect("stub read ok");
        if n == 0 {
            continue;
        }
        let init: serde_json::Value =
            serde_json::from_str(line.trim_end()).expect("initialize JSON");
        common::assert_replayed_initialize(&init, 1);
        let reply = serde_json::json!({
            "jsonrpc":"2.0","id":init["id"],"error":{"code":-32603,"message":"repo lookup failed"}
        });
        if wr.write_all(format!("{reply}\n").as_bytes()).await.is_ok() {
            answered.set(answered.get() + 1);
        }
    }
}

/// An outage before codex has an `initialize` response runs under the 15 s
/// initial-connect budget (15 s + 30 s would outlive codex's MCP startup timeout).
#[tokio::test]
async fn unacked_initialize_outage_runs_under_the_initial_budget() {
    let (_tmp, socket_path) = common::socket();
    let listener = common::listen(&socket_path);
    let mut child = common::spawn_shim(&socket_path);
    let mut stdin = child.stdin.take().expect("stdin piped");
    let mut stdout = BufReader::new(child.stdout.take().expect("stdout piped"));
    let mut stderr = BufReader::new(child.stderr.take().expect("stderr piped"));

    let t0 = Instant::now();
    common::write_stdin(&mut stdin, &common::initialize_line(1)).await;
    let answered = Cell::new(0u32);
    let code = tokio::select! {
        code = common::wait_exit_code_within(&mut child, Duration::from_secs(25), "after the initial budget") => code,
        () = always_internal_error(&listener, &answered) => unreachable!("the stub loop never returns"),
    };
    let elapsed = t0.elapsed();
    assert_eq!(code, 5, "exit code");
    assert!(
        elapsed >= Duration::from_secs(14) && elapsed <= Duration::from_secs(25),
        "the 15 s initial budget applies while the initialize is unacknowledged: exited after {elapsed:?}"
    );
    assert!(
        answered.get() >= 2,
        "the original and at least one retry were answered: {}",
        answered.get()
    );

    let err = common::read_stdout(&mut stdout, "synthesized error for the initialize").await;
    assert_eq!(err["id"], serde_json::json!(1), "got {err}");
    assert_eq!(err["error"]["code"], serde_json::json!(-32000), "got {err}");
    let mut rest = String::new();
    let n = stdout.read_line(&mut rest).await.expect("stdout read ok");
    assert_eq!(n, 0, "exactly one initialize response: {rest:?}");

    let mut all = String::new();
    stderr
        .read_to_string(&mut all)
        .await
        .expect("stderr read ok");
    assert!(
        all.contains("initialize answered -32603; retrying under the outage budget"),
        "{all:?}"
    );
    assert!(
        !all.contains("connection to kernel lost"),
        "nothing was lost, so no lost line: {all:?}"
    );
    let exhausted = all
        .lines()
        .find(|l| l.contains("reconnect budget exhausted"))
        .unwrap_or_else(|| panic!("no budget line in {all:?}"));
    assert!(
        exhausted.contains("last error: initialize answered -32603")
            || exhausted.contains("last error: replayed initialize unanswered"),
        "the exit line carries what actually failed last: {exhausted:?}"
    );
    drop(stdin);
}
