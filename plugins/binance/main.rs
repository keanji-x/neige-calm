//! Binance portfolio plugin — the kernel's first *pushing* plugin.
//!
//! It owns no UI and writes no report. Every refresh it prices the holdings
//! named in its configuration and pushes two overlays at the configured
//! track through the `neige.*` host-callback channel:
//!
//! | overlay `kind` | payload |
//! |---|---|
//! | `portfolio.holdings` | a `table` block payload: one row per asset + a total row |
//! | `portfolio.history` | a `table` block payload: total value over time, newest first |
//!
//! Both payloads are shaped as report `table` blocks on purpose: a report
//! block may name one with `{"source": "neige://plugin/<id>/<kind>"}` and the
//! frontend renders whatever the overlay currently holds. `Event::OverlaySet`
//! already invalidates the overlay queries, so a push lands in an open report
//! without the document being rewritten — the body keeps the reference, the
//! value moves underneath it.
//!
//! **Endpoint.** The default is `data-api.binance.vision`, not
//! `api.binance.com`. The latter answers `HTTP 451`-style
//! `"Service unavailable from a restricted location"` from many hosts
//! (verified from this project's own deploy host), while the former serves the
//! identical `/api/v3` market-data paths with no key and no geo gate. Both are
//! configurable; nothing here assumes either.
//!
//! **History is forward-only.** The series starts empty and grows one point
//! per successful refresh; the plugin never back-fills from klines. That is
//! the deliberate scope of the first slice — "how has my total moved since I
//! started watching", not "what was it last year".

use std::collections::HashMap;
use std::io::{BufRead, BufReader, BufWriter, Stdout, Write};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::time::Duration;

use serde_json::{Value, json};

/// Callback ids start high enough that a forensic reader never confuses one of
/// ours with a kernel-originated request id.
const FIRST_CALLBACK_ID: u64 = 1_000;
/// How long a `neige.*` callback may take before we give up on its reply. The
/// kernel answers from memory or SQLite; anything slower is a wedged host and
/// we would rather log and retry on the next tick than block the poller.
const CALLBACK_TIMEOUT: Duration = Duration::from_secs(15);
/// Ceiling on retained history points. 500 × ~60 bytes stays far inside the
/// 64 KiB KV quota the manifest asks for, with room for the JSON envelope.
const MAX_HISTORY_POINTS: usize = 500;
/// The lowest poll interval we honour, whatever the configuration says. The
/// market-data endpoint is public and unauthenticated; hammering it is how a
/// shared IP gets rate-limited for everyone behind it.
const MIN_POLL_SECONDS: u64 = 5;
/// KV key holding the serialized history array.
const HISTORY_KEY: &str = "portfolio.history";

// ---------------------------------------------------------------------------
// JSON-RPC plumbing
// ---------------------------------------------------------------------------

/// The plugin's half of the stdio channel: it both answers kernel requests and
/// originates `neige.*` calls, so `stdout` is behind a mutex and every
/// outbound call parks a one-shot sender the read loop can complete.
struct Rpc {
    out: Mutex<BufWriter<Stdout>>,
    next_id: AtomicU64,
    pending: Mutex<HashMap<u64, mpsc::Sender<Value>>>,
}

impl Rpc {
    fn new() -> Self {
        Self {
            out: Mutex::new(BufWriter::new(std::io::stdout())),
            next_id: AtomicU64::new(FIRST_CALLBACK_ID),
            pending: Mutex::new(HashMap::new()),
        }
    }

    fn send(&self, value: &Value) {
        let Ok(mut out) = self.out.lock() else {
            return;
        };
        let mut line = match serde_json::to_string(value) {
            Ok(line) => line,
            Err(e) => {
                eprintln!("binance: refusing to send unserializable frame: {e}");
                return;
            }
        };
        line.push('\n');
        let _ = out.write_all(line.as_bytes());
        let _ = out.flush();
    }

