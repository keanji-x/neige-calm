//! #1699 — reconnect behaviour of `neige-mcp-stdio-shim`, one test per
//! pinned decision (D2, D4, D5-a, D5-b, D6). The repro test for the
//! observed bug lives in `stdio_shim.rs`
//! (`kernel_restart_reconnects_and_replays_initialize`).
//!
//! Every stub is driven explicitly and every wait is bounded by
//! `common::TEST_BUDGET`; there is no sleep-and-hope except the one
//! deliberate 500 ms delay in the late-bind test.

#![cfg(unix)]

mod common;

use std::net::Shutdown;
use std::time::Duration;

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixListener;
use tokio::time::timeout;

use common::{TEST_BUDGET, TOKEN};

/// Read the shim's next stderr line (one per state change).
async fn read_stderr_line(reader: &mut BufReader<tokio::process::ChildStderr>) -> String {
    let mut line = String::new();
    let n = timeout(TEST_BUDGET, reader.read_line(&mut line))
        .await
        .expect("stderr line within budget")
        .expect("stderr read ok");
    assert!(n > 0, "shim closed stderr");
    line
}

/// The stub reads the handshake on a raw stream, answers it, then shuts
/// down its READ side only. The shim's next socket write fails with
/// EPIPE while its read side sees no EOF — the D5-a shape, where zero
/// bytes reached the kernel. Returns the stream so the caller decides
/// when the shim finally sees EOF.
async fn handshake_then_shut_read(
    listener: &UnixListener,
    stdin: &mut tokio::process::ChildStdin,
    stdout: &mut BufReader<tokio::process::ChildStdout>,
) -> std::os::unix::net::UnixStream {
    let (mut stream, _addr) = timeout(TEST_BUDGET, listener.accept())
        .await
        .expect("shim connected within budget")
        .expect("accept ok");
    common::write_stdin(stdin, &common::initialize_line(1)).await;
    let mut line = String::new();
    {
        let mut reader = BufReader::new(&mut stream);
        timeout(TEST_BUDGET, reader.read_line(&mut line))
            .await
            .expect("stub read initialize within budget")
            .expect("stub read ok");
    }
    let init: serde_json::Value = serde_json::from_str(line.trim_end()).expect("initialize JSON");
    common::assert_replayed_initialize(&init, 1);
    stream
        .write_all(b"{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{}}\n")
        .await
        .expect("stub reply ok");
    let resp = common::read_stdout(stdout, "initialize response").await;
    assert_eq!(resp["id"], serde_json::json!(1));

    let std_stream = stream.into_std().expect("into_std");
    std_stream
        .shutdown(Shutdown::Read)
        .expect("shutdown read side");
    std_stream
}

/// D5-a: a request whose socket write failed was never queued at the
/// kernel; it is re-sent on the new connection after the replayed
/// handshake, and codex sees its response as if nothing happened.
#[tokio::test]
async fn write_failure_resends_the_frame_after_reconnect() {
    let (_tmp, socket_path) = common::socket();
    let listener = common::listen(&socket_path);
    let mut child = common::spawn_shim(&socket_path);
    let mut stdin = child.stdin.take().expect("stdin piped");
    let mut stdout = BufReader::new(child.stdout.take().expect("stdout piped"));
    let mut stderr = BufReader::new(child.stderr.take().expect("stderr piped"));

    let old_stream = handshake_then_shut_read(&listener, &mut stdin, &mut stdout).await;

    // This write hits EPIPE inside the shim.
    common::write_stdin(&mut stdin, &common::tools_call_line(2)).await;
    let lost = read_stderr_line(&mut stderr).await;
    assert!(
        lost.contains("connection to kernel lost (0 unanswered requests failed)"),
        "the failed write must not count as delivered: {lost:?}"
    );

    // Same listener, new connection: replayed initialize first ...
    let mut conn = common::accept(&listener, "reconnect after write failure").await;
    drop(old_stream);
    let replayed = conn.read_frame("replayed initialize").await;
    common::assert_replayed_initialize(&replayed, 1);
    conn.reply_ok(&replayed["id"]).await;
    // ... then the re-sent request, byte-identical.
    let resent = conn.read_frame("re-sent tools/call").await;
    assert_eq!(resent["method"], "tools/call");
    assert_eq!(resent["id"], serde_json::json!(2));
    conn.reply_ok(&resent["id"]).await;

    let resp = common::read_stdout(&mut stdout, "tools/call response").await;
    assert_eq!(resp["id"], serde_json::json!(2), "got {resp}");
    assert!(
        resp.get("result").is_some(),
        "must be the kernel's answer, got {resp}"
    );
    common::assert_alive(&mut child);

    drop(stdin);
    drop(conn);
    let _ = timeout(TEST_BUDGET, child.wait()).await;
}

