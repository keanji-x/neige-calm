//! Shared harness for the market plugin's process-level suites: a fake kernel
//! driving the real `market` binary over stdio, and loopback stand-ins for
//! every source the plugin can reach (Binance, Sina, Tencent ifzq).
//!
//! Moved here from `cases/market_plugin_process.rs` so that
//! `cases/market_series_process.rs` can boot the same binary the same way; the
//! behaviour of every item is unchanged. See that file's module doc for what
//! the fake kernel pins and why the real binary is spawned.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, channel};
use std::time::Duration;

use serde_json::{Value, json};

/// A port on loopback with nothing behind it: `ureq` fails to connect
/// immediately rather than waiting out a DNS or TCP timeout.
pub const DEAD_ENDPOINT: &str = "http://127.0.0.1:1";

/// Generous enough that a slow machine cannot fail it, far below the plugin's
/// own 15s callback timeout — which is what a blocked reader would cost.
pub const REPLY_BUDGET: Duration = Duration::from_secs(5);

/// How long `drain` waits for the plugin to go quiet. Much shorter than
/// [`REPLY_BUDGET`], because it is not proving anything by itself: a callback
/// that arrives after this window still fails the test, in `is_responsive`,
/// which refuses any `neige.*` request reaching it before the pong.
pub const QUIET_WINDOW: Duration = Duration::from_millis(1_500);

pub const TRACK: &str = "trk_caller";
pub const OTHER_TRACK: &str = "trk_someone_else";

/// A four-line HTTP server that answers every request with one fixed price.
///
/// It exists so the *successful* path is exercised somewhere other than a
/// developer's machine with a working route to the internet: without it, CI
/// only ever sees the plugin fail to price.
pub fn price_server(price: &'static str) -> (String, Arc<AtomicUsize>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind a loopback port");
    let port = listener.local_addr().expect("addr").port();
    let hits = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&hits);
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
            counter.fetch_add(1, Ordering::SeqCst);
            let body = format!("{{\"symbol\":\"X\",\"price\":\"{price}\"}}");
            let _ = write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len(),
            );
            let _ = stream.flush();
        }
    });
    (format!("http://127.0.0.1:{port}"), hits)
}

pub struct FakeKernel {
    pub child: Child,
    pub stdin: ChildStdin,
    pub frames: Receiver<Value>,
    /// Real per-plugin KV, because the plugin's correctness depends on reading
    /// back what it wrote.
    pub kv: HashMap<String, Value>,
    /// Every overlay push seen, as `(kind, payload)`, in order.
    pub pushes: Vec<(String, Value)>,
    /// Every `neige.*` method seen, in order — including the ones that carry
    /// no overlay, which is what absence assertions are made of.
    pub methods: Vec<String>,
    /// When set, a `neige.kv.set` whose key starts with this is answered with
    /// an error. A prefix rather than a flag: refusing *every* write would
    /// also refuse the holdings write, and then the tick under test would
    /// never reach the history step it is about.
    pub refuse_kv_set: Option<String>,
    /// Applied to the KV immediately after answering a `neige.kv.list`, to
    /// open exactly the window a stale-snapshot bug would fall into.
    pub mutate_after_list: Option<(String, Value)>,
}

impl FakeKernel {
    pub fn boot(endpoint: &str) -> Self {
        Self::boot_polling(endpoint, 3600)
    }

    /// `poll_seconds` at its floor (5) makes the background pass observable;
    /// every other test uses an hour so that the only refreshes it sees are
    /// the ones its own tool calls caused.
    pub fn boot_polling(endpoint: &str, poll_seconds: u64) -> Self {
        Self::boot_sources(endpoint, DEAD_ENDPOINT, poll_seconds)
    }

    /// Both sources named. The stock source defaults to [`DEAD_ENDPOINT`]
    /// everywhere else so that no test can reach `hq.sinajs.cn` by omission.
    pub fn boot_sources(endpoint: &str, sina_endpoint: &str, poll_seconds: u64) -> Self {
        Self::boot_settling(endpoint, sina_endpoint, poll_seconds, "USDT")
    }