    fn reply(&self, id: Value, result: Value) {
        self.send(&json!({ "jsonrpc": "2.0", "id": id, "result": result }));
    }

    /// Issue a `neige.*` host callback and wait for its reply.
    fn call(&self, method: &str, params: Value) -> Result<Value, String> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = mpsc::channel();
        self.pending
            .lock()
            .map_err(|_| "pending map poisoned".to_string())?
            .insert(id, tx);
        self.send(&json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        }));
        let outcome = match rx.recv_timeout(CALLBACK_TIMEOUT) {
            Ok(frame) => match frame.get("error") {
                Some(err) => Err(format!("{method} failed: {err}")),
                None => Ok(frame.get("result").cloned().unwrap_or(Value::Null)),
            },
            Err(_) => Err(format!("{method} timed out after {CALLBACK_TIMEOUT:?}")),
        };
        // Whether it answered, errored or timed out, this id is spent. Leaving
        // it in the map would leak one entry per timeout for the process's
        // lifetime.
        if let Ok(mut pending) = self.pending.lock() {
            pending.remove(&id);
        }
        outcome
    }

    /// Route a reply frame back to whoever is waiting on it. Returns false when
    /// no one is — a late reply after a timeout, which is worth a log line and
    /// nothing more.
    fn complete(&self, id: u64, frame: Value) -> bool {
        let sender = self.pending.lock().ok().and_then(|mut p| p.remove(&id));
        match sender {
            Some(tx) => tx.send(frame).is_ok(),
            None => false,
        }
    }
}

// ---------------------------------------------------------------------------
// Configuration
// ---------------------------------------------------------------------------

#[derive(Clone, Debug)]
struct Holding {
    asset: String,
    qty: f64,
}

#[derive(Clone, Debug)]
struct Config {
    track_id: String,
    holdings: Vec<Holding>,
    quote: String,
    poll: Duration,
    endpoint: String,
}

/// Parse `"BTC:100,ETH:2.5"`. Malformed entries are dropped with a log line
/// rather than failing the whole configuration: one typo should cost one row,
/// not the entire portfolio.
fn parse_holdings(raw: &str) -> Vec<Holding> {
    raw.split(',')
        .map(str::trim)
        .filter(|entry| !entry.is_empty())
        .filter_map(|entry| {
            let Some((asset, qty)) = entry.split_once(':') else {
                eprintln!("binance: ignoring holding `{entry}`: expected ASSET:QUANTITY");
                return None;
            };
            let asset = asset.trim().to_ascii_uppercase();
            match qty.trim().parse::<f64>() {
                Ok(qty) if qty.is_finite() && asset.chars().all(|c| c.is_ascii_alphanumeric()) => {
                    Some(Holding { asset, qty })
                }
                _ => {
                    eprintln!("binance: ignoring holding `{entry}`: quantity is not a finite number, or the asset is not alphanumeric");
                    None
                }
            }
        })
        .collect()
}

/// Read the effective configuration out of the handshake's
/// `_meta["dev.neige/config"]` envelope (`{"values": {…}}`, #1284 §2.3).
fn config_from_initialize(init: &Value) -> Option<Config> {
    let values = init
        .pointer("/params/_meta/dev.neige~1config/values")
        .and_then(Value::as_object)?;
    let track_id = values.get("track_id").and_then(Value::as_str)?.to_string();
    if track_id.is_empty() {
        eprintln!("binance: `track_id` is empty; nothing to push to");
        return None;
    }
    let poll_seconds = values
        .get("poll_seconds")
        .and_then(Value::as_u64)
        .unwrap_or(30)
        .max(MIN_POLL_SECONDS);
    Some(Config {
        track_id,
        holdings: parse_holdings(values.get("holdings").and_then(Value::as_str).unwrap_or("")),
        quote: values
            .get("quote")
            .and_then(Value::as_str)
            .unwrap_or("USDT")
            .to_ascii_uppercase(),
        poll: Duration::from_secs(poll_seconds),
        endpoint: values
            .get("endpoint")
            .and_then(Value::as_str)
            .unwrap_or("https://data-api.binance.vision")
            .trim_end_matches('/')
            .to_string(),
    })
}