/// D5-b: a request the kernel read but never answered gets the
/// synthesized -32000 as soon as the shim notices the hang-up, and is
/// never re-sent on the new connection.
#[tokio::test]
async fn unanswered_request_gets_lost_error_and_is_not_resent() {
    let (_tmp, socket_path) = common::socket();
    let listener = common::listen(&socket_path);
    let mut child = common::spawn_shim(&socket_path);
    let mut stdin = child.stdin.take().expect("stdin piped");
    let mut stdout = BufReader::new(child.stdout.take().expect("stdout piped"));

    let mut conn = common::accept(&listener, "first connection").await;
    common::write_stdin(&mut stdin, &common::initialize_line(1)).await;
    let init = conn.read_frame("initialize").await;
    conn.reply_ok(&init["id"]).await;
    let resp = common::read_stdout(&mut stdout, "initialize response").await;
    assert_eq!(resp["id"], serde_json::json!(1));

    // The kernel reads the request, then dies before answering.
    common::write_stdin(&mut stdin, &common::tools_call_line(2)).await;
    let call = conn.read_frame("tools/call").await;
    assert_eq!(call["id"], serde_json::json!(2));
    drop(conn);
    drop(listener);

    // codex gets the synthesized error before any reconnect happens
    // (the listener is not even bound yet).
    let err = common::read_stdout(&mut stdout, "synthesized error").await;
    assert_eq!(err["id"], serde_json::json!(2), "got {err}");
    assert_eq!(err["error"]["code"], serde_json::json!(-32000), "got {err}");
    let message = err["error"]["message"].as_str().expect("message string");
    assert!(
        message.contains("outcome unknown"),
        "message must say the outcome is unknown: {message:?}"
    );
    assert!(err.get("result").is_none());

    // New kernel: replayed initialize, then the NEXT request — id 2
    // never reappears.
    let listener = common::rebind(&socket_path);
    let mut conn = common::accept(&listener, "reconnect").await;
    let replayed = conn.read_frame("replayed initialize").await;
    common::assert_replayed_initialize(&replayed, 1);
    conn.reply_ok(&replayed["id"]).await;
    common::write_stdin(&mut stdin, &common::tools_call_line(3)).await;
    let next = conn.read_frame("first request after replay").await;
    assert_eq!(
        next["id"],
        serde_json::json!(3),
        "the unanswered id 2 must not be re-sent; got {next}"
    );
    conn.reply_ok(&next["id"]).await;
    let resp = common::read_stdout(&mut stdout, "id 3 response").await;
    assert_eq!(resp["id"], serde_json::json!(3));
    common::assert_alive(&mut child);

    drop(stdin);
    drop(conn);
    let _ = timeout(TEST_BUDGET, child.wait()).await;
}