    /// Both sources and the settlement currency. `USDT` is the default
    /// everywhere else, which is what makes a crypto-only portfolio need no
    /// exchange rate at all.
    pub fn boot_settling(
        endpoint: &str,
        sina_endpoint: &str,
        poll_seconds: u64,
        quote: &str,
    ) -> Self {
        Self::boot_with_values(json!({
            // Long enough that the poll thread never fires during
            // a test: every refresh these tests observe is one a
            // tool call caused.
            "poll_seconds": poll_seconds,
            "quote": quote,
            "binance_endpoint": endpoint,
            "sina_endpoint": sina_endpoint,
        }))
    }

    /// The `market.series` shape: the Tencent K-line source and Binance
    /// named, Sina dead (no quote path is exercised), and optionally the
    /// plugin's wall clock frozen at `debug_clock_ms`.
    pub fn boot_series(
        binance_endpoint: &str,
        tencent_endpoint: &str,
        debug_clock_ms: Option<i64>,
    ) -> Self {
        Self::boot_with_values(Self::series_values(
            binance_endpoint,
            tencent_endpoint,
            debug_clock_ms,
        ))
    }

    /// The configuration [`Self::boot_series`] hands over, reusable by
    /// [`Self::reinitialize`] to move the frozen clock.
    pub fn series_values(
        binance_endpoint: &str,
        tencent_endpoint: &str,
        debug_clock_ms: Option<i64>,
    ) -> Value {
        let mut values = json!({
            "poll_seconds": 3600,
            "quote": "USDT",
            "binance_endpoint": binance_endpoint,
            "sina_endpoint": DEAD_ENDPOINT,
            "tencent_endpoint": tencent_endpoint,
        });
        if let Some(frozen) = debug_clock_ms {
            values["debug_clock_ms"] = json!(frozen);
        }
        values
    }