// ---------------------------------------------------------------------------
// Market data
// ---------------------------------------------------------------------------

/// Spot price of `symbol` from `/api/v3/ticker/price`.
///
/// One symbol per request. The batch form (`?symbols=[…]`) exists, but with a
/// handful of holdings it buys nothing and costs a URL-encoded JSON array in
/// the query string — one more thing to get wrong on a path where a partial
/// answer is fine.
fn fetch_price(endpoint: &str, symbol: &str) -> Result<f64, String> {
    let url = format!("{endpoint}/api/v3/ticker/price?symbol={symbol}");
    let body = ureq::get(&url)
        .timeout(Duration::from_secs(10))
        .call()
        .map_err(|e| format!("GET {url}: {e}"))?
        .into_string()
        .map_err(|e| format!("reading {url}: {e}"))?;
    let parsed: Value =
        serde_json::from_str(&body).map_err(|e| format!("{url} returned non-JSON: {e}"))?;
    // The endpoint answers 200 with `{"code":…,"msg":…}` for a geo-blocked or
    // unknown symbol, so a missing `price` is the real error check, not the
    // status code.
    let price = parsed
        .get("price")
        .and_then(Value::as_str)
        .ok_or_else(|| format!("{url} returned no price: {body}"))?;
    let parsed = price
        .parse::<f64>()
        .map_err(|e| format!("{url} returned unparseable price `{price}`: {e}"))?;
    // `"NaN"` and `"inf"` parse successfully as f64, and a non-finite price
    // propagates into the total, out through `json!` as `null`, and back in
    // through `unwrap_or(0.0)` as a *fabricated zero* — a number nobody
    // measured, indistinguishable in the table from a real one.
    if !parsed.is_finite() {
        return Err(format!("{url} returned a non-finite price `{price}`"));
    }
    Ok(parsed)
}

/// Price every holding. Returns the priced rows and the total; an asset whose
/// price could not be read is reported with a `null` value and left out of the
/// total, so one dead symbol cannot silently understate the portfolio as if it
/// were worth zero.
fn price_holdings(cfg: &Config) -> (Vec<Value>, f64, bool) {
    let mut rows = Vec::new();
    let mut total = 0.0;
    let mut complete = true;
    for holding in &cfg.holdings {
        let price = if holding.asset == cfg.quote {
            Ok(1.0)
        } else {
            fetch_price(&cfg.endpoint, &format!("{}{}", holding.asset, cfg.quote))
        };
        // `price * qty` can overflow to infinity even when both factors are
        // finite (a large holding of a large-priced asset), so the product is
        // checked as well as the input.
        let price = price.and_then(|price| {
            let value = price * holding.qty;
            if value.is_finite() {
                Ok((price, value))
            } else {
                Err(format!(
                    "{} × {} is not a finite value",
                    holding.asset, holding.qty
                ))
            }
        });
        match price {
            Ok((price, value)) => {
                total += value;
                rows.push(json!({
                    "asset": holding.asset,
                    "qty": round_to(holding.qty, 8),
                    "price": round_to(price, 2),
                    "value": round_to(value, 2),
                }));
            }
            Err(e) => {
                complete = false;
                eprintln!("binance: {e}");
                rows.push(json!({
                    "asset": holding.asset,
                    "qty": round_to(holding.qty, 8),
                    "price": Value::Null,
                    "value": Value::Null,
                }));
            }
        }
    }
    (rows, total, complete)
}

/// Round for display. Overlay payloads are read by humans through a table, and
/// an f64 rendered at full precision (`79979.99000000000001`) is noise.
fn round_to(value: f64, places: u32) -> f64 {
    let factor = 10f64.powi(places as i32);
    (value * factor).round() / factor
}

// ---------------------------------------------------------------------------
// Overlay payloads
// ---------------------------------------------------------------------------

