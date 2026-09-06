//! Process-level tests for the market-data plugin, driven through a fake
//! kernel over the real stdio channel.
//!
//! What these pin is not reachable from a pure function: which thread runs a
//! tool call, which Track a call is attributed to, and the order of the
//! callbacks a refresh makes. So the binary is spawned for real, and the fake
//! kernel keeps actual KV state — a plugin whose `kv.set` went nowhere would
//! pass a stubbed-out harness while losing every holding in production.
//!
//! Network: only where a test needs a price. `DEAD_ENDPOINT` is a port nothing
//! listens on, and the quote asset (`USDT`) prices at 1.0 without any venue,
//! which is what lets a *partially* priceable portfolio be built offline.

use std::collections::HashMap;
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

/// How long `drain` waits for the plugin to go quiet. Much shorter than
/// [`REPLY_BUDGET`], because it is not proving anything by itself: a callback
/// that arrives after this window still fails the test, in `is_responsive`,
/// which refuses any `neige.*` request reaching it before the pong.
const QUIET_WINDOW: Duration = Duration::from_millis(1_500);

const TRACK: &str = "trk_caller";
const OTHER_TRACK: &str = "trk_someone_else";

/// A four-line HTTP server that answers every request with one fixed price.
///
/// It exists so the *successful* path is exercised somewhere other than a
/// developer's machine with a working route to the internet: without it, CI
/// only ever sees the plugin fail to price.
fn price_server(price: &'static str) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind a loopback port");
    let port = listener.local_addr().expect("addr").port();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { return };
            // A request that stops mid-head must not park this thread forever
            // and starve every later request.
            let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
            let mut head = Vec::new();
            let mut byte = [0u8; 1];
            while stream.read_exact(&mut byte).is_ok() {
                head.push(byte[0]);
                if head.ends_with(b"\r\n\r\n") {
                    break;
                }
            }
            let body = format!("{{\"symbol\":\"X\",\"price\":\"{price}\"}}");
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
    /// Real per-plugin KV, because the plugin's correctness depends on reading
    /// back what it wrote.
    kv: HashMap<String, Value>,
    /// Every overlay push seen, as `(kind, payload)`, in order.
    pushes: Vec<(String, Value)>,
    /// Every `neige.*` method seen, in order — including the ones that carry
    /// no overlay, which is what absence assertions are made of.
    methods: Vec<String>,
    /// When set, `neige.kv.set` is answered with an error.
    refuse_kv_set: bool,
}

