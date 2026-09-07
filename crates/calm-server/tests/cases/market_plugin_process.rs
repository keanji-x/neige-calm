//! Process-level tests for the market-data plugin, driven through a fake
//! kernel over the real stdio channel.
//!
//! What these pin is not reachable from a pure function: which thread runs a
//! tool call, which Track a call is attributed to, and the order of the
//! callbacks a refresh makes. So the binary is spawned for real, and the fake
//! kernel keeps actual KV state — a plugin whose `kv.set` went nowhere would
//! pass a stubbed-out harness while losing every holding in production.
//!
//! Network: only where a test needs a price, and never a real one — every
//! source this plugin has is pointed at loopback here. `DEAD_ENDPOINT` is a
//! port nothing listens on; [`price_server`] stands in for Binance and
//! [`sina_server`] for `hq.sinajs.cn`. `USDT` prices at 1.0 with no request at
//! all, off Binance's pinned quote leg, which is what lets a *partially*
//! priceable portfolio be built offline.
//!
//! A price now carries the currency its SOURCE quoted it in — `USDT` off
//! Binance, `USD`/`HKD`/`CNY` off Sina, decided from the venue and code range — rather than the configured settlement
//! currency, and a portfolio whose priced rows span two of them gets no total
//! at all until exchange rates land.

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
fn price_server(price: &'static str) -> (String, Arc<AtomicUsize>) {
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
    /// When set, a `neige.kv.set` whose key starts with this is answered with
    /// an error. A prefix rather than a flag: refusing *every* write would
    /// also refuse the holdings write, and then the tick under test would
    /// never reach the history step it is about.
    refuse_kv_set: Option<String>,
    /// Applied to the KV immediately after answering a `neige.kv.list`, to
    /// open exactly the window a stale-snapshot bug would fall into.
    mutate_after_list: Option<(String, Value)>,
}

impl FakeKernel {
    fn boot(endpoint: &str) -> Self {
        Self::boot_polling(endpoint, 3600)
    }

    /// `poll_seconds` at its floor (5) makes the background pass observable;
    /// every other test uses an hour so that the only refreshes it sees are
    /// the ones its own tool calls caused.
    fn boot_polling(endpoint: &str, poll_seconds: u64) -> Self {
        Self::boot_sources(endpoint, DEAD_ENDPOINT, poll_seconds)
    }