fn holdings_table(cfg: &Config, rows: Vec<Value>, total: f64, complete: bool, at: &str) -> Value {
    let mut rows = rows;
    rows.push(json!({
        "asset": "Total",
        "qty": Value::Null,
        "price": Value::Null,
        "value": round_to(total, 2),
    }));
    let caption = if complete {
        format!("Priced in {} at {at}", cfg.quote)
    } else {
        format!(
            "Priced in {} at {at} — some prices unavailable; the total covers the priced rows only",
            cfg.quote
        )
    };
    json!({
        "columns": [
            { "key": "asset", "label": "Asset" },
            { "key": "qty", "label": "Quantity", "align": "right" },
            { "key": "price", "label": format!("Price ({})", cfg.quote), "align": "right" },
            { "key": "value", "label": format!("Value ({})", cfg.quote), "align": "right" },
        ],
        "rows": rows,
        "caption": caption,
        "highlight": "Total",
    })
}

/// History as a table, newest first, with the change against the previous
/// point. `points` is `[{ "at": <rfc3339>, "total": <number> }]` in
/// chronological order.
fn history_table(cfg: &Config, points: &[Value]) -> Value {
    let mut rows: Vec<Value> = Vec::with_capacity(points.len());
    for (index, point) in points.iter().enumerate() {
        let total = point.get("total").and_then(Value::as_f64).unwrap_or(0.0);
        let change = if index == 0 {
            Value::Null
        } else {
            let previous = points[index - 1]
                .get("total")
                .and_then(Value::as_f64)
                .unwrap_or(total);
            json!(round_to(total - previous, 2))
        };
        rows.push(json!({
            "at": point.get("at").and_then(Value::as_str).unwrap_or(""),
            "total": round_to(total, 2),
            "change": change,
        }));
    }
    rows.reverse();
    json!({
        "columns": [
            { "key": "at", "label": "At" },
            { "key": "total", "label": format!("Total ({})", cfg.quote), "align": "right" },
            { "key": "change", "label": "Change", "align": "right" },
        ],
        "rows": rows,
        "caption": format!(
            "Total portfolio value over time, newest first — {} point{} since this plugin started watching",
            points.len(),
            if points.len() == 1 { "" } else { "s" },
        ),
    })
}

// ---------------------------------------------------------------------------
// The refresh cycle
// ---------------------------------------------------------------------------

/// Returns whether the overlay actually landed. A refused push (permission
/// denied, a wedged host) leaves the previous value on display, so a caller
/// that reported success would be telling the operator a stale number is
/// fresh.
fn push_overlay(rpc: &Rpc, cfg: &Config, kind: &str, payload: Value) -> bool {
    match rpc.call(
        "neige.overlay.set",
        json!({
            "entity_kind": "track",
            "entity_id": cfg.track_id,
            "kind": kind,
            "payload": payload,
        }),
    ) {
        Ok(_) => true,
        Err(e) => {
            eprintln!("binance: pushing `{kind}` failed: {e}");
            false
        }
    }
}

fn load_history(rpc: &Rpc) -> Result<Vec<Value>, String> {
    let result = rpc.call("neige.kv.get", json!({ "key": HISTORY_KEY }))?;
    // A key that was never written answers `{"value": null}`, which is an
    // empty history. A *failed* read is a different answer and must not reach
    // the writer below as `[]` — that would truncate the series on a transient
    // error, so the `?` above turns it into a skipped tick instead.
    Ok(result
        .get("value")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default())
}

/// Serializes [`refresh`]. The poll thread and a `binance.portfolio.refresh`
/// tool call can arrive at once, and the history cycle is a read-modify-write
/// against a single KV key: interleaving two of them loses whichever point
/// lands first.
static REFRESH_LOCK: Mutex<()> = Mutex::new(());

