//! Process-level regression tests for the Binance plugin, driven through a
//! fake kernel over the real stdio channel.
//!
//! Both defects these pin were found by review, not by the plugin's own unit
//! tests, and neither is reachable from a pure function: one is about *who
//! runs on which thread*, the other about a decision made across two
//! callbacks. So the binary is spawned for real and answered by hand.
//!
//! No network. The endpoint is a port nothing listens on, which makes every
//! price lookup fail fast — the exact condition the second test is about, and
//! harmless to the first, which only cares about latency.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{Receiver, RecvTimeoutError, channel};
use std::time::Duration;

use serde_json::{Value, json};

/// A port on loopback with nothing behind it: `ureq` fails to connect
/// immediately rather than waiting out a DNS or TCP timeout.
const DEAD_ENDPOINT: &str = "http://127.0.0.1:1";

/// Generous enough that a slow machine cannot fail it, far below the plugin's
/// own 15s callback timeout — which is what a blocked reader would cost.
const REPLY_BUDGET: Duration = Duration::from_secs(5);

/// A four-line HTTP server that answers every request with one fixed price.
///
/// It exists so the *successful* path is exercised somewhere other than a
/// developer's machine with a working route to the internet: without it, CI
/// only ever sees the plugin fail to price, and every claim about what a good
/// tick publishes would rest on a hand-run.
fn price_server(price: &'static str) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind a loopback port");
    let port = listener.local_addr().expect("addr").port();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { return };
            // Read just enough to reach the end of the request head; the
            // plugin sends no body.
            let mut head = Vec::new();
            let mut byte = [0u8; 1];
            while stream.read_exact(&mut byte).is_ok() {
                head.push(byte[0]);
                if head.ends_with(b"\r\n\r\n") {
                    break;
                }
            }
            let body = format!("{{\"symbol\":\"BTCUSDT\",\"price\":\"{price}\"}}");
            let _ = write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len(),
            );
            let _ = stream.flush();
        }
    });
    format!("http://127.0.0.1:{port}")
}

struct FakeKernel {
    child: Child,
    stdin: ChildStdin,
    frames: Receiver<Value>,
}

impl FakeKernel {
    /// Spawn the plugin and complete the handshake, handing it `holdings` and
    /// an endpoint that cannot answer.
    fn boot(holdings: &str) -> Self {
        Self::boot_against(holdings, DEAD_ENDPOINT)
    }

    fn boot_against(holdings: &str, endpoint: &str) -> Self {
        let mut child = Command::new(env!("CARGO_BIN_EXE_binance"))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn the binance plugin");
        let stdin = child.stdin.take().expect("stdin");
        let stdout = child.stdout.take().expect("stdout");
        let (tx, frames) = channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                let Ok(line) = line else { return };
                if line.trim().is_empty() {
                    continue;
                }
                let Ok(frame) = serde_json::from_str::<Value>(&line) else {
                    continue;
                };
                if tx.send(frame).is_err() {
                    return;
                }
            }
        });
        let mut kernel = Self {
            child,
            stdin,
            frames,
        };
        kernel.send(json!({
            "jsonrpc": "2.0", "id": 1, "method": "initialize",
            "params": {
                "protocolVersion": "2025-11-25",
                "_meta": {
                    "dev.neige/auth": { "expected_echo": "TOKEN" },
                    "dev.neige/config": { "values": {
                        "track_id": "trk_test",
                        "holdings": holdings,
                        "quote": "USDT",
                        "poll_seconds": 3600,
                        "endpoint": endpoint,
                    } }
                }
            }
        }));
        let handshake = kernel.next_frame().expect("handshake reply");
        assert_eq!(
            handshake.pointer("/result/_meta/dev.neige~1auth/echoed_token"),
            Some(&json!("TOKEN")),
            "the plugin must echo the kernel's token: {handshake}"
        );
        kernel
    }

    fn send(&mut self, frame: Value) {
        writeln!(self.stdin, "{frame}").expect("write to plugin");
        self.stdin.flush().expect("flush");
    }

    fn next_frame(&mut self) -> Result<Value, RecvTimeoutError> {
        self.frames.recv_timeout(REPLY_BUDGET)
    }

    /// Answer one `neige.*` callback with a bland success, so the plugin can
    /// proceed. Returns the method that was answered.
    fn answer(&mut self, frame: &Value) -> String {
        let method = frame
            .get("method")
            .and_then(Value::as_str)
            .expect("a request")
            .to_string();
        let result = match method.as_str() {
            "neige.kv.get" => json!({ "value": Value::Null }),
            "neige.overlay.set" => json!({ "overlay_id": "ov", "updated_at": 1 }),
            _ => json!({}),
        };
        let id = frame.get("id").cloned().expect("request id");
        self.send(json!({ "jsonrpc": "2.0", "id": id, "result": result }));
        method
    }
}

