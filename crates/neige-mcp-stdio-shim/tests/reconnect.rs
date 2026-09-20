//! Reconnect behaviour of `neige-mcp-stdio-shim`. Every wait is bounded by
//! `common::TEST_BUDGET`; the tests that run a budget down for real are in `budget.rs`.

#![cfg(unix)]

mod common;

use std::time::Duration;

use tokio::io::BufReader;
use tokio::time::timeout;

use common::{TEST_BUDGET, TOKEN, handshake_then_shut_read, read_stderr_line};

#[tokio::test]
async fn write_failure_resends_the_frame_after_reconnect() {
    let (_tmp, socket_path) = common::socket();
    let listener = common::listen(&socket_path);
    let mut child = common::spawn_shim(&socket_path);
    let mut stdin = child.stdin.take().expect("stdin piped");
    let mut stdout = BufReader::new(child.stdout.take().expect("stdout piped"));
    let mut stderr = BufReader::new(child.stderr.take().expect("stderr piped"));

    let old_stream = handshake_then_shut_read(&listener, &mut stdin, &mut stdout).await;

    common::write_stdin(&mut stdin, &common::tools_call_line(2)).await;
    let lost = read_stderr_line(&mut stderr).await;
    assert!(
        lost.contains("connection to kernel lost (0 unanswered requests failed)"),
        "the failed write must not count as delivered: {lost:?}"
    );

    let mut conn = common::accept(&listener, "reconnect after write failure").await;
    drop(old_stream);
    let replayed = conn.read_frame("replayed initialize").await;
    common::assert_replayed_initialize(&replayed, 1);
    conn.reply_ok(&replayed["id"]).await;
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

    common::write_stdin(&mut stdin, &common::tools_call_line(2)).await;
    let call = conn.read_frame("tools/call").await;
    assert_eq!(call["id"], serde_json::json!(2));
    drop(conn);
    drop(listener);

    let err = common::read_stdout(&mut stdout, "synthesized error").await;
    assert_eq!(err["id"], serde_json::json!(2), "got {err}");
    assert_eq!(err["error"]["code"], serde_json::json!(-32000), "got {err}");
    let message = err["error"]["message"].as_str().expect("message string");
    assert!(
        message.contains("outcome unknown"),
        "message must say the outcome is unknown: {message:?}"
    );
    assert!(err.get("result").is_none());

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

    let err = common::read_stdout(&mut stdout, "synthesized error for held request").await;
    assert_eq!(err["id"], serde_json::json!(2), "got {err}");
    assert_eq!(err["error"]["code"], serde_json::json!(-32000), "got {err}");
    let rejected = read_stderr_line(&mut stderr).await;
    assert!(rejected.contains("rejected"), "{rejected:?}");

    let code = tokio::select! {
        code = common::wait_exit_code(&mut child, "after rejection") => code,
        _ = listener.accept() => panic!("shim reconnected after a terminal initialize rejection"),
    };
    assert_eq!(code, 4);
    drop(stdin);
}

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

    let mut conn = common::accept(&listener, "second connection").await;
    let replayed = conn.read_frame("replayed initialize (1st)").await;
    common::assert_replayed_initialize(&replayed, 1);
    conn.reply_error(&replayed["id"], -32603, "repo lookup failed")
        .await;
    drop(conn);
    let retrying = read_stderr_line(&mut stderr).await;
    assert!(
        retrying.contains("initialize answered -32603; retrying under the outage budget"),
        "the -32603 answer is reported as what it is, not as a loss: {retrying:?}"
    );

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

    drop(conn);
    drop(listener);
    let _ = std::fs::remove_file(&socket_path);
    let lost = read_stderr_line(&mut stderr).await;
    assert!(lost.contains("reconnecting"), "{lost:?}");

    drop(stdin);
    let code = common::wait_exit_code(&mut child, "after stdin EOF during reconnect").await;
    assert_eq!(code, 0);
    let closed = read_stderr_line(&mut stderr).await;
    assert!(closed.contains("stdin closed"), "{closed:?}");
}

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

    // The stub reading EOF is how we know the shim saw stdin close first.
    drop(stdin);
    assert!(
        conn.read_frame_or_eof("EOF after stdin close")
            .await
            .is_none(),
        "shim must half-close the socket once stdin is closed"
    );
    drop(conn);
    let code = tokio::select! {
        code = common::wait_exit_code(&mut child, "after kernel hang-up") => code,
        _ = listener.accept() => panic!("shim reconnected after stdin EOF"),
    };
    assert_eq!(code, 0);
}

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