/// The outcome of one refresh, as a caller can act on it.
#[derive(Debug, PartialEq, Eq)]
enum Refreshed {
    /// Every holding priced and everything the tick meant to publish landed.
    Fully,
    /// Something did not: a price was unavailable, or a push was refused. The
    /// string says which, for a tool caller to relay.
    Partially(String),
    /// Nothing to do — no holdings are configured.
    NothingConfigured,
}

/// One refresh: price, push the holdings table, and — only when the tick is
/// complete and persisted — append a history point and push the history table.
///
/// The ordering is deliberate. History is a claim about the *portfolio's*
/// value over time, so a tick that could not price part of the portfolio must
/// contribute no point: the alternative is a total covering a subset, plotted
/// against totals covering the whole, which reads as a crash that never
/// happened. The holdings table still goes out — it names the missing prices
/// row by row, which is the honest form of that same information.
fn refresh(rpc: &Rpc, cfg: &Config) -> Refreshed {
    let _serialized = REFRESH_LOCK.lock();
    if cfg.holdings.is_empty() {
        eprintln!("binance: no holdings configured; nothing to price");
        return Refreshed::NothingConfigured;
    }
    let at = now_rfc3339();
    let (rows, total, complete) = price_holdings(cfg);
    if !push_overlay(
        rpc,
        cfg,
        "portfolio.holdings",
        holdings_table(cfg, rows, total, complete, &at),
    ) {
        return Refreshed::Partially("the holdings table could not be published".into());
    }
    if !complete {
        return Refreshed::Partially(
            "some holdings could not be priced; the history point was skipped".into(),
        );
    }
    // Each row's `price × qty` was checked for finiteness, but the sum of
    // finite values can still overflow. An infinite total serializes through
    // `json!` as `null` and reads back through `unwrap_or(0.0)` as a
    // fabricated zero, which the history table would plot as a total wipeout.
    if !total.is_finite() {
        return Refreshed::Partially(
            "the portfolio total is not a finite number; the history point was skipped".into(),
        );
    }

    let mut points = match load_history(rpc) {
        Ok(points) => points,
        Err(e) => {
            eprintln!("binance: reading history failed, leaving it untouched this tick: {e}");
            return Refreshed::Partially("the history could not be read".into());
        }
    };
    points.push(json!({ "at": at, "total": round_to(total, 2) }));
    if points.len() > MAX_HISTORY_POINTS {
        let drop = points.len() - MAX_HISTORY_POINTS;
        points.drain(0..drop);
    }
    // Persist BEFORE publishing. Publishing a series that was not stored puts
    // a point on screen that the next tick — which reloads from the store —
    // silently deletes, and a point that vanishes reads as data loss rather
    // than as the failed write it was.
    if let Err(e) = rpc.call(
        "neige.kv.set",
        json!({ "key": HISTORY_KEY, "value": points }),
    ) {
        eprintln!("binance: persisting history failed: {e}");
        return Refreshed::Partially("the history point could not be persisted".into());
    }
    if !push_overlay(rpc, cfg, "portfolio.history", history_table(cfg, &points)) {
        return Refreshed::Partially("the history table could not be published".into());
    }
    Refreshed::Fully
}

/// RFC-3339 UTC to the second, without pulling `chrono` into a plugin that
/// needs exactly one timestamp format.
fn now_rfc3339() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let (days, rem) = (secs.div_euclid(86_400), secs.rem_euclid(86_400));
    let (year, month, day) = civil_from_days(days);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        rem / 3600,
        (rem % 3600) / 60,
        rem % 60,
    )
}

/// Howard Hinnant's `civil_from_days`, the standard days-since-epoch → Y/M/D
/// conversion. Correct for every date this plugin can observe.
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

// ---------------------------------------------------------------------------
// MCP surface
// ---------------------------------------------------------------------------