    /// Spawn the binary and complete the handshake with exactly these
    /// `_meta["dev.neige/config"].values`.
    pub fn boot_with_values(values: Value) -> Self {
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
            refuse_kv_set: None,
            mutate_after_list: None,
        };
        kernel.reinitialize(1, values);
        kernel
    }

    /// Send an `initialize` carrying `values` and wait out the pass it wakes.
    /// The plugin REPLACES its configuration on every handshake, which is how
    /// a test moves the frozen clock without restarting the process (and
    /// without losing the plugin's in-memory state, which is the point).
    pub fn reinitialize(&mut self, id: u64, values: Value) {
        self.send(json!({
            "jsonrpc": "2.0", "id": id, "method": "initialize",
            "params": {
                "protocolVersion": "2025-11-25",
                "_meta": {
                    "dev.neige/auth": { "expected_echo": "TOKEN" },
                    "dev.neige/config": { "values": values }
                }
            }
        }));
        let handshake = self.next_frame().expect("handshake reply");
        assert_eq!(
            handshake.pointer("/result/_meta/dev.neige~1auth/echoed_token"),
            Some(&json!("TOKEN")),
            "the plugin must echo the kernel's token: {handshake}"
        );
        // The startup refresh finds no portfolios and asks only for the list.
        self.drain();
    }

    pub fn send(&mut self, frame: Value) {
        writeln!(self.stdin, "{frame}").expect("write to plugin");
        self.stdin.flush().expect("flush");
    }

    pub fn next_frame(&mut self) -> Result<Value, RecvTimeoutError> {
        self.frames.recv_timeout(REPLY_BUDGET)
    }

    /// Service `neige.*` requests until the plugin goes quiet, recording what
    /// it asked for. Returns any non-request frame (i.e. a `tools/call` reply)
    /// that arrived.
    pub fn drain(&mut self) -> Option<Value> {
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
    pub fn drain_until_reply(&mut self) -> Value {
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

    pub fn service(&mut self, frame: &Value) {
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
                let key_now = params["key"].as_str().unwrap_or_default().to_string();
                if self
                    .refuse_kv_set
                    .as_deref()
                    .is_some_and(|prefix| key_now.starts_with(prefix))
                {
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
                let answer = json!({ "entries": entries });
                if let Some((key, value)) = self.mutate_after_list.take() {
                    self.kv.insert(key, value);
                }
                answer
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
    pub fn call_tool(
        &mut self,
        id: u64,
        name: &str,
        arguments: Value,
        track: Option<&str>,
    ) -> Value {
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
    ///
    /// **What it does not cover.** The pong comes from the read loop, so it
    /// establishes that the reader is alive and that the tool call's own work
    /// (which is synchronous on the worker, and finished before its reply)
    /// emitted nothing more. It says nothing about a *background* pass: a poll
    /// thread could still emit a callback later. Every test using this sets
    /// `poll_seconds` to an hour so no background pass can run inside it —
    /// that, not the ping, is what makes the absence total.
    pub fn is_responsive(&mut self) -> bool {
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

    /// Record a holding and wait out the pass it wakes.
    pub fn set_holding(&mut self, id: u64, asset: &str, quantity: f64, track: &str) -> Value {
        let reply = self.call_tool(
            id,
            "market.holdings.set",
            json!({ "asset": asset, "quantity": quantity }),
            Some(track),
        );
        self.drain();
        reply
    }

    /// Overlay pushes since `from`, as `kind@track`.
    pub fn pushes_since(&self, from: usize) -> Vec<&str> {
        self.pushes[from..]
            .iter()
            .map(|(kind, _)| kind.as_str())
            .collect()
    }

    /// The `Total` cell of the last `portfolio.holdings` push for `track`.
    pub fn last_total_for(&self, track: &str) -> Option<f64> {
        self.pushes
            .iter()
            .rev()
            .find(|(kind, _)| kind == &format!("portfolio.holdings@{track}"))
            .and_then(|(_, payload)| {
                payload
                    .pointer("/rows")
                    .and_then(Value::as_array)
                    .and_then(|rows| rows.last())
                    .and_then(|row| row["value"].as_f64())
            })
    }

    pub fn kinds_pushed(&self) -> Vec<&str> {
        self.pushes.iter().map(|(kind, _)| kind.as_str()).collect()
    }
}

impl Drop for FakeKernel {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

pub fn text_of(reply: &Value) -> String {
    reply
        .pointer("/result/content/0/text")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

// The fixture rows, the GBK name bytes and the response builder are shared
// with the plugin's own unit tests — one copy of the wire format, so the two
// suites cannot drift onto two different shapes of the same endpoint.
include!("../../../../plugins/market/sina_fixture.rs");

/// A loopback stand-in for `hq.sinajs.cn`, the US/HK/SH/SZ source.
///
/// It reproduces the two properties of that endpoint that the plugin's parser
/// depends on: the body is **GBK**, and a request with no `Referer` header is
/// answered `403 Forbidden` — not with an empty list, not with JSON. The name
/// field carries real GBK bytes so the decode is exercised here too.
pub fn sina_server() -> String {
    sina_server_with_rows(sina_fixture_all_rows())
}

/// The same endpoint serving only the STOCK rows: it lists no exchange rate at
/// all, which is how a pass that can price a holding but cannot convert it is
/// built without taking the whole endpoint down.
pub fn sina_server_without_rates() -> String {
    sina_server_with_rows(SINA_FIXTURE_ROWS.to_vec())
}

pub fn sina_server_with_rows(rows: Vec<(&'static str, &'static str)>) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind a loopback port");
    let port = listener.local_addr().expect("addr").port();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { return };
            let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
            let mut head = Vec::new();
            let mut byte = [0u8; 1];
            while stream.read_exact(&mut byte).is_ok() {
                head.push(byte[0]);
                if head.ends_with(b"\r\n\r\n") {
                    break;
                }
            }
            let head = String::from_utf8_lossy(&head).to_string();
            if !head
                .to_ascii_lowercase()
                .contains("referer: https://finance.sina.com.cn")
            {
                let _ = write!(
                    stream,
                    "HTTP/1.1 403 Forbidden\r\nContent-Length: 9\r\nConnection: close\r\n\r\nForbidden"
                );
                let _ = stream.flush();
                continue;
            }
            let target = head
                .lines()
                .next()
                .unwrap_or_default()
                .split_whitespace()
                .nth(1)
                .unwrap_or_default()
                .to_string();
            let body = sina_fixture_body(&target, &rows);
            let _ = write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len(),
            );
            let _ = stream.write_all(&body);
            let _ = stream.flush();
        }
    });
    format!("http://127.0.0.1:{port}")
}

// ---------------------------------------------------------------------------
// `market.series` sources — Tencent ifzq and Binance klines
// ---------------------------------------------------------------------------

/// One daily bar as the series fixtures hold it, in the natural o/h/l/c
/// order. Each server renders it in ITS source's wire order — ifzq's is
/// o,c,h,l,v — which is what lets a column-order bug in the plugin show.
#[derive(Clone, Debug, PartialEq)]
pub struct FixtureBar {
    pub date: String,
    pub open: f64,
    pub high: f64,
    pub low: f64,
    pub close: f64,
    pub volume: f64,
}

/// Parse a fixture date. `chrono` is the kernel's calendar; the plugin has its
/// own pure one, and the two meeting on the wire is part of what is tested.
pub fn fixture_date(raw: &str) -> chrono::NaiveDate {
    chrono::NaiveDate::parse_from_str(raw, "%Y-%m-%d").expect(raw)
}

/// UTC midnight of a fixture date, in unix milliseconds.
pub fn day_ms(raw: &str) -> i64 {
    fixture_date(raw)
        .and_hms_opt(0, 0, 0)
        .expect("midnight")
        .and_utc()
        .timestamp_millis()
}

/// `YYYY-MM-DD` of a `ts_ms` at UTC midnight (the plugin's point timestamp).
pub fn date_of_ms(ts_ms: i64) -> String {
    chrono::DateTime::from_timestamp_millis(ts_ms)
        .expect("a timestamp")
        .format("%Y-%m-%d")
        .to_string()
}

/// One bar per calendar day in `[from, to]` when `weekdays_only` is false,
/// one per Monday–Friday otherwise. Values are deterministic, distinct per
/// column and per day: open = base, high = base + 2, low = base − 1,
/// close = base + 1, volume = 1000 + index, with base = seed + index.
pub fn bars_between(from: &str, to: &str, seed: f64, weekdays_only: bool) -> Vec<FixtureBar> {
    let mut out = Vec::new();
    let mut day = fixture_date(from);
    let last = fixture_date(to);
    let mut index = 0.0;
    while day <= last {
        let is_weekday = matches!(
            chrono::Datelike::weekday(&day),
            chrono::Weekday::Mon
                | chrono::Weekday::Tue
                | chrono::Weekday::Wed
                | chrono::Weekday::Thu
                | chrono::Weekday::Fri
        );
        if !weekdays_only || is_weekday {
            let base = seed + index;
            out.push(FixtureBar {
                date: day.format("%Y-%m-%d").to_string(),
                open: base,
                high: base + 2.0,
                low: base - 1.0,
                close: base + 1.0,
                volume: 1000.0 + index,
            });
            index += 1.0;
        }
        day = day.succ_opt().expect("next day");
    }
    out
}

pub fn weekday_bars(from: &str, to: &str, seed: f64) -> Vec<FixtureBar> {
    bars_between(from, to, seed, true)
}

/// ifzq's newest-rows cap for `sh`/`sz` codes (spike U8, 2026-09-13).
pub const IFZQ_CN_CAP: usize = 640;
/// The adjustment baseline row ifzq prepends to bare `us` answers (U6).
pub const IFZQ_US_BASELINE_DATE: &str = "2011-06-02";

/// What the ifzq stand-in serves. Codes are spelled as the source spells
/// them: `sh600519`, `sz000001`, `hk00700`, `usNVDA.OQ`.
#[derive(Default)]
pub struct IfzqFixture {
    pub rows: HashMap<String, Vec<FixtureBar>>,
    /// Bare `us<SYM>` → what `qt.<code>[2]` answers (`"NVDA.OQ"`); the
    /// window code is `us` + that.
    pub suffix: HashMap<String, String>,
    /// When set, every request is answered `{"code":0,"msg":<this>,"data":[]}`.
    pub refuse_with: Option<String>,
    /// Applied to the fixture once, right after the FIRST request has been
    /// answered — the "source advanced a day between two requests" seam.
    pub after_first: Option<FixtureAdvance>,
}

/// A one-shot mutation of an [`IfzqFixture`], run by the server thread.
pub type FixtureAdvance = Box<dyn FnOnce(&mut IfzqFixture) + Send>;

impl IfzqFixture {
    pub fn with_rows(code: &str, rows: Vec<FixtureBar>) -> Self {
        let mut fixture = Self::default();
        fixture.rows.insert(code.to_string(), rows);
        fixture
    }

    pub fn add(mut self, code: &str, rows: Vec<FixtureBar>) -> Self {
        self.rows.insert(code.to_string(), rows);
        self
    }

    pub fn with_suffix(mut self, bare: &str, suffixed: &str) -> Self {
        self.suffix.insert(bare.to_string(), suffixed.to_string());
        self
    }
}

pub struct IfzqServer {
    pub endpoint: String,
    /// Every request answered, in order.
    pub hits: Arc<AtomicUsize>,
    /// Every request's `param` value, in order — what the probe / window
    /// ordering and the paging assertions read.
    pub params: Arc<std::sync::Mutex<Vec<String>>>,
    pub fixture: Arc<std::sync::Mutex<IfzqFixture>>,
}

impl IfzqServer {
    pub fn params(&self) -> Vec<String> {
        self.params.lock().unwrap().clone()
    }

    /// The `param` values that carried a window (a non-empty start date).
    pub fn window_params(&self) -> Vec<String> {
        self.params()
            .into_iter()
            .filter(|p| !p.split(',').nth(2).unwrap_or_default().is_empty())
            .collect()
    }

    pub fn hits(&self) -> usize {
        self.hits.load(Ordering::SeqCst)
    }

    /// Replace one code's rows between two tool calls.
    pub fn set_rows(&self, code: &str, rows: Vec<FixtureBar>) {
        self.fixture
            .lock()
            .unwrap()
            .rows
            .insert(code.to_string(), rows);
    }
}

fn ifzq_row(bar: &FixtureBar) -> Value {
    // The source's column order: date, open, CLOSE, HIGH, LOW, volume.
    json!([
        bar.date,
        format!("{:.3}", bar.open),
        format!("{:.3}", bar.close),
        format!("{:.3}", bar.high),
        format!("{:.3}", bar.low),
        format!("{:.3}", bar.volume),
    ])
}

fn percent_decode(raw: &str) -> String {
    let bytes = raw.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && i + 2 < bytes.len()
            && let Ok(byte) = u8::from_str_radix(&raw[i + 1..i + 3], 16)
        {
            out.push(byte);
            i += 3;
            continue;
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Build the ifzq answer for one `param`, reproducing the three shapes the
/// spike measured (U8): `sh`/`sz` answer under `qfqday` and keep only the
/// NEWEST 640 rows of a window; `hk` answers under `day` in full; a bare
/// `us<SYM>` answers ZERO rows to a windowed request and, to a probe, the
/// 2011 baseline row plus the newest bar with `qt.<code>[2]` naming the
/// suffixed code; `us<SYM>.<SUFFIX>` answers under `day` in full.
fn ifzq_body(fixture: &IfzqFixture, param: &str) -> String {
    if let Some(msg) = &fixture.refuse_with {
        return json!({ "code": 0, "msg": msg, "data": [] }).to_string();
    }
    let parts: Vec<&str> = param.split(',').collect();
    let code = parts.first().copied().unwrap_or_default();
    let start = parts.get(2).copied().unwrap_or_default();
    let end = parts.get(3).copied().unwrap_or_default();
    let n: usize = parts.get(4).and_then(|n| n.parse().ok()).unwrap_or(0);
    if n > 2000 {
        return json!({ "code": 0, "msg": "param error", "data": [] }).to_string();
    }
    let windowed = !start.is_empty() || !end.is_empty();
    let bare_us = code.starts_with("us") && !code.contains('.');
    let empty = Vec::new();
    let (key, rows): (&str, Vec<Value>) = if bare_us {
        let suffixed = fixture.suffix.get(code);
        let newest = suffixed
            .and_then(|s| fixture.rows.get(&format!("us{s}")))
            .and_then(|rows| rows.last());
        let rows = if windowed {
            Vec::new()
        } else {
            let mut rows = vec![json!([
                IFZQ_US_BASELINE_DATE,
                "19.020",
                "19.020",
                "19.280",
                "18.840",
                "19701450.000"
            ])];
            rows.extend(newest.map(ifzq_row));
            rows
        };
        let mut entry = json!({ "day": rows });
        if let Some(suffixed) = suffixed {
            entry["qt"] = json!({ code: ["delay", "name", suffixed] });
        }
        return json!({ "code": 0, "msg": "", "data": { code: entry } }).to_string();
    } else {
        let all = fixture.rows.get(code).unwrap_or(&empty);
        let mut selected: Vec<&FixtureBar> = all
            .iter()
            .filter(|bar| {
                (start.is_empty() || bar.date.as_str() >= start)
                    && (end.is_empty() || bar.date.as_str() <= end)
            })
            .collect();
        if !windowed && selected.len() > n {
            selected = selected[selected.len() - n..].to_vec();
        }
        let cn = code.starts_with("sh") || code.starts_with("sz");
        if cn && selected.len() > IFZQ_CN_CAP {
            selected = selected[selected.len() - IFZQ_CN_CAP..].to_vec();
        }
        (
            if cn { "qfqday" } else { "day" },
            selected.into_iter().map(ifzq_row).collect(),
        )
    };
    json!({ "code": 0, "msg": "", "data": { code: { key: rows } } }).to_string()
}

/// A loopback stand-in for `web.ifzq.gtimg.cn`'s daily K-line path.
pub fn ifzq_server(fixture: IfzqFixture) -> IfzqServer {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind a loopback port");
    let port = listener.local_addr().expect("addr").port();
    let hits = Arc::new(AtomicUsize::new(0));
    let params = Arc::new(std::sync::Mutex::new(Vec::new()));
    let fixture = Arc::new(std::sync::Mutex::new(fixture));
    let server = IfzqServer {
        endpoint: format!("http://127.0.0.1:{port}"),
        hits: Arc::clone(&hits),
        params: Arc::clone(&params),
        fixture: Arc::clone(&fixture),
    };
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { return };
            let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
            let head = read_head(&mut stream);
            let target = request_target(&head);
            let param = target
                .split_once("param=")
                .map(|(_, rest)| rest.split('&').next().unwrap_or_default())
                .map(percent_decode)
                .unwrap_or_default();
            let body = {
                let mut fixture = fixture.lock().unwrap();
                let body = ifzq_body(&fixture, &param);
                if hits.load(Ordering::SeqCst) == 0
                    && let Some(advance) = fixture.after_first.take()
                {
                    advance(&mut fixture);
                }
                body
            };
            params.lock().unwrap().push(param);
            hits.fetch_add(1, Ordering::SeqCst);
            let _ = write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len(),
            );
            let _ = stream.flush();
        }
    });
    server
}