/// D4 terminal: the replayed initialize answered with a non-retryable
/// error ends the shim with exit 4, the held request gets the
/// synthesized error, and no further connect attempt is made.
#[tokio::test]
async fn replayed_initialize_rejected_fails_requests_and_exits_4() {
    let (_tmp, socket_path) = common::socket();
    let listener = common::listen(&socket_path);
    let mut child = common::spawn_shim(&socket_path);
    let mut stdin = child.stdin.take().expect("stdin piped");
    let mut stdout = BufReader::new(child.stdout.take().expect("stdout piped"));
    let mut stderr = BufReader::new(child.stderr.take().expect("stderr piped"));

    let old_stream = handshake_then_shut_read(&listener, &mut stdin, &mut stdout).await;
    common::write_stdin(&mut stdin, &common::tools_call_line(2)).await;
    let lost = read_stderr_line(&mut stderr).await;
    assert!(lost.contains("connection to kernel lost"), "{lost:?}");

    let mut conn = common::accept(&listener, "reconnect").await;
    drop(old_stream);
    let replayed = conn.read_frame("replayed initialize").await;
    common::assert_replayed_initialize(&replayed, 1);
    conn.reply_error(&replayed["id"], -32401, "session not active")
        .await;

    // The held request is answered with the synthesized error ...
    let err = common::read_stdout(&mut stdout, "synthesized error for held request").await;
    assert_eq!(err["id"], serde_json::json!(2), "got {err}");
    assert_eq!(err["error"]["code"], serde_json::json!(-32000), "got {err}");
    let rejected = read_stderr_line(&mut stderr).await;
    assert!(rejected.contains("rejected"), "{rejected:?}");

    // ... the process exits 4 without connecting again (stdin is still
    // open, so this is not the clean path).
    let code = tokio::select! {
        code = common::wait_exit_code(&mut child, "after rejection") => code,
        _ = listener.accept() => panic!("shim reconnected after a terminal initialize rejection"),
    };
    assert_eq!(code, 4);
    assert!(
        timeout(Duration::from_secs(1), listener.accept())
            .await
            .is_err(),
        "no connect attempt may arrive after exit"
    );
    drop(stdin);
}

/// D4 retry: -32603 on the replayed initialize is the kernel's
/// transient repo error; the shim reconnects again under the same
/// budget and resumes once a replay is accepted.
#[tokio::test]
async fn replayed_initialize_internal_error_retries_then_resumes() {
    let (_tmp, socket_path) = common::socket();
    let listener = common::listen(&socket_path);
    let mut child = common::spawn_shim(&socket_path);
    let mut stdin = child.stdin.take().expect("stdin piped");
    let mut stdout = BufReader::new(child.stdout.take().expect("stdout piped"));
    let mut stderr = BufReader::new(child.stderr.take().expect("stderr piped"));

    let mut conn = common::accept(&listener, "first connection").await;
    common::write_stdin(&mut stdin, &common::initialize_line(1)).await;
    let init = conn.read_frame("initialize").await;
    conn.reply_ok(&init["id"]).await;
    let resp = common::read_stdout(&mut stdout, "initialize response").await;
    assert_eq!(resp["id"], serde_json::json!(1));
    drop(conn);
    let lost = read_stderr_line(&mut stderr).await;
    assert!(lost.contains("connection to kernel lost"), "{lost:?}");

    // Second connection: transient error, then the kernel drops it
    // (as `handle_connection` does after any initialize error).
    let mut conn = common::accept(&listener, "second connection").await;
    let replayed = conn.read_frame("replayed initialize (1st)").await;
    common::assert_replayed_initialize(&replayed, 1);
    conn.reply_error(&replayed["id"], -32603, "repo lookup failed")
        .await;
    drop(conn);

    // Third connection: accepted, and pumping resumes.
    let mut conn = common::accept(&listener, "third connection").await;
    let replayed = conn.read_frame("replayed initialize (2nd)").await;
    common::assert_replayed_initialize(&replayed, 1);
    conn.reply_ok(&replayed["id"]).await;
    let reconnected = read_stderr_line(&mut stderr).await;
    assert!(
        reconnected.contains("reconnected after 2 attempts"),
        "one outage, two attempts: {reconnected:?}"
    );

    common::write_stdin(&mut stdin, &common::tools_call_line(2)).await;
    let call = conn.read_frame("tools/call after resume").await;
    assert_eq!(call["id"], serde_json::json!(2));
    conn.reply_ok(&call["id"]).await;
    let resp = common::read_stdout(&mut stdout, "tools/call response").await;
    assert_eq!(resp["id"], serde_json::json!(2));
    common::assert_alive(&mut child);

    drop(stdin);
    drop(conn);
    let _ = timeout(TEST_BUDGET, child.wait()).await;
}