#[tokio::test]
async fn reconnect_without_a_cached_initialize_forwards_directly() {
    let (_tmp, socket_path) = common::socket();
    let listener = common::listen(&socket_path);
    let mut child = common::spawn_shim(&socket_path);
    let mut stdin = child.stdin.take().expect("stdin piped");
    let mut stdout = BufReader::new(child.stdout.take().expect("stdout piped"));
    let mut stderr = BufReader::new(child.stderr.take().expect("stderr piped"));

    let mut conn = common::accept(&listener, "first connection").await;
    common::write_stdin(&mut stdin, &common::request_line(1, "tools/list")).await;
    let first = conn.read_frame("tools/list").await;
    assert_eq!(first["method"], "tools/list", "got {first}");
    assert!(
        first["params"].get("_meta").is_none(),
        "only an initialize gets the token: {first}"
    );
    conn.reply_ok(&first["id"]).await;
    let resp = common::read_stdout(&mut stdout, "tools/list response").await;
    assert_eq!(resp["id"], serde_json::json!(1));

    drop(conn);
    let lost = read_stderr_line(&mut stderr).await;
    assert!(
        lost.contains("connection to kernel lost (0 unanswered requests failed)"),
        "{lost:?}"
    );

    common::write_stdin(&mut stdin, &common::tools_call_line(2)).await;
    let mut conn = common::accept(&listener, "reconnect").await;
    let reconnected = read_stderr_line(&mut stderr).await;
    assert!(
        reconnected.contains("reconnected after 1 attempts"),
        "{reconnected:?}"
    );
    let call = conn.read_frame("first frame on the new connection").await;
    assert_eq!(
        call["method"], "tools/call",
        "nothing to replay, the request itself comes first: {call}"
    );
    assert_eq!(call["id"], serde_json::json!(2));
    conn.reply_ok(&call["id"]).await;
    let resp = common::read_stdout(&mut stdout, "tools/call response").await;
    assert_eq!(resp["id"], serde_json::json!(2));
    common::assert_alive(&mut child);

    drop(stdin);
    drop(conn);
    let _ = timeout(TEST_BUDGET, child.wait()).await;
}

#[tokio::test]
async fn initialize_in_flight_at_the_loss_is_replayed_and_its_response_forwarded() {
    let (_tmp, socket_path) = common::socket();
    let listener = common::listen(&socket_path);
    let mut child = common::spawn_shim(&socket_path);
    let mut stdin = child.stdin.take().expect("stdin piped");
    let mut stdout = BufReader::new(child.stdout.take().expect("stdout piped"));
    let mut stderr = BufReader::new(child.stderr.take().expect("stderr piped"));

    let mut conn = common::accept(&listener, "first connection").await;
    common::write_stdin(&mut stdin, &common::initialize_line(1)).await;
    let init = conn.read_frame("initialize").await;
    common::assert_replayed_initialize(&init, 1);
    drop(conn);
    drop(listener);
    let lost = read_stderr_line(&mut stderr).await;
    assert!(
        lost.contains("connection to kernel lost (0 unanswered requests failed)"),
        "the initialize is not an outstanding request: {lost:?}"
    );

    let listener = common::rebind(&socket_path);
    let mut conn = common::accept(&listener, "reconnect").await;
    let replayed = conn.read_frame("replayed initialize").await;
    common::assert_replayed_initialize(&replayed, 1);
    conn.reply_ok(&replayed["id"]).await;
    let reconnected = read_stderr_line(&mut stderr).await;
    assert!(reconnected.contains("reconnected after"), "{reconnected:?}");
    let resp = common::read_stdout(&mut stdout, "forwarded initialize response").await;
    assert_eq!(resp["id"], serde_json::json!(1), "got {resp}");
    assert!(resp.get("result").is_some(), "the kernel's answer: {resp}");

    common::write_stdin(&mut stdin, &common::tools_call_line(2)).await;
    let call = conn.read_frame("tools/call after the replay").await;
    assert_eq!(call["id"], serde_json::json!(2));
    conn.reply_ok(&call["id"]).await;
    let resp = common::read_stdout(&mut stdout, "tools/call response").await;
    assert_eq!(resp["id"], serde_json::json!(2));
    common::assert_alive(&mut child);

    drop(stdin);
    drop(conn);
    let _ = timeout(TEST_BUDGET, child.wait()).await;
}