fn read_head(stream: &mut std::net::TcpStream) -> String {
    let mut head = Vec::new();
    let mut byte = [0u8; 1];
    while stream.read_exact(&mut byte).is_ok() {
        head.push(byte[0]);
        if head.ends_with(b"\r\n\r\n") {
            break;
        }
    }
    String::from_utf8_lossy(&head).to_string()
}

fn request_target(head: &str) -> String {
    head.lines()
        .next()
        .unwrap_or_default()
        .split_whitespace()
        .nth(1)
        .unwrap_or_default()
        .to_string()
}

/// What the Binance klines stand-in serves, by spot symbol (`BTCUSDT`).
#[derive(Default)]
pub struct BinanceFixture {
    pub klines: HashMap<String, Vec<FixtureBar>>,
}

impl BinanceFixture {
    pub fn with_klines(symbol: &str, rows: Vec<FixtureBar>) -> Self {
        let mut fixture = Self::default();
        fixture.klines.insert(symbol.to_string(), rows);
        fixture
    }
}

pub struct BinanceServer {
    pub endpoint: String,
    pub hits: Arc<AtomicUsize>,
    pub fixture: Arc<std::sync::Mutex<BinanceFixture>>,
}

impl BinanceServer {
    pub fn hits(&self) -> usize {
        self.hits.load(Ordering::SeqCst)
    }
}