/// D2: stdin closes while the shim is between connections (kernel down,
/// nothing listening). The shim must notice and exit 0 instead of
/// reconnecting for the rest of the budget as an orphan.
#[tokio::test]
async fn stdin_eof_during_reconnect_exits_0() {
    let (_tmp, socket_path) = common::socket();
    let listener = common::listen(&socket_path);
    let mut child = common::spawn_shim(&socket_path);
    let mut stdin = child.stdin.take().expect("stdin piped");
    let mut stdout = BufReader::new(child.stdout.take().expect("stdout piped"));
    let mut stderr = BufReader::new(child.stderr.take().expect("stderr piped"));

    let mut conn = common::accept(&listener, "first connection").await;
    common::write_stdin(&mut stdin, &common::initialize_line(1)).await;
    let init = conn.read_frame("initialize").await;
    conn.reply_ok(&init["id"]).await;
    let resp = common::read_stdout(&mut stdout, "initialize response").await;
    assert_eq!(resp["id"], serde_json::json!(1));

    // Kernel gone for good: stream and listener dropped, socket file
    // removed, nothing re-bound.
    drop(conn);
    drop(listener);
    let _ = std::fs::remove_file(&socket_path);
    let lost = read_stderr_line(&mut stderr).await;
    assert!(lost.contains("reconnecting"), "{lost:?}");

    // codex goes away while the shim is in the reconnect loop.
    drop(stdin);
    let code = common::wait_exit_code(&mut child, "after stdin EOF during reconnect").await;
    assert_eq!(code, 0);
    let closed = read_stderr_line(&mut stderr).await;
    assert!(closed.contains("stdin closed"), "{closed:?}");
}

/// D2: stdin EOF first, then the kernel hangs up — the clean order.
/// Exit 0 and no reconnect (the listener is still bound and would
/// accept one).
#[tokio::test]
async fn stdin_eof_then_kernel_hangup_exits_0_without_reconnect() {
    let (_tmp, socket_path) = common::socket();
    let listener = common::listen(&socket_path);
    let mut child = common::spawn_shim(&socket_path);
    let mut stdin = child.stdin.take().expect("stdin piped");
    let mut stdout = BufReader::new(child.stdout.take().expect("stdout piped"));

    let mut conn = common::accept(&listener, "first connection").await;
    common::write_stdin(&mut stdin, &common::initialize_line(1)).await;
    let init = conn.read_frame("initialize").await;
    conn.reply_ok(&init["id"]).await;
    let resp = common::read_stdout(&mut stdout, "initialize response").await;
    assert_eq!(resp["id"], serde_json::json!(1));

    // codex closes stdin; the shim half-closes towards the kernel, so
    // the stub reads EOF — that is how we know the shim saw it first.
    drop(stdin);
    assert!(
        conn.read_frame_or_eof("EOF after stdin close")
            .await
            .is_none(),
        "shim must half-close the socket once stdin is closed"
    );
    // Kernel hangs up.
    drop(conn);
    let code = tokio::select! {
        code = common::wait_exit_code(&mut child, "after kernel hang-up") => code,
        _ = listener.accept() => panic!("shim reconnected after stdin EOF"),
    };
    assert_eq!(code, 0);
}

/// D6: the initial connection also retries — a listener that binds
/// 500 ms after spawn still gets the shim (the old shim exited 3 at
/// once).
#[tokio::test]
async fn listener_bound_after_spawn_still_gets_connected() {
    let (_tmp, socket_path) = common::socket();
    let mut child = common::spawn_shim(&socket_path);
    let mut stdin = child.stdin.take().expect("stdin piped");
    let mut stdout = BufReader::new(child.stdout.take().expect("stdout piped"));

    tokio::time::sleep(Duration::from_millis(500)).await;
    common::assert_alive(&mut child);
    let listener = common::listen(&socket_path);

    let mut conn = common::accept(&listener, "late-bound listener").await;
    common::write_stdin(&mut stdin, &common::initialize_line(1)).await;
    let init = conn.read_frame("initialize").await;
    assert_eq!(
        init["params"]["_meta"]["dev.neige/auth"]["token"],
        serde_json::json!(TOKEN)
    );
    conn.reply_ok(&init["id"]).await;
    let resp = common::read_stdout(&mut stdout, "initialize response").await;
    assert_eq!(resp["id"], serde_json::json!(1));

    drop(stdin);
    drop(conn);
    let _ = timeout(TEST_BUDGET, child.wait()).await;
}
