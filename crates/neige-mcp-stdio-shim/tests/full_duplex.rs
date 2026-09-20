//! The pump must keep reading the socket while a request write is in flight: the
//! kernel is serial, so a pump that stops reading while it writes deadlocks once a
//! request and a response both exceed the socket buffers.

#![cfg(unix)]

mod common;

use std::time::Duration;

use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::unix::{OwnedReadHalf, OwnedWriteHalf};
use tokio::time::timeout;

/// Payload per frame; every frame is one JSON line of a little over this.
const PAYLOAD: usize = 4 * 1024 * 1024;
/// A deadlocked pump never finishes; this bounds the whole scenario.
const DEADLOCK_BUDGET: Duration = Duration::from_secs(15);

fn big_tools_call(id: i64) -> String {
    format!(
        "{{\"jsonrpc\":\"2.0\",\"id\":{id},\"method\":\"tools/call\",\"params\":{{\"name\":\"calm.report.write\",\"arguments\":{{\"body\":\"{}\"}}}}}}\n",
        "x".repeat(PAYLOAD)
    )
}

fn big_response(id: &serde_json::Value) -> String {
    format!(
        "{{\"jsonrpc\":\"2.0\",\"id\":{id},\"result\":{{\"body\":\"{}\"}}}}\n",
        "y".repeat(PAYLOAD)
    )
}

/// One accumulator so the test can tell a complete line from bytes of the next
/// request having started to arrive.
struct StubReader {
    rd: OwnedReadHalf,
    acc: Vec<u8>,
}

impl StubReader {
    async fn read_chunk(&mut self) {
        let mut buf = [0u8; 64 * 1024];
        let n = self.rd.read(&mut buf).await.expect("stub read ok");
        assert!(n > 0, "stub saw EOF from the shim");
        self.acc.extend_from_slice(&buf[..n]);
    }

    /// Read one complete request line, parsed as JSON.
    async fn read_frame(&mut self) -> serde_json::Value {
        loop {
            if let Some(i) = self.acc.iter().position(|&b| b == b'\n') {
                let line: Vec<u8> = self.acc.drain(..=i).collect();
                return serde_json::from_slice(&line).expect("stub received JSON");
            }
            self.read_chunk().await;
        }
    }

    /// Return once at least one byte of the next frame has arrived.
    async fn wait_for_next_frame_bytes(&mut self) {
        while self.acc.is_empty() {
            self.read_chunk().await;
        }
    }
}

async fn stub_reply(wr: &mut OwnedWriteHalf, id: &serde_json::Value) {
    wr.write_all(big_response(id).as_bytes())
        .await
        .expect("stub response write ok");
}

#[tokio::test]
async fn large_pipelined_requests_do_not_deadlock_against_a_serial_kernel() {
    let (_tmp, socket_path) = common::socket();
    let listener = common::listen(&socket_path);
    let mut child = common::spawn_shim(&socket_path);
    let mut stdin = child.stdin.take().expect("stdin piped");
    let mut stdout = BufReader::new(child.stdout.take().expect("stdout piped"));

    let (stream, _addr) = timeout(common::TEST_BUDGET, listener.accept())
        .await
        .expect("shim connected within budget")
        .expect("accept ok");
    let (rd, mut wr) = stream.into_split();
    let mut stub = StubReader {
        rd,
        acc: Vec::new(),
    };

    common::write_stdin(&mut stdin, &common::initialize_line(1)).await;
    let init = stub.read_frame().await;
    common::assert_replayed_initialize(&init, 1);
    wr.write_all(b"{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{}}\n")
        .await
        .expect("stub reply ok");
    let resp = common::read_stdout(&mut stdout, "initialize response").await;
    assert_eq!(resp["id"], serde_json::json!(1));

    // codex reads responses regardless of what it is writing.
    let writer = tokio::spawn(async move {
        stdin
            .write_all(big_tools_call(2).as_bytes())
            .await
            .expect("stdin write id 2");
        stdin
            .write_all(big_tools_call(3).as_bytes())
            .await
            .expect("stdin write id 3");
        stdin.flush().await.expect("stdin flush");
        stdin
    });
    let drainer = tokio::spawn(async move {
        let mut ids = Vec::new();
        for _ in 0..2 {
            let mut line = String::new();
            let n = stdout.read_line(&mut line).await.expect("stdout read ok");
            assert!(n > 0, "shim closed stdout after {ids:?}");
            let v: serde_json::Value =
                serde_json::from_str(line.trim_end()).expect("stdout frame is JSON");
            assert!(v.get("result").is_some(), "expected a result, got {v}");
            ids.push(v["id"].clone());
        }
        ids
    });

    let scenario = async {
        let call = stub.read_frame().await;
        assert_eq!(call["method"], "tools/call", "got {call}");
        let id = call["id"].clone();
        assert_eq!(id, serde_json::json!(2));
        // Wait for the next request's first bytes so the shim is provably inside the
        // id-3 write before the response goes out.
        stub.wait_for_next_frame_bytes().await;
        stub_reply(&mut wr, &id).await;
        let call = stub.read_frame().await;
        assert_eq!(call["method"], "tools/call", "got {call}");
        let id = call["id"].clone();
        assert_eq!(id, serde_json::json!(3));
        stub_reply(&mut wr, &id).await;
        let ids = drainer.await.expect("drainer task");
        assert_eq!(ids, vec![serde_json::json!(2), serde_json::json!(3)]);
    };
    timeout(DEADLOCK_BUDGET, scenario)
        .await
        .expect("deadlock: the pump stopped reading the socket while writing a request");

    let stdin = writer.await.expect("writer task");
    common::assert_alive(&mut child);
    drop(stdin);
    drop(wr);
    let _ = timeout(common::TEST_BUDGET, child.wait()).await;
}