    /// Both sources named. The stock source defaults to [`DEAD_ENDPOINT`]
    /// everywhere else so that no test can reach `hq.sinajs.cn` by omission.
    fn boot_sources(endpoint: &str, sina_endpoint: &str, poll_seconds: u64) -> Self {
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
                        "poll_seconds": poll_seconds,
                        "quote": "USDT",
                        "binance_endpoint": endpoint,
                        "sina_endpoint": sina_endpoint,
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
    ///
    /// **What it does not cover.** The pong comes from the read loop, so it
    /// establishes that the reader is alive and that the tool call's own work
    /// (which is synchronous on the worker, and finished before its reply)
    /// emitted nothing more. It says nothing about a *background* pass: a poll
    /// thread could still emit a callback later. Every test using this sets
    /// `poll_seconds` to an hour so no background pass can run inside it —
    /// that, not the ping, is what makes the absence total.
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

    /// Record a holding and wait out the pass it wakes.
    fn set_holding(&mut self, id: u64, asset: &str, quantity: f64, track: &str) -> Value {
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
    fn pushes_since(&self, from: usize) -> Vec<&str> {
        self.pushes[from..]
            .iter()
            .map(|(kind, _)| kind.as_str())
            .collect()
    }

    /// The `Total` cell of the last `portfolio.holdings` push for `track`.
    fn last_total_for(&self, track: &str) -> Option<f64> {
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

/// Recording a holding wakes a pass that prices it and publishes to the Track
/// the CALL came from.
///
/// The tool itself does not price: pricing is a network call, and a tool
/// annotated `openWorldHint: true` is one codex refuses outright under the
/// kernel's `approval_policy: "never"`. So the write records state and wakes
/// the poll thread, and the reader still sees a fresh table a moment later.
#[test]
fn setting_a_holding_prices_it_now_and_publishes_to_the_callers_track() {
    let (endpoint, _hits) = price_server("2.5");
    // A long poll interval proves the publish came from the WAKE, not from
    // the interval elapsing.
    let mut kernel = FakeKernel::boot(&endpoint);

    let reply = kernel.set_holding(2, "BTC", 4.0, TRACK);

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
        Some(&json!([{ "asset": "CRYPTO:BTC", "quantity": 4.0 }])),
        "the holding is stored under the caller's Track, spelled as the \
         canonical identity a bare crypto name normalises to"
    );
}

/// A holding stored before venues existed and a write that names the same
/// asset with its venue are ONE holding, and the write leaves ONE row.
///
/// Driven through the real binary and the real KV because the collapse
/// happens across the whole `set` path — load (which normalises), `retain`,
/// then a whole-array overwrite. Without read-side normalisation the retain
/// compares `CRYPTO:BTC` against the legacy `BTC`, keeps it, and the KV ends
/// up with two rows summing to 160 that the tables would price as one
/// position of 160 BTC.
///
/// It also pins what the first write leaves behind: the canonical spelling,
/// in place of the legacy one.
#[test]
fn a_legacy_bare_holding_is_replaced_not_doubled_by_a_qualified_write() {
    let mut kernel = FakeKernel::boot(DEAD_ENDPOINT);
    // Seeded directly, as a pre-venues install would have left it. The plugin
    // has never seen this Track.
    kernel.kv.insert(
        "holdings/trk_caller".to_string(),
        json!([{ "asset": "BTC", "quantity": 100.0 }]),
    );

    kernel.set_holding(2, "crypto:BTC", 60.0, TRACK);

    assert_eq!(
        kernel.kv.get("holdings/trk_caller"),
        Some(&json!([{ "asset": "CRYPTO:BTC", "quantity": 60.0 }])),
        "one row at the written quantity — two rows here would be 160 BTC"
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

/// Two Tracks keep two portfolios, and each is priced against its own.
#[test]
fn holdings_are_per_track() {
    let (endpoint, _hits) = price_server("2");
    let mut kernel = FakeKernel::boot(&endpoint);
    kernel.set_holding(2, "BTC", 1.0, TRACK);
    kernel.set_holding(3, "BTC", 5.0, OTHER_TRACK);

    assert_eq!(
        kernel.kv.get("holdings/trk_caller"),
        Some(&json!([{ "asset": "CRYPTO:BTC", "quantity": 1.0 }]))
    );
    assert_eq!(
        kernel.kv.get("holdings/trk_someone_else"),
        Some(&json!([{ "asset": "CRYPTO:BTC", "quantity": 5.0 }])),
        "the second call must not overwrite the first Track's portfolio"
    );
    // Priced against its own holdings, not against a shared document. Read
    // per Track rather than in push order: a pass may batch both Tracks.
    assert_eq!(kernel.last_total_for(TRACK), Some(2.0));
    assert_eq!(kernel.last_total_for(OTHER_TRACK), Some(10.0));

    // Storing separately is not the same as reading separately. Without this,
    // an implementation that wrote per Track but read from one shared
    // document would still pass everything above.
    let listed = kernel.call_tool(4, "market.holdings.list", json!({}), Some(TRACK));
    let holdings = listed
        .pointer("/result/structuredContent/holdings")
        .and_then(Value::as_array)
        .expect("holdings")
        .clone();
    assert_eq!(holdings.len(), 1, "{listed:#?}");
    assert_eq!(
        holdings[0]["qty"].as_f64(),
        Some(1.0),
        "reading back the first Track must not see the second Track's quantity"
    );
}

/// Zero is the spelling of "no longer held", and it takes the holding out of
/// the stored portfolio rather than leaving a position of nothing.
#[test]
fn a_quantity_of_zero_removes_the_holding() {
    let (endpoint, _hits) = price_server("2");
    let mut kernel = FakeKernel::boot(&endpoint);
    kernel.set_holding(2, "BTC", 1.0, TRACK);
    kernel.set_holding(3, "BTC", 0.0, TRACK);
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

/// A pass that cannot price part of the portfolio publishes the holdings table
/// — which names the gap row by row — and no history point.
///
/// The portfolio is deliberately PARTLY priceable: `USDT` is the quote asset
/// and prices at 1.0 with no network, while `BTC` goes to a dead endpoint. A
/// wholly unpriceable portfolio would leave the total at zero, and a defect
/// that skipped history only on a zero total would survive the test.
#[test]
fn a_tick_that_cannot_price_everything_writes_no_history_point() {
    let mut kernel = FakeKernel::boot(DEAD_ENDPOINT);
    kernel.set_holding(2, "USDT", 3.0, TRACK);
    let before = kernel.pushes.len();
    kernel.set_holding(3, "BTC", 1.0, TRACK);

    assert_eq!(
        kernel.pushes_since(before),
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
        "the total covers the priced rows only, so this pass is PARTIAL not empty"
    );
    assert!(kernel.is_responsive());
}

/// A history point that could not be stored is not published either:
/// publishing it would show a point the next pass silently drops, which reads
/// as data loss rather than the failed write it was.
#[test]
fn a_history_point_that_cannot_be_stored_is_not_published() {
    let (endpoint, _hits) = price_server("2");
    let mut kernel = FakeKernel::boot(&endpoint);
    kernel.set_holding(2, "BTC", 1.0, TRACK);
    let before = kernel.pushes.len();

    // Refuse only the history write. Refusing every write would also refuse
    // the holdings write below, and the pass would never reach the step this
    // test is about.
    kernel.refuse_kv_set = Some("history/".into());
    kernel.set_holding(3, "ETH", 2.0, TRACK);

    assert_eq!(
        kernel.pushes_since(before),
        vec![format!("portfolio.holdings@{TRACK}")],
        "the refused write must end the pass — no history overlay follows it"
    );
    assert!(kernel.is_responsive());
}

/// A poll pass must not republish a portfolio it read before a tool changed it.
///
/// The pass lists every Track up front and then prices them one at a time. If
/// it published the listing's snapshot, a Track that a tool call updated and
/// re-published in the meantime would be overwritten with the older value —
/// and its obsolete total appended to the history, drawing a move that never
/// happened. The fake kernel changes the stored holding in exactly that
/// window: after the listing is answered, before the Track is priced.
#[test]
fn a_poll_pass_prices_what_is_stored_now_not_what_it_listed() {
    let (endpoint, _hits) = price_server("2");
    let mut kernel = FakeKernel::boot_polling(&endpoint, 5);

    kernel.set_holding(2, "BTC", 1.0, TRACK);
    let before = kernel.pushes.len();

    // From the next listing onward the store says 5, not 1.
    kernel.mutate_after_list = Some((
        "holdings/trk_caller".to_string(),
        json!([{ "asset": "BTC", "quantity": 5.0 }]),
    ));

    // Wait out one poll pass (5s floor) and service it.
    let mut totals = Vec::new();
    let deadline = std::time::Instant::now() + Duration::from_secs(20);
    while std::time::Instant::now() < deadline && totals.is_empty() {
        if let Ok(frame) = kernel.frames.recv_timeout(Duration::from_secs(8))
            && frame.get("method").is_some()
        {
            kernel.service(&frame);
        }
        totals = kernel.pushes[before..]
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
    }

    assert_eq!(
        totals.first(),
        Some(&10.0),
        "the pass must price the stored 5 BTC (=10), not the 1 BTC it listed"
    );
}

/// One asset is priced once per pass, however many Tracks hold it.
///
/// Without the cache a pass costs one request per holding per Track: several
/// Tracks watching the same asset would ask for the same number several times
/// within the same second, and the pass would take proportionally longer.
#[test]
fn a_poll_pass_prices_each_asset_once_across_tracks() {
    let (endpoint, hits) = price_server("2");
    let mut kernel = FakeKernel::boot_polling(&endpoint, 5);

    for (id, track) in [(2, TRACK), (3, OTHER_TRACK)] {
        kernel.set_holding(id, "BTC", 1.0, track);
    }
    let before_pass = hits.load(Ordering::SeqCst);

    // Service exactly one background pass: both Tracks, one asset.
    let mut seen_tracks = std::collections::HashSet::new();
    let deadline = std::time::Instant::now() + Duration::from_secs(20);
    while std::time::Instant::now() < deadline && seen_tracks.len() < 2 {
        if let Ok(frame) = kernel.frames.recv_timeout(Duration::from_secs(8))
            && frame.get("method").is_some()
        {
            {
                let kind = frame
                    .pointer("/params/kind")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string();
                let entity = frame
                    .pointer("/params/entity_id")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string();
                kernel.service(&frame);
                if kind == "portfolio.holdings" {
                    seen_tracks.insert(entity);
                }
            }
        }
    }

    assert_eq!(
        seen_tracks.len(),
        2,
        "both Tracks must be priced in the pass"
    );
    assert_eq!(
        hits.load(Ordering::SeqCst) - before_pass,
        1,
        "BTC must be fetched once for the whole pass, not once per Track"
    );
}

/// Selling out replaces the table rather than leaving the old one on screen.
///
/// Returning early on an empty portfolio would leave a reader who has just
/// sold everything looking at their previous position, presented as current —
/// a worse lie than an empty table. No history point goes with it: the series
/// is about a portfolio's value, and there is no longer a portfolio.
#[test]
fn removing_the_last_holding_publishes_an_empty_table() {
    let (endpoint, _hits) = price_server("2");
    let mut kernel = FakeKernel::boot(&endpoint);
    kernel.set_holding(2, "BTC", 1.0, TRACK);
    let before = kernel.pushes.len();
    let reply = kernel.set_holding(3, "BTC", 0.0, TRACK);

    assert_ne!(
        reply.pointer("/result/isError"),
        Some(&json!(true)),
        "removing the last holding is a success: {reply:#?}"
    );
    assert_eq!(
        kernel.pushes_since(before),
        vec![format!("portfolio.holdings@{TRACK}")],
        "the emptied table is republished, and no history point goes with it"
    );
    let rows = kernel.pushes.last().unwrap().1["rows"]
        .as_array()
        .expect("rows")
        .clone();
    assert_eq!(rows.len(), 1, "only the Total row remains: {rows:?}");
    assert_eq!(rows[0]["value"].as_f64(), Some(0.0));
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
fn sina_server() -> String {
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
            let body = sina_fixture_body(&target, SINA_FIXTURE_ROWS);
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

/// The last `portfolio.holdings` payload pushed at `track`.
fn last_holdings_table<'a>(kernel: &'a FakeKernel, track: &str) -> &'a Value {
    kernel
        .pushes
        .iter()
        .rev()
        .find(|(kind, _)| kind == &format!("portfolio.holdings@{track}"))
        .map(|(_, payload)| payload)
        .expect("a holdings push")
}

/// A stock holding is priced through the whole shipping path — the real
/// binary, the real KV, the real HTTP client — and reaches the reader in the
/// currency ITS market quotes, not in the configured settlement currency.
///
/// The install settles in `USDT` (see `boot_sources`), so a row labelled
/// `USDT` would look right in every crypto test and be wrong by a factor of
/// the HKD/USDT rate here.
#[test]
fn a_hong_kong_holding_is_priced_in_hkd_and_totalled_in_hkd() {
    let mut kernel = FakeKernel::boot_sources(DEAD_ENDPOINT, &sina_server(), 3600);

    kernel.set_holding(2, "HK:1810", 100.0, TRACK);

    assert_eq!(
        kernel.kinds_pushed(),
        vec![
            format!("portfolio.holdings@{TRACK}"),
            format!("portfolio.history@{TRACK}"),
        ],
        "a fully-priced tick publishes both tables"
    );
    let table = last_holdings_table(&kernel, TRACK);
    let rows = table["rows"].as_array().expect("rows");
    assert_eq!(rows[0]["asset"], json!("1810"));
    assert_eq!(rows[0]["venue"], json!("HK"));
    assert_eq!(
        rows[0]["price"].as_f64(),
        Some(27.48),
        "field 6 of the HK row is the last price; field 3 (28.44) is the \
         previous close: {rows:?}"
    );
    assert_eq!(rows[0]["currency"], json!("HKD"));
    assert_eq!(rows[1]["value"].as_f64(), Some(2748.0), "the Total row");
    assert_eq!(rows[1]["currency"], json!("HKD"));
    let caption = table["caption"].as_str().expect("caption");
    assert!(caption.contains("totalled in HKD"), "{caption}");
    assert!(!caption.contains("USDT"), "{caption}");

    // The stored history point exists — a stock holding no longer stalls the
    // series the way it did while the stock venues had no source.
    let points = kernel
        .kv
        .get("history/trk_caller")
        .and_then(Value::as_array)
        .expect("a history document");
    assert_eq!(points.len(), 1, "{points:?}");
    assert_eq!(points[0]["total"].as_f64(), Some(2748.0));
}

/// **A portfolio spanning two currencies gets rows but no total — and no
/// history point.**
///
/// `USDT` prices at 1.0 off Binance's quote leg with no request; `US:NVDA`
/// prices at 230.36 USD off the stock source. Both rows price, so the tick is
/// COMPLETE — the refusal below is about currencies, not about a missing
/// price, which is what makes this different from every other no-total test
/// in this file. Adding 1 to 230.36 would publish 231.36 of nothing as the
/// portfolio's value and append it to a series measured in something else.
///
/// The missing history point is the registered consequence: such a Track has
/// no series until exchange rates land.
#[test]
fn a_portfolio_across_two_currencies_publishes_rows_but_no_total_and_no_history() {
    let mut kernel = FakeKernel::boot_sources(DEAD_ENDPOINT, &sina_server(), 3600);
    kernel.set_holding(2, "USDT", 1.0, TRACK);
    // The crypto-only tick before this one must have had a total and a point,
    // or the assertion below would pass for a plugin that never totals.
    assert_eq!(kernel.last_total_for(TRACK), Some(1.0));
    let before = kernel.pushes.len();

    kernel.set_holding(3, "US:NVDA", 1.0, TRACK);

    assert_eq!(
        kernel.pushes_since(before),
        vec![format!("portfolio.holdings@{TRACK}")],
        "holdings only — no history read, no history write, no history overlay"
    );
    let table = last_holdings_table(&kernel, TRACK);
    let rows = table["rows"].as_array().expect("rows");
    assert_eq!(rows.len(), 3, "both asset rows plus the Total: {rows:?}");
    for row in &rows[..2] {
        assert!(
            row["price"].as_f64().is_some(),
            "both holdings priced, so this tick is COMPLETE: {row}"
        );
    }
    assert!(
        rows[2]["value"].is_null(),
        "231.36 is a number in no currency: {rows:?}"
    );
    assert!(rows[2]["currency"].is_null(), "{rows:?}");
    let caption = table["caption"].as_str().expect("caption");
    assert!(caption.contains("USDT and USD"), "{caption}");
    assert!(caption.contains("no exchange rates"), "{caption}");

    // The stored series still holds only the single-currency point from the
    // first tick: the mixed one added nothing.
    let points = kernel
        .kv
        .get("history/trk_caller")
        .and_then(Value::as_array)
        .expect("a history document");
    assert_eq!(points.len(), 1, "{points:?}");

    // And `market.holdings.list` says the same thing in its own words.
    let listed = kernel.call_tool(4, "market.holdings.list", json!({}), Some(TRACK));
    let text = text_of(&listed);
    assert!(text.contains("1 USDT"), "{text}");
    assert!(text.contains("230.36 USD"), "{text}");
    assert!(text.contains("no exchange rates"), "{text}");
    assert!(
        listed
            .pointer("/result/structuredContent/total")
            .is_some_and(Value::is_null),
        "{listed:#?}"
    );
    assert!(
        listed
            .pointer("/result/structuredContent/currency")
            .is_some_and(Value::is_null),
        "{listed:#?}"
    );
    assert!(kernel.is_responsive());
}