#[tokio::test]
async fn original_initialize_internal_error_is_retried_on_a_fresh_connection() {
    let (_tmp, socket_path) = common::socket();
    let listener = common::listen(&socket_path);
    let mut child = common::spawn_shim(&socket_path);
    let mut stdin = child.stdin.take().expect("stdin piped");
    let mut stdout = BufReader::new(child.stdout.take().expect("stdout piped"));
    let mut stderr = BufReader::new(child.stderr.take().expect("stderr piped"));

    let mut conn = common::accept(&listener, "first connection").await;
    common::write_stdin(&mut stdin, &common::initialize_line(1)).await;
    let init = conn.read_frame("initialize").await;
    common::assert_replayed_initialize(&init, 1);
    conn.reply_error(&init["id"], -32603, "repo lookup failed")
        .await;
    drop(conn);
    let retrying = read_stderr_line(&mut stderr).await;
    assert!(
        retrying.contains("initialize answered -32603; retrying under the outage budget"),
        "{retrying:?}"
    );

    let mut conn = common::accept(&listener, "second connection").await;
    let replayed = conn.read_frame("initialize on the fresh connection").await;
    common::assert_replayed_initialize(&replayed, 1);
    conn.reply_ok(&replayed["id"]).await;
    let reconnected = read_stderr_line(&mut stderr).await;
    assert!(
        reconnected.contains("reconnected after 1 attempts"),
        "{reconnected:?}"
    );
    let resp = common::read_stdout(&mut stdout, "the one initialize response").await;
    assert_eq!(resp["id"], serde_json::json!(1), "got {resp}");
    assert!(
        resp.get("result").is_some() && resp.get("error").is_none(),
        "codex must see the accepted handshake, not the -32603: {resp}"
    );

    common::write_stdin(&mut stdin, &common::tools_call_line(2)).await;
    let call = conn.read_frame("tools/call").await;
    assert_eq!(call["id"], serde_json::json!(2));
    conn.reply_ok(&call["id"]).await;
    let resp = common::read_stdout(&mut stdout, "tools/call response").await;
    assert_eq!(resp["id"], serde_json::json!(2), "got {resp}");
    common::assert_alive(&mut child);

    drop(stdin);
    drop(conn);
    let _ = timeout(TEST_BUDGET, child.wait()).await;
}

#[tokio::test]
async fn hangup_during_replay_handshake_reconnects_under_the_same_budget() {
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

    let mut conn = common::accept(&listener, "second connection").await;
    let replayed = conn.read_frame("replayed initialize (1st)").await;
    common::assert_replayed_initialize(&replayed, 1);
    drop(conn);

    let mut conn = common::accept(&listener, "third connection").await;
    let replayed = conn.read_frame("replayed initialize (2nd)").await;
    common::assert_replayed_initialize(&replayed, 1);
    conn.reply_ok(&replayed["id"]).await;
    let next = read_stderr_line(&mut stderr).await;
    assert!(
        next.contains("reconnected after 2 attempts"),
        "one outage, two attempts, and no second lost line: {next:?}"
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