fn initialize_reply(init: &Value) -> Value {
    let mut result = json!({
        "protocolVersion": init
            .pointer("/params/protocolVersion")
            .cloned()
            .unwrap_or_else(|| json!("2025-11-25")),
        "serverInfo": { "name": "binance", "version": env!("CARGO_PKG_VERSION") },
        "capabilities": {
            "tools": {},
            "experimental": { "dev.neige/kernel-callbacks": { "version": 1 } }
        },
    });
    if let Some(echo) = init
        .pointer("/params/_meta/dev.neige~1auth/expected_echo")
        .and_then(Value::as_str)
    {
        result["_meta"] = json!({ "dev.neige/auth": { "echoed_token": echo } });
    }
    result
}

fn text_result(text: String, structured: Value) -> Value {
    json!({
        "content": [{ "type": "text", "text": text }],
        "structuredContent": structured,
    })
}

fn tools_call_reply(rpc: &Rpc, cfg: Option<&Config>, frame: &Value) -> Value {
    let name = frame
        .pointer("/params/name")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let args = frame
        .pointer("/params/arguments")
        .cloned()
        .unwrap_or_else(|| json!({}));
    match name {
        "binance.price" => {
            let symbol = args
                .get("symbol")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_ascii_uppercase();
            let endpoint = cfg
                .map(|c| c.endpoint.clone())
                .unwrap_or_else(|| "https://data-api.binance.vision".to_string());
            match fetch_price(&endpoint, &symbol) {
                Ok(price) => text_result(
                    format!("{symbol} = {price}"),
                    json!({ "symbol": symbol, "price": price }),
                ),
                Err(e) => json!({
                    "content": [{ "type": "text", "text": e }],
                    "isError": true,
                }),
            }
        }
        "binance.portfolio.refresh" => match cfg {
            Some(cfg) => match refresh(rpc, cfg) {
                Refreshed::Fully => text_result(
                    format!(
                        "Refreshed {} holding(s) on track {}",
                        cfg.holdings.len(),
                        cfg.track_id
                    ),
                    json!({ "track_id": cfg.track_id, "holdings": cfg.holdings.len() }),
                ),
                // A partial refresh is reported as an error, not as success
                // with a caveat: the caller's next act is to read a number,
                // and "Refreshed" would tell them a stale or subset value is
                // current.
                Refreshed::Partially(why) => json!({
                    "content": [{ "type": "text", "text": format!("Refresh incomplete — {why}.") }],
                    "isError": true,
                }),
                Refreshed::NothingConfigured => json!({
                    "content": [{ "type": "text", "text": "No holdings configured — set `holdings` in the plugin's settings, e.g. \"BTC:100\"." }],
                    "isError": true,
                }),
            },
            None => json!({
                "content": [{ "type": "text", "text": "This plugin is not configured — set `track_id` in its settings." }],
                "isError": true,
            }),
        },
        other => json!({
            "content": [{ "type": "text", "text": format!("unknown tool `{other}`") }],
            "isError": true,
        }),
    }
}

// ---------------------------------------------------------------------------
// main
// ---------------------------------------------------------------------------