impl FakeKernel {
    fn boot(endpoint: &str) -> Self {
        let mut child = Command::new(env!("CARGO_BIN_EXE_market"))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn the market plugin");
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
            kv: HashMap::new(),
            pushes: Vec::new(),
            methods: Vec::new(),
            refuse_kv_set: false,
        };
        kernel.send(json!({
            "jsonrpc": "2.0", "id": 1, "method": "initialize",
            "params": {
                "protocolVersion": "2025-11-25",
                "_meta": {
                    "dev.neige/auth": { "expected_echo": "TOKEN" },
                    "dev.neige/config": { "values": {
                        // Long enough that the poll thread never fires during
                        // a test: every refresh these tests observe is one a
                        // tool call caused.
                        "poll_seconds": 3600,
                        "quote": "USDT",
                        "binance_endpoint": endpoint,
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
        // The startup refresh finds no portfolios and asks only for the list.
        kernel.drain();
        kernel
    }

    fn send(&mut self, frame: Value) {
        writeln!(self.stdin, "{frame}").expect("write to plugin");
        self.stdin.flush().expect("flush");
    }

    fn next_frame(&mut self) -> Result<Value, RecvTimeoutError> {
        self.frames.recv_timeout(REPLY_BUDGET)
    }

    /// Service `neige.*` requests until the plugin goes quiet, recording what
    /// it asked for. Returns any non-request frame (i.e. a `tools/call` reply)
    /// that arrived.
    fn drain(&mut self) -> Option<Value> {
        let mut reply = None;
        while let Ok(frame) = self.frames.recv_timeout(QUIET_WINDOW) {
            if frame.get("method").is_none() {
                reply = Some(frame);
                continue;
            }
            self.service(&frame);
        }
        reply
    }

    /// Same, but stops as soon as a `tools/call` reply arrives — for the
    /// latency assertion, where waiting out the quiet period would defeat the
    /// point.
    fn drain_until_reply(&mut self) -> Value {
        for _ in 0..16 {
            let frame = self
                .next_frame()
                .expect("the plugin must keep talking while a tool call runs");
            if frame.get("method").is_none() {
                return frame;
            }
            self.service(&frame);
        }
        panic!("no tools/call reply within {} frames", 16);
    }

    fn service(&mut self, frame: &Value) {
        let method = frame
            .get("method")
            .and_then(Value::as_str)
            .expect("a request")
            .to_string();
        let id = frame.get("id").cloned().expect("request id");
        let params = frame.get("params").cloned().unwrap_or(Value::Null);
        self.methods.push(method.clone());
        let result = match method.as_str() {
            "neige.kv.get" => {
                let key = params["key"].as_str().unwrap_or_default();
                json!({ "value": self.kv.get(key).cloned().unwrap_or(Value::Null) })
            }
            "neige.kv.set" => {
                if self.refuse_kv_set {
                    self.send(json!({
                        "jsonrpc": "2.0", "id": id,
                        "error": { "code": -32000, "message": "quota exceeded" }
                    }));
                    return;
                }
                let key = params["key"].as_str().unwrap_or_default().to_string();
                self.kv.insert(key, params["value"].clone());
                json!({})
            }
            "neige.kv.list" => {
                let prefix = params["prefix"].as_str().unwrap_or_default();
                let entries: Vec<Value> = self
                    .kv
                    .iter()
                    .filter(|(key, _)| key.starts_with(prefix))
                    .map(|(key, value)| json!({ "key": key, "value": value }))
                    .collect();
                json!({ "entries": entries })
            }
            "neige.overlay.set" => {
                let kind = params["kind"].as_str().unwrap_or_default().to_string();
                self.pushes.push((
                    format!("{}@{}", kind, params["entity_id"].as_str().unwrap_or("")),
                    params["payload"].clone(),
                ));
                json!({ "overlay_id": "ov", "updated_at": 1 })
            }
            _ => json!({}),
        };
        self.send(json!({ "jsonrpc": "2.0", "id": id, "result": result }));
    }

    /// Call a tool the way the kernel does: the Track rides in `_meta`, and
    /// `arguments` carries only the tool's own parameters.
    fn call_tool(&mut self, id: u64, name: &str, arguments: Value, track: Option<&str>) -> Value {
        let mut params = json!({ "name": name, "arguments": arguments });
        if let Some(track) = track {
            params["_meta"] = json!({ "dev.neige/track": { "id": track } });
        }
        self.send(json!({ "jsonrpc": "2.0", "id": id, "method": "tools/call", "params": params }));
        self.drain_until_reply()
    }

    /// Does the plugin still answer? Used after an *absence* assertion: an
    /// empty channel proves nothing on its own, because a plugin that crashed
    /// or wedged produces the same silence as one that correctly had nothing
    /// left to say. A `neige.*` request arriving before the pong is a callback
    /// the absence assertion just declared would not happen.
    fn is_responsive(&mut self) -> bool {
        self.send(json!({ "jsonrpc": "2.0", "id": 9_999, "method": "ping" }));
        while let Ok(frame) = self.next_frame() {
            if frame.get("id") == Some(&json!(9_999)) {
                return true;
            }
            assert!(
                frame.get("method").is_none(),
                "a late callback arrived after the tick was declared over: {frame}"
            );
        }
        false
    }

    fn kinds_pushed(&self) -> Vec<&str> {
        self.pushes.iter().map(|(kind, _)| kind.as_str()).collect()
    }
}

impl Drop for FakeKernel {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn text_of(reply: &Value) -> String {
    reply
        .pointer("/result/content/0/text")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

/// Recording a holding prices it immediately and publishes to the Track the
/// CALL came from — the whole point of the tool-driven flow: someone who has
/// just said what they hold is looking at the report now, not in `poll_seconds`.
#[test]
fn setting_a_holding_prices_it_now_and_publishes_to_the_callers_track() {
    let endpoint = price_server("2.5");
    let mut kernel = FakeKernel::boot(&endpoint);

    let reply = kernel.call_tool(
        2,
        "market.holdings.set",
        json!({ "asset": "BTC", "quantity": 4 }),
        Some(TRACK),
    );
    kernel.drain();

    assert_ne!(
        reply.pointer("/result/isError"),
        Some(&json!(true)),
        "{reply:#?}"
    );
    assert_eq!(
        kernel.kinds_pushed(),
        vec![
            format!("portfolio.holdings@{TRACK}"),
            format!("portfolio.history@{TRACK}"),
        ],
        "both tables, both on the caller's Track"
    );
    let total = kernel.pushes[0]
        .1
        .pointer("/rows")
        .and_then(Value::as_array)
        .and_then(|rows| rows.last())
        .and_then(|row| row["value"].as_f64());
    assert_eq!(total, Some(10.0), "4 BTC at 2.5 is 10");
    assert_eq!(
        kernel.kv.get("holdings/trk_caller"),
        Some(&json!([{ "asset": "BTC", "quantity": 4.0 }])),
        "the holding is stored under the caller's Track"
    );
}

/// A call with no Track is refused, not defaulted. Acting on some other
/// Track's portfolio is the failure the `_meta` namespace exists to prevent.
#[test]
fn a_call_without_a_track_is_refused_rather_than_guessed() {
    let mut kernel = FakeKernel::boot(DEAD_ENDPOINT);
    let reply = kernel.call_tool(
        2,
        "market.holdings.set",
        json!({ "asset": "BTC", "quantity": 1 }),
        None,
    );
    assert_eq!(reply.pointer("/result/isError"), Some(&json!(true)));
    assert!(text_of(&reply).contains("carries no Track"), "{reply:#?}");
    kernel.drain();
    assert!(
        kernel.pushes.is_empty() && kernel.kv.is_empty(),
        "nothing may be written or published: {:?}",
        kernel.methods
    );
    assert!(kernel.is_responsive());
}

/// Two Tracks keep two portfolios, and a refresh prices each against its own.
#[test]
fn holdings_are_per_track() {
    let endpoint = price_server("2");
    let mut kernel = FakeKernel::boot(&endpoint);
    kernel.call_tool(
        2,
        "market.holdings.set",
        json!({ "asset": "BTC", "quantity": 1 }),
        Some(TRACK),
    );
    kernel.call_tool(
        3,
        "market.holdings.set",
        json!({ "asset": "BTC", "quantity": 5 }),
        Some(OTHER_TRACK),
    );
    kernel.drain();

    assert_eq!(
        kernel.kv.get("holdings/trk_caller"),
        Some(&json!([{ "asset": "BTC", "quantity": 1.0 }]))
    );
    assert_eq!(
        kernel.kv.get("holdings/trk_someone_else"),
        Some(&json!([{ "asset": "BTC", "quantity": 5.0 }])),
        "the second call must not overwrite the first Track's portfolio"
    );
    let totals: Vec<f64> = kernel
        .pushes
        .iter()
        .filter(|(kind, _)| kind.starts_with("portfolio.holdings@"))
        .filter_map(|(_, payload)| {
            payload
                .pointer("/rows")
                .and_then(Value::as_array)
                .and_then(|rows| rows.last())
                .and_then(|row| row["value"].as_f64())
        })
        .collect();
    assert_eq!(totals, vec![2.0, 10.0], "each Track priced against its own");
}

/// Zero is the spelling of "no longer held", and it takes the holding out of
/// the stored portfolio rather than leaving a position of nothing.
#[test]
fn a_quantity_of_zero_removes_the_holding() {
    let endpoint = price_server("2");
    let mut kernel = FakeKernel::boot(&endpoint);
    kernel.call_tool(
        2,
        "market.holdings.set",
        json!({ "asset": "BTC", "quantity": 1 }),
        Some(TRACK),
    );
    kernel.call_tool(
        3,
        "market.holdings.set",
        json!({ "asset": "BTC", "quantity": 0 }),
        Some(TRACK),
    );
    kernel.drain();
    assert_eq!(kernel.kv.get("holdings/trk_caller"), Some(&json!([])));
}

/// The reader thread must never be the thread doing the work: a tool call
/// issues callbacks whose replies arrive on the same stdin the plugin reads.
/// The assertion is latency — the reply must land far inside the plugin's own
/// 15s callback timeout, which is what a blocked reader would cost.
#[test]
fn a_tool_call_is_answered_while_the_reader_keeps_reading() {
    let mut kernel = FakeKernel::boot(DEAD_ENDPOINT);
    let reply = kernel.call_tool(2, "market.holdings.list", json!({}), Some(TRACK));
    assert_eq!(reply.get("id"), Some(&json!(2)), "{reply:#?}");
}

/// A tick that cannot price part of the portfolio publishes the holdings table
/// — which names the gap row by row — and no history point.
///
/// The portfolio is deliberately PARTLY priceable: `USDT` is the quote asset
/// and prices at 1.0 with no network, while `BTC` goes to a dead endpoint. A
/// wholly unpriceable portfolio would leave the total at zero, and a defect
/// that skipped history only on a zero total would survive the test.
#[test]
fn a_tick_that_cannot_price_everything_writes_no_history_point() {
    let mut kernel = FakeKernel::boot(DEAD_ENDPOINT);
    kernel.call_tool(
        2,
        "market.holdings.set",
        json!({ "asset": "USDT", "quantity": 3 }),
        Some(TRACK),
    );
    let before = kernel.pushes.len();
    let reply = kernel.call_tool(
        3,
        "market.holdings.set",
        json!({ "asset": "BTC", "quantity": 1 }),
        Some(TRACK),
    );
    kernel.drain();

    let after: Vec<&str> = kernel.kinds_pushed().split_off(before);
    assert_eq!(
        after,
        vec![format!("portfolio.holdings@{TRACK}")],
        "holdings only — no history read, no history write, no history overlay"
    );
    let rows = kernel.pushes.last().unwrap().1["rows"]
        .as_array()
        .expect("rows")
        .clone();
    let btc = rows
        .iter()
        .find(|row| row["asset"] == json!("BTC"))
        .expect("the unpriceable holding keeps its row");
    assert!(btc["price"].is_null() && btc["value"].is_null(), "{btc}");
    assert_eq!(
        rows.last().unwrap()["value"].as_f64(),
        Some(3.0),
        "the total covers the priced rows only, so this tick is PARTIAL not empty"
    );
    assert_eq!(reply.pointer("/result/isError"), Some(&json!(true)));
    assert!(
        text_of(&reply).contains("history point was skipped"),
        "the caller must be told what was skipped: {reply:#?}"
    );
    assert!(kernel.is_responsive());
}

/// A history point that could not be stored is not published either:
/// publishing it would show a point the next tick silently drops, which reads
/// as data loss rather than the failed write it was.
#[test]
fn a_history_point_that_cannot_be_stored_is_not_published() {
    let endpoint = price_server("2");
    let mut kernel = FakeKernel::boot(&endpoint);
    // Seed the holding while writes still work, then refuse the history write.
    kernel.call_tool(
        2,
        "market.holdings.set",
        json!({ "asset": "BTC", "quantity": 1 }),
        Some(TRACK),
    );
    kernel.drain();
    let before = kernel.pushes.len();
    kernel.refuse_kv_set = true;

    let reply = kernel.call_tool(3, "market.refresh", json!({}), Some(TRACK));
    kernel.drain();

    let after: Vec<&str> = kernel.kinds_pushed().split_off(before);
    assert_eq!(
        after,
        vec![format!("portfolio.holdings@{TRACK}")],
        "the refused write must end the tick — no history overlay follows it"
    );
    assert_eq!(reply.pointer("/result/isError"), Some(&json!(true)));
    assert!(kernel.is_responsive());
}