fn query_param<'a>(target: &'a str, key: &str) -> Option<&'a str> {
    target.split_once('?')?.1.split('&').find_map(|pair| {
        pair.strip_prefix(key)
            .and_then(|rest| rest.strip_prefix('='))
    })
}

/// Build one klines answer. The real endpoint filters by `openTime` within
/// `[startTime, endTime]` and returns the FIRST `limit` klines of that range
/// (oldest first); with neither bound it returns the newest `limit`. An
/// unlisted symbol is HTTP 400 with `{"code":-1121,…}`.
fn binance_body(fixture: &BinanceFixture, target: &str) -> (u16, String) {
    let Some(symbol) = query_param(target, "symbol") else {
        return (
            400,
            json!({ "code": -1100, "msg": "Illegal characters" }).to_string(),
        );
    };
    let Some(all) = fixture.klines.get(symbol) else {
        return (
            400,
            json!({ "code": -1121, "msg": "Invalid symbol." }).to_string(),
        );
    };
    let start = query_param(target, "startTime").and_then(|v| v.parse::<i64>().ok());
    let end = query_param(target, "endTime").and_then(|v| v.parse::<i64>().ok());
    let limit: usize = query_param(target, "limit")
        .and_then(|v| v.parse().ok())
        .unwrap_or(500)
        .min(1000);
    let mut selected: Vec<&FixtureBar> = all
        .iter()
        .filter(|bar| {
            let open = day_ms(&bar.date);
            start.is_none_or(|s| open >= s) && end.is_none_or(|e| open <= e)
        })
        .collect();
    if start.is_none() && end.is_none() {
        if selected.len() > limit {
            selected = selected[selected.len() - limit..].to_vec();
        }
    } else {
        selected.truncate(limit);
    }
    let rows: Vec<Value> = selected
        .into_iter()
        .map(|bar| {
            let open_time = day_ms(&bar.date);
            json!([
                open_time,
                format!("{:.2}", bar.open),
                format!("{:.2}", bar.high),
                format!("{:.2}", bar.low),
                format!("{:.2}", bar.close),
                format!("{:.5}", bar.volume),
                open_time + 86_400_000 - 1,
                "0",
                0,
                "0",
                "0",
                "0"
            ])
        })
        .collect();
    (200, Value::Array(rows).to_string())
}

/// A loopback stand-in for Binance's `/api/v3/klines`.
pub fn binance_klines_server(fixture: BinanceFixture) -> BinanceServer {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind a loopback port");
    let port = listener.local_addr().expect("addr").port();
    let hits = Arc::new(AtomicUsize::new(0));
    let fixture = Arc::new(std::sync::Mutex::new(fixture));
    let server = BinanceServer {
        endpoint: format!("http://127.0.0.1:{port}"),
        hits: Arc::clone(&hits),
        fixture: Arc::clone(&fixture),
    };
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { return };
            let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
            let head = read_head(&mut stream);
            let target = request_target(&head);
            let (status, body) = binance_body(&fixture.lock().unwrap(), &target);
            hits.fetch_add(1, Ordering::SeqCst);
            let reason = if status == 200 { "OK" } else { "Bad Request" };
            let _ = write!(
                stream,
                "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len(),
            );
            let _ = stream.flush();
        }
    });
    server
}