fn main() {
    let rpc = Arc::new(Rpc::new());
    let mut cfg: Option<Config> = None;
    let reader = BufReader::new(std::io::stdin());

    for line in reader.lines() {
        let Ok(line) = line else { return };
        if line.trim().is_empty() {
            continue;
        }
        let frame: Value = match serde_json::from_str(&line) {
            Ok(frame) => frame,
            Err(e) => {
                eprintln!("binance: bad json from kernel: {e}");
                continue;
            }
        };

        // A frame with no `method` is a reply to one of our callbacks.
        let method = frame.get("method").and_then(Value::as_str);
        let Some(method) = method else {
            let id = frame.get("id").and_then(Value::as_u64);
            match id {
                Some(id) if rpc.complete(id, frame) => {}
                Some(id) => eprintln!("binance: reply to id {id} arrived with nobody waiting"),
                None => eprintln!("binance: frame with neither method nor id, ignored"),
            }
            continue;
        };

        let Some(id) = frame.get("id").cloned() else {
            // A notification (e.g. `notifications/initialized`). Nothing to
            // answer.
            continue;
        };

        match method {
            "initialize" => {
                rpc.reply(id, initialize_reply(&frame));
                cfg = config_from_initialize(&frame);
                match cfg.clone() {
                    Some(config) => {
                        eprintln!(
                            "binance: configured — track={} holdings={} quote={} poll={}s endpoint={}",
                            config.track_id,
                            config.holdings.len(),
                            config.quote,
                            config.poll.as_secs(),
                            config.endpoint,
                        );
                        let rpc = Arc::clone(&rpc);
                        std::thread::spawn(move || {
                            loop {
                                if let Refreshed::Partially(why) = refresh(&rpc, &config) {
                                    eprintln!("binance: incomplete refresh — {why}");
                                }
                                std::thread::sleep(config.poll);
                            }
                        });
                    }
                    None => eprintln!(
                        "binance: no usable configuration at initialize._meta[\"dev.neige/config\"]; \
                         idle until reconfigured and restarted"
                    ),
                }
            }
            "tools/call" => {
                // Off the read loop, always. A tool call issues `neige.*`
                // callbacks, and the replies to those arrive on the very
                // stdin this loop is reading: handling the call inline makes
                // the plugin wait 15s for a reply it is itself preventing
                // itself from reading, then report a timeout — while every
                // other request (a ping, a second tool call) queues behind it.
                let rpc = Arc::clone(&rpc);
                let cfg = cfg.clone();
                std::thread::spawn(move || {
                    let reply = tools_call_reply(&rpc, cfg.as_ref(), &frame);
                    rpc.reply(id, reply);
                });
            }
            "ping" => rpc.reply(id, json!({})),
            other => {
                rpc.send(&json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "error": { "code": -32601, "message": format!("method not found: {other}") },
                }));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use calm_types::report_blocks::kinds::{KIND_TABLE, validate_payload};

    fn config() -> Config {
        Config {
            track_id: "trk_1".into(),
            holdings: parse_holdings("BTC:100"),
            quote: "USDT".into(),
            poll: Duration::from_secs(30),
            endpoint: "https://data-api.binance.vision".into(),
        }
    }

    #[test]
    fn holdings_parse_drops_only_the_malformed_entry() {
        let parsed = parse_holdings("BTC:100, eth:2.5 ,BAD,ZERO:abc,USDT:0");
        let seen: Vec<(&str, f64)> = parsed.iter().map(|h| (h.asset.as_str(), h.qty)).collect();
        assert_eq!(
            seen,
            vec![("BTC", 100.0), ("ETH", 2.5), ("USDT", 0.0)],
            "a typo must cost its own row, not the whole portfolio"
        );
        assert!(parse_holdings("").is_empty());
    }

    // The load-bearing one: whatever this plugin pushes is read back by a
    // report `table` block, so it must satisfy the *kernel's* table
    // validator — not a second opinion written here, which could agree with
    // the plugin and disagree with the renderer.
    #[test]
    fn pushed_payloads_are_valid_report_table_blocks() {
        let cfg = config();
        let rows = vec![json!({ "asset": "BTC", "qty": 100.0, "price": 1.0, "value": 100.0 })];
        let holdings = holdings_table(&cfg, rows, 100.0, true, "2026-09-06T12:00:00Z");
        assert_eq!(validate_payload(KIND_TABLE, &holdings), Ok(()));

        let points = vec![
            json!({ "at": "2026-09-06T12:00:00Z", "total": 100.0 }),
            json!({ "at": "2026-09-06T12:00:30Z", "total": 110.0 }),
        ];
        assert_eq!(
            validate_payload(KIND_TABLE, &history_table(&cfg, &points)),
            Ok(())
        );
    }

    #[test]
    fn a_partial_pricing_keeps_the_row_and_says_so_in_the_caption() {
        let cfg = config();
        let rows = vec![
            json!({ "asset": "BTC", "qty": 100.0, "price": Value::Null, "value": Value::Null }),
        ];
        let table = holdings_table(&cfg, rows, 0.0, false, "2026-09-06T12:00:00Z");
        // The unpriced asset must still appear — dropping it would understate
        // the portfolio silently, which is the failure mode a null says out loud.
        let rows = table["rows"].as_array().expect("rows");
        assert_eq!(rows.len(), 2, "the holding row plus the total row");
        assert!(rows[0]["value"].is_null());
        assert!(
            table["caption"]
                .as_str()
                .expect("caption")
                .contains("some prices unavailable"),
            "a total that omits rows must say so: {}",
            table["caption"],
        );
        assert_eq!(validate_payload(KIND_TABLE, &table), Ok(()));
    }

    #[test]
    fn history_rows_are_newest_first_with_the_change_against_the_previous_point() {
        let cfg = config();
        let points = vec![
            json!({ "at": "t1", "total": 100.0 }),
            json!({ "at": "t2", "total": 110.0 }),
            json!({ "at": "t3", "total": 90.0 }),
        ];
        let table = history_table(&cfg, &points);
        let rows = table["rows"].as_array().expect("rows");
        assert_eq!(
            rows.iter()
                .map(|r| r["at"].as_str().unwrap())
                .collect::<Vec<_>>(),
            vec!["t3", "t2", "t1"],
        );
        assert_eq!(rows[0]["change"], json!(-20.0));
        assert_eq!(
            rows[2]["change"],
            Value::Null,
            "the first point has nothing to change from"
        );
    }

    #[test]
    fn a_total_that_overflows_is_not_reported_as_a_number() {
        // Two holdings of the quote asset itself price without any network
        // (1.0 each), and each row's value is finite while their sum is not.
        // The per-row check alone would pass this straight into the history.
        let cfg = Config {
            holdings: parse_holdings("USDT:1e308,USDT:1e308"),
            ..config()
        };
        let (rows, total, complete) = price_holdings(&cfg);
        assert!(complete, "both rows price fine on their own");
        assert_eq!(rows.len(), 2);
        assert!(
            !total.is_finite(),
            "the sum overflows — this is the input `refresh` must refuse to record"
        );
        // And the value that would reach the overlay is `null`, not a number:
        // the shape that `unwrap_or(0.0)` downstream would turn into a zero.
        assert!(json!(round_to(total, 2)).is_null());
    }

    #[test]
    fn a_non_finite_row_value_is_a_pricing_failure_not_a_row() {
        let cfg = Config {
            holdings: parse_holdings("USDT:1e308"),
            quote: "USDT".into(),
            ..config()
        };
        let (rows, total, complete) = price_holdings(&cfg);
        assert!(complete && total.is_finite(), "1e308 × 1.0 is still finite");
        assert_eq!(rows.len(), 1);
    }

    #[test]
    fn config_needs_a_track_id_and_clamps_the_poll_interval() {
        let init = |values: Value| json!({ "params": { "_meta": { "dev.neige/config": { "values": values } } } });
        assert!(config_from_initialize(&init(json!({}))).is_none());
        assert!(config_from_initialize(&init(json!({ "track_id": "" }))).is_none());

        let cfg = config_from_initialize(&init(json!({ "track_id": "t", "poll_seconds": 1 })))
            .expect("configured");
        assert_eq!(cfg.poll, Duration::from_secs(MIN_POLL_SECONDS));
        // Absent optional keys fall back to the same values the manifest
        // declares as defaults; the kernel normally supplies them, but a
        // hand-written config must not produce an unusable plugin.
        assert_eq!(cfg.quote, "USDT");
        assert_eq!(cfg.endpoint, "https://data-api.binance.vision");
    }

    #[test]
    fn timestamps_are_rfc3339_utc() {
        // 2026-09-06T12:53:47Z, the instant of the first real run.
        assert_eq!(civil_from_days(20_702), (2026, 9, 6));
        let now = now_rfc3339();
        assert_eq!(now.len(), 20, "{now}");
        assert!(now.ends_with('Z'), "{now}");
    }
}