impl Drop for FakeKernel {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// The reader thread must never be the thread doing the work.
///
/// A `tools/call` issues `neige.*` callbacks whose replies arrive on the same
/// stdin the plugin is reading. Handling the call on the read loop makes the
/// plugin wait out its own 15s callback timeout for a reply it is preventing
/// itself from reading — and every other request queues behind it. The
/// assertion is therefore latency: the reply must arrive inside a budget far
/// below that timeout.
#[test]
fn a_tool_call_is_answered_while_the_reader_keeps_reading() {
    let mut kernel = FakeKernel::boot("BTC:1");
    // The poll thread refreshes once at startup; answer whatever it sends
    // until it settles, so the tool call below starts from a quiet channel.
    let first = kernel.next_frame().expect("the startup refresh pushes");
    kernel.answer(&first);

    kernel.send(json!({
        "jsonrpc": "2.0", "id": 2, "method": "tools/call",
        "params": { "name": "binance.portfolio.refresh", "arguments": {} }
    }));

    // Everything the tool call asks of the kernel is answered promptly; the
    // test fails on the recv timeout if the plugin has stopped reading.
    let mut reply = None;
    for _ in 0..8 {
        let frame = kernel
            .next_frame()
            .expect("the plugin must keep talking while a tool call runs");
        if frame.get("method").is_none() {
            reply = Some(frame);
            break;
        }
        kernel.answer(&frame);
    }
    let reply = reply.expect("a tools/call reply within the budget");
    assert_eq!(reply.get("id"), Some(&json!(2)), "{reply}");
}

/// A tick that cannot price part of the portfolio must publish the holdings
/// table — which names the gap row by row — and no history point.
///
/// History is a claim about the portfolio's value over time. A total covering
/// a subset, plotted against totals covering the whole, draws a crash that
/// never happened, and the previous point's `change` column states it as a
/// number.
#[test]
fn a_tick_that_cannot_price_everything_writes_no_history_point() {
    let mut kernel = FakeKernel::boot("BTC:1");

    let mut methods = Vec::new();
    // Drive the startup refresh to completion. Every price lookup fails
    // (nothing listens on the endpoint), so a correct plugin pushes exactly
    // one overlay and stops.
    while let Ok(frame) = kernel.next_frame() {
        if frame.get("method").is_some() {
            let method = kernel.answer(&frame);
            let kind = frame
                .pointer("/params/kind")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            methods.push(if kind.is_empty() {
                method
            } else {
                format!("{method}:{kind}")
            });
        }
    }

    assert_eq!(
        methods,
        vec!["neige.overlay.set:portfolio.holdings"],
        "an unpriceable tick publishes the holdings table and nothing else — \
         no history read, no history write, no history overlay"
    );
}

/// …and the tool call says so, rather than reporting a refresh that did not
/// happen. A caller's next act is to read a number off the table.
#[test]
fn a_partial_refresh_is_reported_as_an_error_not_as_success() {
    let mut kernel = FakeKernel::boot("BTC:1");
    let first = kernel.next_frame().expect("the startup refresh pushes");
    kernel.answer(&first);

    kernel.send(json!({
        "jsonrpc": "2.0", "id": 7, "method": "tools/call",
        "params": { "name": "binance.portfolio.refresh", "arguments": {} }
    }));
    let mut reply = None;
    for _ in 0..8 {
        let frame = kernel.next_frame().expect("a reply within the budget");
        if frame.get("method").is_none() {
            reply = Some(frame);
            break;
        }
        kernel.answer(&frame);
    }
    let reply = reply.expect("a tools/call reply");
    assert_eq!(
        reply.pointer("/result/isError"),
        Some(&json!(true)),
        "a refresh that could not price the portfolio is not a success: {reply}"
    );
    let text = reply
        .pointer("/result/content/0/text")
        .and_then(Value::as_str)
        .unwrap_or_default();
    assert!(
        text.contains("history point was skipped"),
        "the reason must name what was skipped, got: {text}"
    );
}

/// The whole cycle, end to end, against a price the test controls: holdings
/// table, history read, history write, history table — in that order, with
/// the total the arithmetic says it should be.
#[test]
fn a_priced_tick_publishes_both_tables_and_persists_the_point() {
    let endpoint = price_server("2.5");
    let mut kernel = FakeKernel::boot_against("BTC:4", &endpoint);

    let mut steps = Vec::new();
    let mut stored: Option<Value> = None;
    let mut holdings_total = None;
    for _ in 0..6 {
        let Ok(frame) = kernel.next_frame() else {
            break;
        };
        let Some(method) = frame.get("method").and_then(Value::as_str) else {
            continue;
        };
        let kind = frame
            .pointer("/params/kind")
            .and_then(Value::as_str)
            .unwrap_or("");
        steps.push(if kind.is_empty() {
            method.to_string()
        } else {
            format!("{method}:{kind}")
        });
        if kind == "portfolio.holdings" {
            let rows = frame
                .pointer("/params/payload/rows")
                .and_then(Value::as_array)
                .expect("rows")
                .clone();
            holdings_total = rows.last().and_then(|row| row["value"].as_f64());
        }
        if method == "neige.kv.set" {
            stored = frame.pointer("/params/value").cloned();
        }
        kernel.answer(&frame);
        if steps.len() >= 4 {
            break;
        }
    }

    assert_eq!(
        steps,
        vec![
            "neige.overlay.set:portfolio.holdings",
            "neige.kv.get",
            "neige.kv.set",
            "neige.overlay.set:portfolio.history",
        ],
        "the history point is persisted BEFORE it is published — publishing \
         first would put a point on screen that the next tick deletes"
    );
    assert_eq!(holdings_total, Some(10.0), "4 BTC at 2.5 is 10");
    let stored = stored.expect("a stored series");
    let points = stored.as_array().expect("an array");
    assert_eq!(points.len(), 1);
    assert_eq!(points[0]["total"], json!(10.0));
}

/// A history point that could not be stored is not published either.
///
/// Publishing it would show a point that the next tick — which reloads from
/// the store — silently drops. A point that vanishes reads as data loss, not
/// as the failed write it was.
#[test]
fn a_history_point_that_cannot_be_stored_is_not_published() {
    let endpoint = price_server("2.5");
    let mut kernel = FakeKernel::boot_against("BTC:4", &endpoint);

    let mut methods = Vec::new();
    for _ in 0..6 {
        let Ok(frame) = kernel.next_frame() else {
            break;
        };
        let Some(method) = frame.get("method").and_then(Value::as_str) else {
            continue;
        };
        let kind = frame
            .pointer("/params/kind")
            .and_then(Value::as_str)
            .unwrap_or("");
        methods.push(if kind.is_empty() {
            method.to_string()
        } else {
            format!("{method}:{kind}")
        });
        let id = frame.get("id").cloned().expect("id");
        if method == "neige.kv.set" {
            kernel.send(json!({
                "jsonrpc": "2.0", "id": id,
                "error": { "code": -32000, "message": "quota exceeded" }
            }));
        } else {
            kernel.answer(&frame);
        }
    }

    assert_eq!(
        methods,
        vec![
            "neige.overlay.set:portfolio.holdings",
            "neige.kv.get",
            "neige.kv.set",
        ],
        "the refused write must end the tick — no history overlay follows it"
    );
}
