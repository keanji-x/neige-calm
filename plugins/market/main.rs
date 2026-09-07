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
/// KV key prefix under which each Track's holdings are stored, one document
/// per Track (`holdings/<track_id>`). The prefix is also how the poll loop
/// discovers which Tracks to price at all.
const HOLDINGS_PREFIX: &str = "holdings/";
/// KV key prefix for each Track's value history (`history/<track_id>`).
const HISTORY_PREFIX: &str = "history/";

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
                eprintln!("market: refusing to send unserializable frame: {e}");
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
// Asset identity
// ---------------------------------------------------------------------------

/// Where an asset trades. A name alone does not identify a security: `W` is
/// Wayfair on the NYSE and Wormhole on a crypto exchange, and a plugin that
/// guesses between them prices one as the other by a factor of thousands.
///
/// There is deliberately no rule that infers a venue from the SHAPE of a name
/// — no "six digits means Shanghai", no "four digits means Hong Kong". Every
/// such rule has counterexamples on real tickers, and a wrong guess here is a
/// silently wrong number in a total, not a visible failure.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum Venue {
    Crypto,
    Us,
    Hk,
    Cn,
}

impl Venue {
    /// The prefix a caller writes, and the one a canonical identity carries.
    fn prefix(self) -> &'static str {
        match self {
            Venue::Crypto => "CRYPTO",
            Venue::Us => "US",
            Venue::Hk => "HK",
            Venue::Cn => "CN",
        }
    }

    fn from_prefix(prefix: &str) -> Option<Self> {
        match prefix {
            "CRYPTO" => Some(Venue::Crypto),
            "US" => Some(Venue::Us),
            "HK" => Some(Venue::Hk),
            "CN" => Some(Venue::Cn),
            _ => None,
        }
    }
}

/// A venue plus the symbol that venue itself uses (`NVDA`, `1810`, `600519`).
///
/// The canonical spelling is `<VENUE>:<SYMBOL>`, upper-case, and it is what
/// gets stored, compared and cached. Everything downstream of
/// [`parse_asset`] lives in that one space: `holdings.retain`, the price
/// cache key and the priced rows all compare identities, never raw input.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct AssetId {
    venue: Venue,
    /// The symbol AS THE VENUE SPELLS IT — never prefixed. This is what a
    /// provider URL is built from; the canonical string is what a reader and
    /// the KV see.
    symbol: String,
}

impl AssetId {
    fn canonical(&self) -> String {
        format!("{}:{}", self.venue.prefix(), self.symbol)
    }
}

/// What a caller is told when [`parse_asset`] refuses. One string, shared by
/// both tools, so the two cannot describe two different grammars.
const ASSET_SYNTAX_ERROR: &str = "`asset` must be a name like \"BTC\", or a \
    venue-qualified \"<VENUE>:<SYMBOL>\" over the venues CRYPTO, US, HK and CN. \
    A name with no venue is a crypto asset. Only CRYPTO has a price source \
    today: a name on US, HK or CN can be recorded, but nothing prices it yet.";

/// The one place an asset name becomes an identity.
///
/// All three entry points call THIS — `Holding::from_json` (which is both the
/// KV read path and the write-back path), `market.holdings.set`'s argument
/// check, and `market.quote`'s argument check. They used to carry three
/// different rules (the third checked only for emptiness), and a second copy
/// of this grammar written next to any of them would drift from it.
///
/// The grammar, whole:
///
/// - Upper-cased and trimmed first, so `crypto:btc` and `CRYPTO:BTC` are one
///   identity. The canonical form is upper-case because that is what the
///   entry points already stored.
/// - `<VENUE>:<SYMBOL>` with a KNOWN venue prefix, symbol `[A-Z0-9]+`.
///   An unknown prefix is rejected outright rather than treated as a bare
///   name: `FOO:BAR` is a typo, and pricing it as crypto `FOO:BAR` — or
///   worse, as something else — is how a wrong number gets published.
/// - A name with NO `:` is crypto. **This is frozen and not configurable.**
///   Every row a pre-venues `market.holdings.set` wrote is a bare name, and
///   a configuration knob that reinterpreted them would silently reprice an
///   existing portfolio when an operator flipped it.
/// - Because the split is on `:` and nothing else, `USNVDA`, `HK1810` and
///   `CRYPTOBTC` stay bare crypto names. A prefix is only a prefix when the
///   colon is there.
fn parse_asset(raw: &str) -> Option<AssetId> {
    let raw = raw.trim().to_ascii_uppercase();
    let (venue, symbol) = match raw.split_once(':') {
        Some((prefix, symbol)) => (Venue::from_prefix(prefix)?, symbol),
        None => (Venue::Crypto, raw.as_str()),
    };
    if symbol.is_empty() || !symbol.chars().all(|c| c.is_ascii_alphanumeric()) {
        return None;
    }
    Some(AssetId {
        venue,
        symbol: symbol.to_string(),
    })
}

// ---------------------------------------------------------------------------
// Configuration
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq)]
struct Holding {
    /// The canonical identity, parsed. Stored parsed rather than as a string
    /// so that no consumer re-splits it — a second copy of the venue rule
    /// downstream is exactly what [`parse_asset`] exists to prevent.
    asset: AssetId,
    quantity: f64,
}

impl Holding {
    /// Parse one stored row, NORMALISING its asset to the canonical identity
    /// on the way in.
    ///
    /// This is the read side, and normalising here is what keeps every
    /// downstream comparison in one space: `holdings.retain`
    /// (`market.holdings.set`), the per-pass price cache key and the priced
    /// rows all see `CRYPTO:BTC` whether the KV said `BTC`, `btc` or
    /// `CRYPTO:BTC`.
    ///
    /// **This is a write-triggered gradual migration, not the absence of
    /// one.** There is no batch scan: a scan would have to read the whole
    /// array and write it back, and `neige.kv.set` (`store_holdings`) is a
    /// whole-array overwrite with no CAS — a scan racing a concurrent `set`
    /// would silently drop the `set`. Instead each Track migrates on its own
    /// first write: `market.holdings.set` does load (normalised here) →
    /// `retain` → `store_holdings`, and that overwrite lands the canonical
    /// spelling. So a Track that is never written again keeps its legacy
    /// spelling in the KV indefinitely, and **the KV stays a mixed space for
    /// as long as that is true**. No read path IN THIS PLUGIN gets at the
    /// stored holdings without coming through here, so the mixture is
    /// invisible above this function — but it is real, and anything that
    /// inspects the stored JSON directly (a test, an operator, anything else
    /// holding this plugin's KV) must expect both spellings.
    fn from_json(value: &Value) -> Option<Self> {
        let asset = parse_asset(value.get("asset")?.as_str()?)?;
        let quantity = value.get("quantity")?.as_f64()?;
        if !quantity.is_finite() || quantity <= 0.0 {
            return None;
        }
        Some(Self { asset, quantity })
    }

    fn to_json(&self) -> Value {
        json!({ "asset": self.asset.canonical(), "quantity": self.quantity })
    }
}

#[derive(Clone, Debug)]
struct Config {
    quote: String,
    poll: Duration,
    binance_endpoint: String,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            quote: "USDT".into(),
            poll: Duration::from_secs(30),
            binance_endpoint: "https://data-api.binance.vision".into(),
        }
    }
}

/// Read the effective configuration out of the handshake's
/// `_meta["dev.neige/config"]` envelope (`{"values": {…}}`, #1284 §2.3).
///
/// Every key is optional and every default matches the manifest's, so an
/// unconfigured install is a working install. Nothing here names a Track or a
/// holding any more: those are per-Track facts an agent writes through the
/// tools, not settings an operator maintains.
fn config_from_initialize(init: &Value) -> Config {
    let mut cfg = Config::default();
    let Some(values) = init
        .pointer("/params/_meta/dev.neige~1config/values")
        .and_then(Value::as_object)
    else {
        return cfg;
    };
    if let Some(quote) = values.get("quote").and_then(Value::as_str)
        && !quote.trim().is_empty()
    {
        cfg.quote = quote.trim().to_ascii_uppercase();
    }
    if let Some(poll) = values.get("poll_seconds").and_then(Value::as_u64) {
        cfg.poll = Duration::from_secs(poll.max(MIN_POLL_SECONDS));
    }
    if let Some(endpoint) = values.get("binance_endpoint").and_then(Value::as_str)
        && !endpoint.trim().is_empty()
    {
        cfg.binance_endpoint = endpoint.trim().trim_end_matches('/').to_string();
    }
    cfg
}

/// The Track a `tools/call` was made from, as the kernel reported it.
///
/// Read from `params._meta["dev.neige/track"].id` and nowhere else. The
/// arguments of a tool call are written by the calling agent, so a Track named
/// there would be a Track the agent chose — including one belonging to someone
/// else. The kernel fills this namespace from the identity it already resolved.
fn track_from_call(frame: &Value) -> Option<String> {
    frame
        .pointer("/params/_meta/dev.neige~1track/id")
        .and_then(Value::as_str)
        .filter(|id| !id.is_empty())
        .map(str::to_string)
}

fn holdings_key(track_id: &str) -> String {
    format!("{HOLDINGS_PREFIX}{track_id}")
}

fn history_key(track_id: &str) -> String {
    format!("{HISTORY_PREFIX}{track_id}")
}

/// A stored entry that no longer parses is dropped with a log line rather than
/// failing the read: one bad row must not make a Track's whole portfolio
/// unreadable, and the row it drops is one nothing could have priced anyway.
fn holdings_from_value(value: Option<&Value>, track_id: &str) -> Vec<Holding> {
    let Some(items) = value.and_then(Value::as_array) else {
        return Vec::new();
    };
    items
        .iter()
        .filter_map(|item| {
            let parsed = Holding::from_json(item);
            if parsed.is_none() {
                eprintln!("market: dropping unreadable holding on {track_id}: {item}");
            }
            parsed
        })
        .collect()
}

/// Read one Track's holdings.
fn load_holdings(rpc: &Rpc, track_id: &str) -> Result<Vec<Holding>, String> {
    let result = rpc.call("neige.kv.get", json!({ "key": holdings_key(track_id) }))?;
    Ok(holdings_from_value(result.get("value"), track_id))
}

/// The exact JSON `store_holdings` writes. Split out so what lands in the KV
/// can be asserted on without a fake kernel.
fn store_value(holdings: &[Holding]) -> Value {
    Value::Array(holdings.iter().map(Holding::to_json).collect())
}

fn store_holdings(rpc: &Rpc, track_id: &str, holdings: &[Holding]) -> Result<(), String> {
    rpc.call(
        "neige.kv.set",
        json!({ "key": holdings_key(track_id), "value": store_value(holdings) }),
    )?;
    Ok(())
}

/// Every Track this plugin holds a portfolio for.
///
/// The poll loop has no other way to know which Tracks to price: holdings
/// arrive through tool calls, at any time, from any Track. Only the names are
/// taken — the values in the listing are a snapshot, and [`refresh`] re-reads
/// each Track's own document under the lock rather than trusting one.
fn portfolios(rpc: &Rpc) -> Result<Vec<String>, String> {
    let result = rpc.call("neige.kv.list", json!({ "prefix": HOLDINGS_PREFIX }))?;
    let entries = result
        .get("entries")
        .and_then(Value::as_array)
        .ok_or_else(|| format!("neige.kv.list returned no `entries` array: {result}"))?;
    Ok(entries
        .iter()
        .filter_map(|entry| entry.get("key").and_then(Value::as_str))
        .filter_map(|key| key.strip_prefix(HOLDINGS_PREFIX))
        .filter(|track_id| !track_id.is_empty())
        .map(str::to_string)
        .collect())
}

// ---------------------------------------------------------------------------
// Providers
// ---------------------------------------------------------------------------
//
// A caller names an ASSET IDENTITY (`CRYPTO:BTC`, `US:NVDA`). At which URL and
// in which response shape a venue answers is this plugin's business and never
// the caller's — the day a US-equity provider lands, an identity that is
// already qualified with its venue does not have to be renamed: a name that
// used to resolve nowhere starts resolving.
//
// What the caller DOES say is the venue, because a name alone does not
// identify a security (see [`parse_asset`]). Still no per-provider priority
// list and no configuration surface for routing: one identity has one venue,
// and the venue is written down rather than guessed.
//
// Only one provider exists today. It serves the crypto venue, and its
// resolution rule is one line: `<SYMBOL><QUOTE>` is a Binance spot symbol,
// built from the venue-local symbol. Identities on the OTHER venues are not
// sent to it — they answer `Unknown`, which renders as a null row a reader
// can see. Routing them to Binance would be worse than useless: `US:BTC`
// would come back with bitcoin's price attached to a US-listing identity,
// and a fabricated number in a total is the failure this whole identity
// layer exists to prevent. `Unknown` is also exactly the answer a later
// provider turns into a price without the stored identity being renamed.
//
// KNOWN GAP (S1). US, HK and CN have no source at all in this slice, so an
// identity on one of them answers `Unknown` on every pass. `price_holdings`
// reports that as an unpriced row and clears `complete`, and `refresh` skips
// the history point whenever `complete` is false — so a Track holding ONE
// such identity contributes no history point for as long as it holds it, and
// its series stands still. That stall is not new with venues (a bare crypto
// name the venue does not list has always come back `Unknown` the same way),
// but it is now reachable by recording a name this plugin's own grammar
// accepts. It closes when a source for those venues lands; until then the
// tool descriptions and the README say so rather than the code refusing the
// name.

/// What a provider answered, or why it could not.
#[derive(Clone)]
enum Quote {
    /// A positive, finite price in the configured quote asset.
    Price(f64),
    /// The provider does not know this asset. Distinct from a failure: it is
    /// the answer that a future provider would turn into a price, and the
    /// answer a caller can act on by checking the name.
    Unknown,
    /// The lookup itself failed — network, malformed response, a venue error.
    Failed(String),
}

/// Price one asset. `1.0` for the quote asset itself, which needs no venue and
/// no network.
///
/// The "is this the quote asset" test compares the CRYPTO venue-local symbol
/// against `cfg.quote`, not the canonical identity: `cfg.quote` is a bare
/// venue-local name that no entry point parses or validates. For any value
/// spelled without a venue prefix — the default `USDT` included — comparing
/// it against the canonical `CRYPTO:USDT` would not match, and a stablecoin
/// holding would go to the venue and be looked up as `USDTUSDT`.
///
/// Only a crypto identity can match. A holding of `US:USDT` is a different
/// asset that happens to share a symbol, and answering `1.0` for it would be
/// a fabricated price.
fn quote_asset(cfg: &Config, asset: &AssetId) -> Quote {
    if let Some(price) = quote_asset_shortcut(cfg, asset) {
        return Quote::Price(price);
    }
    match asset.venue {
        Venue::Crypto => binance_spot(cfg, asset),
        // No source serves these venues yet. Saying so is the whole answer:
        // `Unknown` is the one a provider added later turns into a price.
        Venue::Us | Venue::Hk | Venue::Cn => Quote::Unknown,
    }
}

/// The half of [`quote_asset`] that needs neither venue nor network, split out
/// so it can be asserted on directly.
fn quote_asset_shortcut(cfg: &Config, asset: &AssetId) -> Option<f64> {
    (asset.venue == Venue::Crypto && asset.symbol == cfg.quote).then_some(1.0)
}

/// One asset is priced once per pass, however many Tracks hold it.
///
/// Without this, a pass costs one request per holding per Track — three Tracks
/// watching BTC would ask three times for the same number within the same
/// second, and the pass would take three times as long to finish. The cache
/// lives for exactly one pass so a later pass always re-reads the market.
type PriceCache = HashMap<AssetId, Quote>;

fn quote_cached(cfg: &Config, asset: &AssetId, cache: &mut PriceCache) -> Quote {
    if let Some(hit) = cache.get(asset) {
        return hit.clone();
    }
    let quote = quote_asset(cfg, asset);
    cache.insert(asset.clone(), quote.clone());
    quote
}

/// The spot symbol Binance is asked about.
///
/// Built from the VENUE-LOCAL symbol, never from the canonical identity: a
/// Binance spot symbol is `<ASSET><QUOTE>`, and `CRYPTO:BTCUSDT` is not one.
///
/// KNOWN GAP, unchanged by the identity layer and deliberately not fixed
/// here. `cfg.quote` goes through no grammar at all — `config_from_initialize`
/// only trims and upper-cases it — and concatenation cannot be undone:
///
/// * `quote = "USDT#X"` with a `BTC` holding builds `BTCUSDT#X`; the `#X` is
///   a URL fragment, so the server is asked about `BTCUSDT`, answers, and the
///   price is then labelled and captioned `USDT#X`. The unit on screen is not
///   the unit the number is in.
/// * `quote = "T"` with an `ETHUSD` holding builds `ETHUSDT`, the same string
///   an `ETH` holding under `quote = "USDT"` builds. Two different requests
///   are indistinguishable once concatenated.
///
/// Both were reachable byte for byte before this slice and neither is made
/// more likely by it: the identity grammar constrains the ASSET, and
/// `cfg.quote` is not an asset.
fn binance_symbol(cfg: &Config, asset: &AssetId) -> String {
    format!("{}{}", asset.symbol, cfg.quote)
}

/// Binance spot, via `/api/v3/ticker/price`.
///
/// The endpoint answers `200` with a `{"code":…,"msg":…}` body for an unknown
/// symbol *and* for a geo-blocked caller, so the status code says nothing and
/// the absence of `price` is the real check.
fn binance_spot(cfg: &Config, asset: &AssetId) -> Quote {
    let symbol = binance_symbol(cfg, asset);
    let url = format!(
        "{}/api/v3/ticker/price?symbol={symbol}",
        cfg.binance_endpoint
    );
    let response = match ureq::get(&url).timeout(Duration::from_secs(10)).call() {
        Ok(response) => response,
        Err(ureq::Error::Status(_, response)) => response,
        Err(e) => return Quote::Failed(format!("GET {url}: {e}")),
    };
    let body = match response.into_string() {
        Ok(body) => body,
        Err(e) => return Quote::Failed(format!("reading {url}: {e}")),
    };
    let parsed: Value = match serde_json::from_str(&body) {
        Ok(parsed) => parsed,
        Err(e) => return Quote::Failed(format!("{url} returned non-JSON: {e}")),
    };
    let Some(price) = parsed.get("price").and_then(Value::as_str) else {
        // `-1121 Invalid symbol` is "we do not list this", which is Unknown.
        // Anything else under `code` is a failure of the lookup, and its
        // message is NOT quoted back: the body is attacker-influenced text
        // that would otherwise ride into an overlay a reader trusts.
        return match parsed.get("code").and_then(Value::as_i64) {
            Some(-1121) => Quote::Unknown,
            Some(code) => {
                Quote::Failed(format!("{symbol}: venue refused the lookup (code {code})"))
            }
            None => Quote::Failed(format!("{symbol}: response carried no price")),
        };
    };
    match price.parse::<f64>() {
        // Finite is not enough: `0` and negative parse fine and would price a
        // holding at or below nothing, which no spot market means.
        Ok(price) if price.is_finite() && price > 0.0 => Quote::Price(price),
        _ => Quote::Failed(format!(
            "{symbol}: `{price}` is not a positive finite price"
        )),
    }
}

/// Price every holding of one portfolio.
///
/// Returns the table rows, the total, and whether every holding priced. An
/// asset that could not be priced keeps its row with `null` price and value —
/// dropping it would understate the portfolio silently, which is exactly what
/// a null says out loud — and is left out of the total.
fn price_holdings(
    cfg: &Config,
    holdings: &[Holding],
    cache: &mut PriceCache,
) -> (Vec<Value>, f64, bool) {
    let mut rows = Vec::with_capacity(holdings.len());
    let mut total = 0.0;
    let mut complete = true;
    for holding in holdings {
        // `price * quantity` can overflow to infinity even when both factors
        // are finite, so the product is checked as well as the input.
        let priced = match quote_cached(cfg, &holding.asset, cache) {
            Quote::Price(price) => {
                let value = price * holding.quantity;
                if value.is_finite() {
                    Ok((price, value))
                } else {
                    Err(format!(
                        "{} × {} is not a finite value",
                        holding.asset.canonical(),
                        holding.quantity
                    ))
                }
            }
            Quote::Unknown => Err(format!(
                "no configured source prices {}",
                holding.asset.canonical()
            )),
            Quote::Failed(why) => Err(why),
        };
        match priced {
            Ok((price, value)) => {
                total += value;
                rows.push(json!({
                    "asset": holding.asset.symbol,
                    "venue": holding.asset.venue.prefix(),
                    "qty": round_to(holding.quantity, 8),
                    "price": round_to(price, 2),
                    "value": round_to(value, 2),
                }));
            }
            Err(e) => {
                complete = false;
                eprintln!("market: {e}");
                rows.push(json!({
                    "asset": holding.asset.symbol,
                    "venue": holding.asset.venue.prefix(),
                    "qty": round_to(holding.quantity, 8),
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
///
/// Rounding is the one arithmetic step that can *create* an infinity out of a
/// finite input: `1e308 * 100` overflows. That mattered — every finiteness
/// guard upstream would pass, and the infinity would serialize as `null` and
/// read back as a fabricated zero, which is the exact defect those guards
/// exist to prevent. A value too large to scale is therefore returned
/// unrounded rather than rounded to infinity: the display loses two decimals
/// it never had at that magnitude, and finiteness — which the caller checks —
/// is preserved.
fn round_to(value: f64, places: u32) -> f64 {
    let factor = 10f64.powi(places as i32);
    let scaled = value * factor;
    if !scaled.is_finite() {
        return value;
    }
    scaled.round() / factor
}

// ---------------------------------------------------------------------------
// Overlay payloads
// ---------------------------------------------------------------------------

/// `total` is `None` when the values could not be summed into a finite
/// number. The table still goes out in that case: it is the only place the
/// per-asset rows appear, and withholding it would leave whatever was
/// published last on screen, presented as current.
fn holdings_table(
    cfg: &Config,
    rows: Vec<Value>,
    total: Option<f64>,
    complete: bool,
    at: &str,
) -> Value {
    let mut rows = rows;
    rows.push(json!({
        "asset": "Total",
        "venue": Value::Null,
        "qty": Value::Null,
        "price": Value::Null,
        "value": total.map(|total| round_to(total, 2)),
    }));
    let caption = match (total, complete) {
        (None, _) => format!(
            "Priced in {} at {at} — the values do not sum to a finite number, so no total is shown",
            cfg.quote
        ),
        (Some(_), true) => format!("Priced in {} at {at}", cfg.quote),
        (Some(_), false) => format!(
            "Priced in {} at {at} — some prices unavailable; the total covers the priced rows only",
            cfg.quote
        ),
    };
    json!({
        "columns": [
            { "key": "asset", "label": "Asset" },
            // The venue is its own column rather than a prefix glued onto the
            // name: `W` on two venues is two different companies, and a
            // reader has to be able to tell which row is which.
            { "key": "venue", "label": "Venue" },
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
fn push_overlay(rpc: &Rpc, track_id: &str, kind: &str, payload: Value) -> bool {
    match rpc.call(
        "neige.overlay.set",
        json!({
            "entity_kind": "track",
            "entity_id": track_id,
            "kind": kind,
            "payload": payload,
        }),
    ) {
        Ok(_) => true,
        Err(e) => {
            eprintln!("market: pushing `{kind}` failed: {e}");
            false
        }
    }
}

fn load_history(rpc: &Rpc, track_id: &str) -> Result<Vec<Value>, String> {
    let result = rpc.call("neige.kv.get", json!({ "key": history_key(track_id) }))?;
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

/// Serializes [`refresh`]. The poll thread and a tool call can arrive at once,
/// and the history cycle is a read-modify-write against a single KV key:
/// interleaving two of them loses whichever point lands first.
///
/// One lock for every Track rather than one per Track: refreshes are short and
/// infrequent, and a global lock cannot deadlock against itself the way a
/// per-Track map of locks can be made to.
static REFRESH_LOCK: Mutex<()> = Mutex::new(());

/// The outcome of one refresh, as a caller can act on it.
#[derive(Debug, PartialEq, Eq)]
enum Refreshed {
    /// Every holding priced and everything the tick meant to publish landed.
    Fully,
    /// Something did not: a price was unavailable, or a push was refused. The
    /// string says which, for a tool caller to relay.
    Partially(String),
    /// Nothing to do — this Track holds nothing.
    NothingHeld,
}

/// One refresh of one Track: price, push the holdings table, and — only when
/// the tick is complete and persisted — append a history point and push the
/// history table.
///
/// The ordering is deliberate. History is a claim about the *portfolio's*
/// value over time, so a tick that could not price part of the portfolio must
/// contribute no point: the alternative is a total covering a subset, plotted
/// against totals covering the whole, which reads as a crash that never
/// happened. The holdings table still goes out — it names the missing prices
/// row by row, which is the honest form of that same information.
fn refresh(rpc: &Rpc, cfg: &Config, track_id: &str, cache: &mut PriceCache) -> Refreshed {
    let _serialized = REFRESH_LOCK.lock();
    // Read the holdings HERE, inside the lock, rather than taking them from
    // the caller. A poll pass lists every portfolio up front and then prices
    // them one at a time; by the time a slow pass reaches this Track, a tool
    // call may already have changed and re-published it. Publishing the
    // caller's snapshot would overwrite that newer state with an older one and
    // append its obsolete total to the history — a portfolio appearing to
    // revert on its own. Re-reading costs one round trip and removes the
    // window entirely.
    let holdings = match load_holdings(rpc, track_id) {
        Ok(holdings) => holdings,
        Err(e) => {
            eprintln!("market: reading {track_id}'s holdings failed: {e}");
            return Refreshed::Partially("this Track's holdings could not be read".into());
        }
    };
    let at = now_rfc3339();

    // An empty portfolio still publishes. Returning early would leave the
    // last non-empty table on screen for a Track that now holds nothing —
    // someone who has just sold out would keep seeing their old position,
    // which is a worse lie than an empty table. No history point: the series
    // is about a portfolio's value, and there is no portfolio to value.
    if holdings.is_empty() {
        return if push_overlay(
            rpc,
            track_id,
            "portfolio.holdings",
            holdings_table(cfg, Vec::new(), Some(0.0), true, &at),
        ) {
            Refreshed::NothingHeld
        } else {
            Refreshed::Partially("the (now empty) holdings table could not be published".into())
        };
    }

    let (rows, total, complete) = price_holdings(cfg, &holdings, cache);

    // Each row's `price × qty` was checked for finiteness, but the sum of
    // finite values can still overflow. `None` keeps the per-asset rows —
    // which are exactly what a reader needs when the total is impossible —
    // while refusing to state a total that is not a number.
    let summed = total.is_finite().then_some(total);
    if summed.is_none() {
        eprintln!("market: {track_id}'s total is {total}; publishing the rows without it");
    }
    if !push_overlay(
        rpc,
        track_id,
        "portfolio.holdings",
        holdings_table(cfg, rows, summed, complete, &at),
    ) {
        return Refreshed::Partially("the holdings table could not be published".into());
    }
    if !complete {
        return Refreshed::Partially(
            "some holdings could not be priced; the history point was skipped".into(),
        );
    }
    let Some(total) = summed else {
        return Refreshed::Partially(
            "the portfolio total is not a finite number; the history point was skipped".into(),
        );
    };

    let mut points = match load_history(rpc, track_id) {
        Ok(points) => points,
        Err(e) => {
            eprintln!("market: reading {track_id}'s history failed, leaving it untouched: {e}");
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
        json!({ "key": history_key(track_id), "value": points }),
    ) {
        eprintln!("market: persisting {track_id}'s history failed: {e}");
        return Refreshed::Partially("the history point could not be persisted".into());
    }
    if !push_overlay(
        rpc,
        track_id,
        "portfolio.history",
        history_table(cfg, &points),
    ) {
        return Refreshed::Partially("the history table could not be published".into());
    }
    Refreshed::Fully
}

/// One pass over every Track that holds something.
///
/// The listing is only used to name the Tracks; each one's holdings are read
/// again under the lock (see [`refresh`]). One price cache spans the pass, so
/// an asset several Tracks hold is fetched once.
fn refresh_all(rpc: &Rpc, cfg: &Config) {
    let track_ids = match portfolios(rpc) {
        Ok(portfolios) => portfolios,
        Err(e) => {
            eprintln!("market: listing portfolios failed; skipping this pass: {e}");
            return;
        }
    };
    let mut cache = PriceCache::new();
    for track_id in track_ids {
        if let Refreshed::Partially(why) = refresh(rpc, cfg, &track_id, &mut cache) {
            eprintln!("market: incomplete refresh of {track_id} — {why}");
        }
    }
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
        "serverInfo": { "name": "market", "version": env!("CARGO_PKG_VERSION") },
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

/// The one-line prose `market.holdings.list` answers with, built from the
/// rows [`price_holdings`] produced.
///
/// The venue is glued to the symbol HERE, unlike in the table, because this
/// exit has no columns to put it in: `1 × W` names Wayfair and Wormhole
/// equally well, and a Planner reading the line has nothing else to go on.
/// Split out from the tool arm so it can be asserted on without a kernel.
fn holdings_line(cfg: &Config, rows: &[Value]) -> String {
    rows.iter()
        .map(|row| {
            let value = row["value"]
                .as_f64()
                .map(|v| format!("{v} {}", cfg.quote))
                .unwrap_or_else(|| "price unavailable".into());
            let venue = row["venue"].as_str().unwrap_or("?");
            let asset = row["asset"].as_str().unwrap_or("?");
            format!("{} × {venue}:{asset} = {value}", row["qty"])
        })
        .collect::<Vec<_>>()
        .join("; ")
}

fn tool_error(text: impl Into<String>) -> Value {
    json!({ "content": [{ "type": "text", "text": text.into() }], "isError": true })
}

/// Every tool here needs to know which Track it is acting for, and takes that
/// from the kernel's `_meta` — never from its arguments. A call with no Track
/// is a call from somewhere that has none (a direct daemon connection), and it
/// is refused rather than defaulted: silently acting on some other Track is
/// the failure this whole namespace exists to prevent.
fn tools_call_reply(rpc: &Rpc, cfg: &Config, wake: &mpsc::Sender<()>, frame: &Value) -> Value {
    let name = frame
        .pointer("/params/name")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let args = frame
        .pointer("/params/arguments")
        .cloned()
        .unwrap_or_else(|| json!({}));

    if name == "market.quote" {
        let raw = args
            .get("asset")
            .and_then(Value::as_str)
            .unwrap_or_default();
        // Same parse as the KV read path and as `market.holdings.set`. This
        // entry point used to check only for emptiness, so
        // `market.quote{asset:"1810.HK"}` went straight into a Binance symbol.
        let Some(asset) = parse_asset(raw) else {
            return tool_error(ASSET_SYNTAX_ERROR);
        };
        let canonical = asset.canonical();
        return match quote_asset(cfg, &asset) {
            Quote::Price(price) => text_result(
                format!("{canonical} = {price} {}", cfg.quote),
                json!({
                    "asset": asset.symbol,
                    "venue": asset.venue.prefix(),
                    "price": price,
                    "quote": cfg.quote,
                }),
            ),
            Quote::Unknown => tool_error(format!(
                "No configured source prices `{canonical}` — either its venue has no \
                 source yet, or the source it has does not list this symbol."
            )),
            Quote::Failed(why) => tool_error(format!("Could not price {canonical} — {why}.")),
        };
    }

    let Some(track_id) = track_from_call(frame) else {
        return tool_error(
            "This tool acts on the Track it is called from, and this call carries no Track.",
        );
    };

    match name {
        "market.holdings.set" => {
            let raw = args
                .get("asset")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let quantity = args.get("quantity").and_then(Value::as_f64);
            // The same parse the KV read path uses, so what this tool accepts
            // and what a stored row can spell cannot drift apart.
            let Some(asset) = parse_asset(raw) else {
                return tool_error(ASSET_SYNTAX_ERROR);
            };
            let Some(quantity) = quantity.filter(|q| q.is_finite() && *q >= 0.0) else {
                return tool_error("`quantity` must be a finite number of zero or more.");
            };
            let mut holdings = match load_holdings(rpc, &track_id) {
                Ok(holdings) => holdings,
                Err(e) => {
                    return tool_error(format!("Could not read this Track's holdings — {e}."));
                }
            };
            // Both sides are canonical identities: `holdings` came through
            // `Holding::from_json`, which normalises, and `asset` came
            // through the same parse. A legacy `BTC` row and an incoming
            // `crypto:BTC` are therefore one holding, not two summed rows.
            holdings.retain(|h| h.asset != asset);
            // Zero is the spelling of "no longer held": one tool, and no way
            // to be left holding a position of nothing.
            if quantity > 0.0 {
                holdings.push(Holding {
                    asset: asset.clone(),
                    quantity,
                });
            }
            holdings.sort_by_key(|h| h.asset.canonical());
            if let Err(e) = store_holdings(rpc, &track_id, &holdings) {
                return tool_error(format!("Could not save this Track's holdings — {e}."));
            }
            // Recording a holding does NOT price it here. Pricing is a network
            // call, and a tool that touches the open world needs
            // `openWorldHint: true`, which is what makes codex demand approval
            // (`requires_mcp_tool_approval`) — and the kernel spawns agents
            // with `approval_policy: "never"`, so such a tool is not "gated",
            // it is *unusable*. Found by running it: the Planner produced a
            // perfectly-formed call and got back "requires approval, but
            // approval policy is never".
            //
            // So this writes state and wakes the poll thread, which does the
            // pricing a moment later. The tool stays honestly annotated
            // (`openWorldHint: false`), the reader still gets a fresh table
            // within a second or two, and nothing has to lie about what it
            // touches.
            let _ = wake.send(());
            let canonical = asset.canonical();
            let summary = if quantity > 0.0 {
                format!("Holding {quantity} {canonical}")
            } else {
                format!("No longer holding {canonical}")
            };
            text_result(
                format!(
                    "{summary}. {} asset(s) tracked; the tables refresh in a moment.",
                    holdings.len()
                ),
                json!({ "holdings": holdings.iter().map(Holding::to_json).collect::<Vec<_>>() }),
            )
        }
        "market.holdings.list" => {
            let holdings = match load_holdings(rpc, &track_id) {
                Ok(holdings) => holdings,
                Err(e) => {
                    return tool_error(format!("Could not read this Track's holdings — {e}."));
                }
            };
            if holdings.is_empty() {
                return text_result(
                    "This Track holds nothing yet.".into(),
                    json!({ "holdings": [] }),
                );
            }
            let (rows, total, complete) = price_holdings(cfg, &holdings, &mut PriceCache::new());
            let text = holdings_line(cfg, &rows);
            text_result(
                if complete && total.is_finite() {
                    format!("{text}. Total {} {}.", round_to(total, 2), cfg.quote)
                } else {
                    format!("{text}. No total — not every holding could be priced.")
                },
                json!({
                    "holdings": rows,
                    "total": total.is_finite().then(|| round_to(total, 2)),
                    "quote": cfg.quote,
                    "complete": complete,
                }),
            )
        }
        other => tool_error(format!("unknown tool `{other}`")),
    }
}

// ---------------------------------------------------------------------------
// main
// ---------------------------------------------------------------------------

fn main() {
    let rpc = Arc::new(Rpc::new());
    let reader = BufReader::new(std::io::stdin());

    // One configuration, shared. A second `initialize` (a reconnect, a
    // reload) must REPLACE what everyone reads, not hand a second copy to a
    // second poll thread: two pollers on different configs would publish to
    // the same overlays with, say, two different quote assets, and the
    // reader would see the totals alternate between them.
    let config = Arc::new(Mutex::new(Config::default()));
    let mut polling = false;

    // One worker for every tool call. Not the read loop, because a tool call
    // issues `neige.*` callbacks whose replies arrive on the very stdin this
    // loop is reading — handling one inline makes the plugin wait out its own
    // 15s timeout for a reply it is preventing itself from reading. Not a
    // thread each, because a tool call can take as long as a network round
    // trip and a caller sending faster than that would accumulate threads
    // without bound, and a `thread::spawn` that then failed would panic on
    // the reader and take the plugin down silently. The single worker below
    // is therefore also what serialises tool calls against each other: they
    // are handled one at a time because one thread drains `tool_queue`.
    let (tool_calls, tool_queue) = mpsc::channel::<Value>();
    // Recording a holding wakes the poll thread instead of pricing inline, so
    // the write tool never touches the network — see the note at the
    // `market.holdings.set` arm.
    // `wake_tx` stays alive in this scope for the process's lifetime: if every
    // sender dropped, the poller's `recv_timeout` would return `Disconnected`
    // immediately and spin. The receiver is taken by the single poll thread.
    let (wake_tx, wake_rx) = mpsc::channel::<()>();
    let mut wake_rx = Some(wake_rx);
    {
        let rpc = Arc::clone(&rpc);
        let config = Arc::clone(&config);
        let wake_tx = wake_tx.clone();
        std::thread::spawn(move || {
            for frame in tool_queue {
                let Some(id) = frame.get("id").cloned() else {
                    continue;
                };
                // Read the configuration per call, so a call that was queued
                // before a re-initialize still runs on the current one.
                let cfg = config.lock().map(|cfg| cfg.clone()).unwrap_or_default();
                let reply = tools_call_reply(&rpc, &cfg, &wake_tx, &frame);
                rpc.reply(id, reply);
            }
        });
    }

    for line in reader.lines() {
        let Ok(line) = line else { return };
        if line.trim().is_empty() {
            continue;
        }
        let frame: Value = match serde_json::from_str(&line) {
            Ok(frame) => frame,
            Err(e) => {
                eprintln!("market: bad json from kernel: {e}");
                continue;
            }
        };

        // A frame with no `method` is a reply to one of our callbacks.
        let Some(method) = frame.get("method").and_then(Value::as_str) else {
            match frame.get("id").and_then(Value::as_u64) {
                Some(id) if rpc.complete(id, frame) => {}
                Some(id) => eprintln!("market: reply to id {id} arrived with nobody waiting"),
                None => eprintln!("market: frame with neither method nor id, ignored"),
            }
            continue;
        };

        let Some(id) = frame.get("id").cloned() else {
            // A notification (e.g. `notifications/initialized`).
            continue;
        };

        match method {
            "initialize" => {
                rpc.reply(id, initialize_reply(&frame));
                let parsed = config_from_initialize(&frame);
                eprintln!(
                    "market: configured — quote={} poll={}s binance={}",
                    parsed.quote,
                    parsed.poll.as_secs(),
                    parsed.binance_endpoint,
                );
                if let Ok(mut cfg) = config.lock() {
                    *cfg = parsed;
                }
                if let Some(wake_rx) = wake_rx.take().filter(|_| !polling) {
                    polling = true;
                    let rpc = Arc::clone(&rpc);
                    let config = Arc::clone(&config);
                    std::thread::spawn(move || {
                        loop {
                            let cfg = config.lock().map(|cfg| cfg.clone()).unwrap_or_default();
                            refresh_all(&rpc, &cfg);
                            // Sleep, but wake early when a tool records a
                            // holding. Draining the backlog afterwards keeps a
                            // burst of edits to one pass instead of one pass
                            // each.
                            let _ = wake_rx.recv_timeout(cfg.poll);
                            while wake_rx.try_recv().is_ok() {}
                        }
                    });
                }
            }
            "tools/call" => {
                if tool_calls.send(frame).is_err() {
                    eprintln!("market: the tool worker is gone; refusing the call");
                    rpc.send(&json!({
                        "jsonrpc": "2.0",
                        "id": id,
                        "error": { "code": -32603, "message": "tool worker unavailable" },
                    }));
                }
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

    fn cfg() -> Config {
        Config::default()
    }

    /// The identity a bare name gets. Written through `parse_asset` on
    /// purpose: a hand-built `AssetId` in a test would stop proving that the
    /// parser agrees with it.
    fn id(raw: &str) -> AssetId {
        parse_asset(raw).unwrap_or_else(|| panic!("`{raw}` must parse"))
    }

    fn holding(raw: &str, quantity: f64) -> Holding {
        Holding {
            asset: id(raw),
            quantity,
        }
    }

    #[test]
    fn a_holding_must_be_a_named_asset_and_a_positive_finite_quantity() {
        assert_eq!(
            Holding::from_json(&json!({ "asset": " btc ", "quantity": 100.0 })),
            Some(holding("BTC", 100.0)),
            "names are trimmed and upper-cased so `btc` and `BTC` are one holding"
        );
        for bad in [
            json!({ "asset": "BTC" }),
            json!({ "quantity": 1.0 }),
            json!({ "asset": "", "quantity": 1.0 }),
            json!({ "asset": "BT C", "quantity": 1.0 }),
            json!({ "asset": "BTC", "quantity": 0.0 }),
            json!({ "asset": "BTC", "quantity": -1.0 }),
            json!({ "asset": "BTC", "quantity": "100" }),
        ] {
            assert_eq!(Holding::from_json(&bad), None, "{bad}");
        }
    }

    /// **I1 — the parser is a superset of what could already be stored.**
    ///
    /// Before venues existed, a stored asset was `[A-Z0-9]+` (trimmed and
    /// upper-cased at the entry point, then required to be entirely
    /// alphanumeric). Every such name must still parse, or read-side
    /// normalisation would DROP the row — `holdings_from_value` discards what
    /// does not parse and the next `set` overwrites the whole array, so a
    /// gap here is permanent data loss, not a read error.
    ///
    /// The coverage here is a SAMPLE plus a bounded sweep, not a proof over
    /// every legacy name: a literal list of shapes that have each broken a
    /// naive rule, every one- and two-character name over the alphabet, and
    /// every legal character in third position after `A`. That is enough to
    /// kill a rule keyed on "starts with a letter", "has no digits", or a
    /// length cap anywhere below 32 — it says nothing about rules those
    /// samples do not reach.
    #[test]
    fn i1_the_legacy_shapes_sampled_here_and_every_short_name_still_parse() {
        // Real names that have each broken a naive rule at some point:
        // leading digit, all digits, digits-then-letters, a bare quote asset.
        for name in [
            "BTC",
            "ETH",
            "USDT",
            "1INCH",
            "600519",
            "0700",
            "W",
            "X",
            "0",
            "9",
            "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
        ] {
            let parsed = parse_asset(name).unwrap_or_else(|| panic!("`{name}` must parse"));
            assert_eq!(parsed.venue, Venue::Crypto, "`{name}` is a bare name");
            assert_eq!(parsed.symbol, name, "`{name}` keeps its symbol verbatim");
        }
        // The sweep: every one- and two-character name over the legal
        // alphabet, and every legal character in third position after `A`.
        //
        // Asserted against a LITERAL expectation, never against `id(name)`:
        // `id` is `parse_asset(..).unwrap()`, so comparing the two would be
        // `parse_asset(x) == parse_asset(x)` and would hold for any parser at
        // all, including one that answered `US` for everything.
        let alphabet: Vec<char> = ('A'..='Z').chain('0'..='9').collect();
        let crypto = |symbol: &str| AssetId {
            venue: Venue::Crypto,
            symbol: symbol.to_string(),
        };
        for &a in &alphabet {
            let one = a.to_string();
            assert_eq!(parse_asset(&one), Some(crypto(&one)), "{one}");
            for &b in &alphabet {
                let two = format!("{a}{b}");
                assert_eq!(parse_asset(&two), Some(crypto(&two)), "{two}");
                let three = format!("A{a}{b}");
                assert_eq!(parse_asset(&three), Some(crypto(&three)), "{three}");
            }
        }
        // Lower case is accepted and folded, because the entry points folded
        // it before this function existed.
        assert_eq!(parse_asset(" btc "), Some(crypto("BTC")));
    }

    /// **I2 — pricing takes the same path it took before, WHILE
    /// `cfg.quote` is its default `USDT` and while Binance's quote leg is
    /// still `cfg.quote`.**
    ///
    /// This equivalence is scoped to THIS slice and to that configuration. It
    /// is NOT an unconditional property, and it must not be used as a gate on
    /// the slice that decouples the Binance quote leg from the settlement
    /// currency: once the leg is pinned to `USDT` independently, `quote=CNY`
    /// turns a `BTCCNY` lookup (which the venue answers with an empty body ⇒
    /// `Failed`) into a `BTCUSDT` one (⇒ `Price`), and the variants differ by
    /// design.
    ///
    /// What is compared is the decision the old code made, reconstructed from
    /// its two branches: `asset == cfg.quote` ⇒ `Price(1.0)`, otherwise a
    /// request for the spot symbol `<ASSET><QUOTE>`. The second half is
    /// checked by pointing the SHIPPING lookup at a recording endpoint and
    /// reading the request target off the wire, not by re-deriving
    /// `binance_symbol`'s formula next to `binance_symbol`.
    #[test]
    fn i2_todays_names_keep_their_pricing_path_while_quote_is_usdt() {
        let (endpoint, targets) = recording_endpoint("2.5");
        let cfg = Config {
            binance_endpoint: endpoint,
            ..cfg()
        };
        assert_eq!(
            cfg.quote, "USDT",
            "this equivalence is scoped to the default"
        );
        for name in ["BTC", "ETH", "USDT", "1INCH", "600519", "0700", "W"] {
            let parsed = id(name);
            // The old shortcut condition, spelled as the old code spelled it.
            let was_the_quote_asset = name == cfg.quote;
            let takes_the_shortcut =
                matches!(quote_asset_shortcut(&cfg, &parsed), Some(price) if price == 1.0);
            assert_eq!(
                takes_the_shortcut, was_the_quote_asset,
                "`{name}` must reach the same branch it used to"
            );
            // `quote_asset`, not `binance_symbol`: this is the function the
            // refresh and both tools call, and it is what decides whether the
            // name reaches a provider at all.
            let quoted = quote_asset(&cfg, &parsed);
            if was_the_quote_asset {
                assert!(
                    matches!(quoted, Quote::Price(price) if price == 1.0),
                    "`{name}` must still price itself without a request"
                );
                continue;
            }
            assert!(
                matches!(quoted, Quote::Price(price) if price == 2.5),
                "`{name}` must still be priced by the venue"
            );
            let target = targets
                .recv_timeout(Duration::from_secs(5))
                .unwrap_or_else(|e| panic!("`{name}` sent no request: {e}"));
            assert_eq!(
                target,
                format!("/api/v3/ticker/price?symbol={name}USDT"),
                "`{name}` must be looked up under the same spot symbol as before"
            );
        }
        assert!(
            targets.try_recv().is_err(),
            "the quote asset must not have reached the venue at all"
        );
    }

    /// A loopback endpoint that answers every request with one price and
    /// reports the request target it saw.
    ///
    /// The point is that the assertion is made about the URL the shipping
    /// code builds. Comparing `binance_symbol`'s output against
    /// `format!("{name}{quote}")` would only be `binance_symbol` agreeing
    /// with itself, and would stay green if `binance_spot` stopped using it.
    fn recording_endpoint(price: &'static str) -> (String, mpsc::Receiver<String>) {
        use std::io::Read;
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind loopback");
        let base = format!("http://{}", listener.local_addr().expect("addr"));
        let (tx, rx) = mpsc::channel();
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
                let request_line = String::from_utf8_lossy(&head)
                    .lines()
                    .next()
                    .unwrap_or_default()
                    .to_string();
                let target = request_line
                    .split_whitespace()
                    .nth(1)
                    .unwrap_or_default()
                    .to_string();
                if tx.send(target).is_err() {
                    return;
                }
                let body = format!("{{\"symbol\":\"X\",\"price\":\"{price}\"}}");
                let _ = write!(
                    stream,
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len(),
                );
                let _ = stream.flush();
            }
        });
        (base, rx)
    }

    /// A venue prefix is a prefix only when the colon is there. Without this
    /// the parser would have to guess, and every guessing rule proposed for
    /// this (digit counts, lengths, known-ticker lists) has counterexamples
    /// among real names.
    #[test]
    fn a_name_without_a_colon_is_never_read_as_a_prefix() {
        for name in ["USNVDA", "HK1810", "CRYPTOBTC", "CNW", "US", "HK", "CRYPTO"] {
            let parsed = id(name);
            assert_eq!(parsed.venue, Venue::Crypto, "`{name}`");
            assert_eq!(parsed.symbol, name);
            assert_eq!(parsed.canonical(), format!("CRYPTO:{name}"));
        }
    }

    #[test]
    fn a_qualified_name_names_a_venue_and_folds_to_one_canonical_form() {
        assert_eq!(id("us:nvda").canonical(), "US:NVDA");
        assert_eq!(id("HK:1810").canonical(), "HK:1810");
        assert_eq!(id("cn:600519").canonical(), "CN:600519");
        assert_eq!(id(" CRYPTO:btc ").canonical(), "CRYPTO:BTC");
        // A prefix that names no venue is refused rather than swallowed as
        // part of a bare name: pricing `SH:600519` as crypto `SH:600519`, or
        // as anything else, would be a number nobody asked for.
        for bad in [
            "SH:600519",
            "JP:7203",
            ":BTC",
            "BTC:",
            "US:",
            "US::NVDA",
            "US:NV DA",
            "US:1810.HK",
            "",
            "   ",
            "BT C",
            "BTC.US",
            "1810.HK",
        ] {
            assert_eq!(parse_asset(bad), None, "`{bad}` must not parse");
        }
    }

    /// A venue with no source answers `Unknown`, and is NOT handed to the
    /// crypto provider. Routing it there would price `US:BTC` at bitcoin's
    /// price — a number for an identity nobody quoted.
    #[test]
    fn a_venue_with_no_source_says_unknown_rather_than_borrowing_the_crypto_price() {
        // The endpoint is dead, so anything that DID reach the provider would
        // come back `Failed`, not `Unknown` — the two are distinguishable
        // here on purpose.
        let cfg = Config {
            binance_endpoint: "http://127.0.0.1:1".into(),
            ..cfg()
        };
        for name in ["US:BTC", "US:NVDA", "HK:1810", "CN:600519", "US:USDT"] {
            assert!(
                matches!(quote_asset(&cfg, &id(name)), Quote::Unknown),
                "`{name}` must not be looked up as a crypto symbol"
            );
        }
        assert!(
            matches!(quote_asset(&cfg, &id("BTC")), Quote::Failed(_)),
            "a crypto identity still goes to the provider"
        );
    }

    /// The three names that made venues necessary. `W` is Wayfair on a US
    /// exchange and Wormhole in crypto, and the bare form is frozen as the
    /// crypto one — so a bare `W` and a `CRYPTO:W` are ONE holding, while a
    /// `US:W` is a different one that must not be merged with either.
    #[test]
    fn bare_w_is_the_crypto_w_and_us_w_is_a_third_string_but_a_second_identity() {
        assert_eq!(id("W"), id("CRYPTO:W"), "the bare form is frozen as crypto");
        assert_eq!(id("W").canonical(), "CRYPTO:W");
        assert_ne!(id("US:W"), id("W"), "two venues, two assets");
        assert_eq!(id("US:W").canonical(), "US:W");
        // Two identities ⇒ two rows that a `set` on one does not touch. This
        // is the registered gap: a holding mis-recorded as bare `W` and then
        // re-recorded as `US:W` leaves both rows standing.
        let mut holdings = vec![holding("W", 10.0), holding("US:W", 5.0)];
        let target = id("US:W");
        holdings.retain(|h| h.asset != target);
        assert_eq!(
            holdings
                .iter()
                .map(|h| h.asset.canonical())
                .collect::<Vec<_>>(),
            vec!["CRYPTO:W"],
        );
    }

    /// Read-side normalisation, at the seam a `set` actually goes through:
    /// a legacy row spelled `BTC` and an incoming `crypto:BTC` must collapse
    /// to ONE holding. Without normalisation the retain compares
    /// `CRYPTO:BTC` against the un-normalised `BTC`, keeps both, and
    /// `price_holdings` sums them.
    #[test]
    fn a_legacy_row_and_a_qualified_write_are_one_holding_not_two() {
        let stored = json!([{ "asset": "BTC", "quantity": 100.0 }]);
        let mut holdings = holdings_from_value(Some(&stored), "trk");
        let incoming = id("crypto:BTC");
        holdings.retain(|h| h.asset != incoming);
        holdings.push(Holding {
            asset: incoming,
            quantity: 60.0,
        });
        assert_eq!(holdings.len(), 1, "one asset, one row: {holdings:?}");
        assert_eq!(
            holdings[0].quantity, 60.0,
            "the write replaces, it does not add"
        );
        assert_eq!(
            store_value(&holdings),
            json!([{ "asset": "CRYPTO:BTC", "quantity": 60.0 }]),
            "and the write-back is canonical — this is the gradual migration"
        );
    }

    /// The venue reaches the reader at all three exits. It is a column of its
    /// own rather than a prefix on the name, so a table holding `US:W` and
    /// `CRYPTO:W` shows two distinguishable rows.
    #[test]
    fn every_exit_echoes_the_venue() {
        let cfg = Config {
            binance_endpoint: "http://127.0.0.1:1".into(),
            ..cfg()
        };
        let (rows, total, complete) = price_holdings(
            &cfg,
            &[holding("USDT", 3.0), holding("US:W", 1.0)],
            &mut PriceCache::new(),
        );
        assert_eq!(rows[0]["asset"], json!("USDT"));
        assert_eq!(rows[0]["venue"], json!("CRYPTO"));
        assert_eq!(rows[1]["asset"], json!("W"));
        assert_eq!(rows[1]["venue"], json!("US"));

        // Exit 2: `market.holdings.list`. It hands these same rows out as its
        // structuredContent, so the venue reaches that half with them — and
        // its HUMAN-READABLE line, which has no columns, must name the venue
        // too. `1 × W` is Wayfair and Wormhole equally.
        let line = holdings_line(&cfg, &rows);
        assert!(line.contains("CRYPTO:USDT"), "{line}");
        assert!(line.contains("US:W"), "{line}");

        // Exit 3: `market.quote`, through the tool dispatcher rather than
        // through the formatting alone. The quote asset prices at 1.0 without
        // a network call and this arm makes no host callback, so the `Rpc`
        // below is never used.
        let (wake, _woken) = mpsc::channel();
        let quoted = tools_call_reply(
            &Rpc::new(),
            &cfg,
            &wake,
            &json!({ "params": { "name": "market.quote", "arguments": { "asset": "usdt" } } }),
        );
        let text = quoted["content"][0]["text"].as_str().expect("text");
        assert!(text.contains("CRYPTO:USDT"), "{text}");
        assert_eq!(quoted["structuredContent"]["venue"], json!("CRYPTO"));

        let table = holdings_table(&cfg, rows, Some(total), complete, "2026-09-06T12:00:00Z");
        let columns: Vec<&str> = table["columns"]
            .as_array()
            .expect("columns")
            .iter()
            .filter_map(|c| c["key"].as_str())
            .collect();
        assert!(columns.contains(&"venue"), "{columns:?}");
        assert!(
            columns.len() <= calm_types::report_blocks::kinds::MAX_TABLE_COLUMNS,
            "{columns:?}"
        );
        assert_eq!(validate_payload(KIND_TABLE, &table), Ok(()));
    }

    #[test]
    fn one_unreadable_row_costs_only_itself() {
        let stored = json!([
            { "asset": "BTC", "quantity": 100.0 },
            { "asset": "???", "quantity": 1.0 },
            { "asset": "ETH", "quantity": 2.5 },
        ]);
        let holdings = holdings_from_value(Some(&stored), "trk");
        assert_eq!(
            holdings
                .iter()
                .map(|h| h.asset.canonical())
                .collect::<Vec<_>>(),
            vec!["CRYPTO:BTC", "CRYPTO:ETH"],
            "a bad row must not make the whole portfolio unreadable"
        );
        assert!(holdings_from_value(None, "trk").is_empty());
        assert!(holdings_from_value(Some(&json!("nonsense")), "trk").is_empty());
    }

    // The kernel fills `_meta["dev.neige/track"]`; the caller fills
    // `arguments`. Reading the wrong one is how a plugin ends up writing to
    // whichever Track a poisoned instruction named.
    #[test]
    fn the_track_comes_from_the_kernels_meta_never_from_the_arguments() {
        let frame = json!({
            "params": {
                "arguments": { "track_id": "attacker-chosen" },
                "_meta": { "dev.neige/track": { "id": "trk_real" } }
            }
        });
        assert_eq!(track_from_call(&frame), Some("trk_real".into()));

        // No `_meta` ⇒ no Track, even though the arguments name one.
        let forged = json!({ "params": { "arguments": { "track_id": "attacker-chosen" } } });
        assert_eq!(track_from_call(&forged), None);
        // An empty id is not a Track either.
        let empty = json!({ "params": { "_meta": { "dev.neige/track": { "id": "" } } } });
        assert_eq!(track_from_call(&empty), None);
    }

    #[test]
    fn the_quote_asset_prices_itself_without_a_venue() {
        // Also the reason the pricing tests below need no network: a portfolio
        // of the quote asset exercises every path except the HTTP call.
        assert!(matches!(quote_asset(&cfg(), &id("USDT")), Quote::Price(p) if p == 1.0));
    }

    #[test]
    fn pushed_payloads_are_valid_report_table_blocks() {
        // The load-bearing one: whatever this plugin pushes is read back by a
        // report `table` block, so it must satisfy the KERNEL's validator —
        // not a second opinion written here, which could agree with the
        // plugin and disagree with the renderer.
        let cfg = cfg();
        let (rows, total, complete) =
            price_holdings(&cfg, &[holding("USDT", 3.0)], &mut PriceCache::new());
        assert!(complete && total == 3.0);
        let holdings = holdings_table(&cfg, rows, Some(total), complete, "2026-09-06T12:00:00Z");
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
    fn an_unpriceable_holding_keeps_its_row_and_leaves_the_total_alone() {
        // `ZZZZ` is not the quote asset, so it goes to the venue — which is
        // unreachable at this endpoint, so it fails. The priced half must
        // still be priced, and the total must cover only it.
        let cfg = Config {
            binance_endpoint: "http://127.0.0.1:1".into(),
            ..cfg()
        };
        let (rows, total, complete) = price_holdings(
            &cfg,
            &[holding("USDT", 3.0), holding("ZZZZ", 1.0)],
            &mut PriceCache::new(),
        );
        assert!(!complete);
        assert_eq!(total, 3.0, "the total covers the priced rows only");
        assert_eq!(rows.len(), 2, "the unpriceable holding keeps its row");
        assert!(rows[1]["price"].is_null() && rows[1]["value"].is_null());
    }

    #[test]
    fn an_unsummable_portfolio_still_publishes_its_rows() {
        // The rows are the only place the per-asset detail exists. Suppressing
        // the whole table would leave the previous overlay on screen, read as
        // current — the failure mode is silence, not a wrong number.
        let table = holdings_table(
            &cfg(),
            vec![json!({ "asset": "BTC", "qty": 1.0, "price": 2.0, "value": 2.0 })],
            None,
            true,
            "2026-09-06T12:00:00Z",
        );
        let rows = table["rows"].as_array().expect("rows");
        assert_eq!(rows.len(), 2, "the asset row survives");
        assert!(rows[1]["value"].is_null(), "and the total states nothing");
        assert!(
            table["caption"]
                .as_str()
                .expect("caption")
                .contains("do not sum to a finite number")
        );
        assert_eq!(validate_payload(KIND_TABLE, &table), Ok(()));
    }

    #[test]
    fn a_total_that_overflows_is_not_reported_as_a_number() {
        // Two holdings of the quote asset price without any network (1.0
        // each), and each row's value is finite while their sum is not. The
        // per-row check alone would pass this straight into the history.
        let (rows, total, complete) = price_holdings(
            &cfg(),
            &[holding("USDT", 1e308), holding("USDT", 1e308)],
            &mut PriceCache::new(),
        );
        assert!(complete, "both rows price fine on their own");
        assert_eq!(rows.len(), 2);
        assert!(!total.is_finite(), "the sum is what `refresh` must refuse");
        assert!(json!(round_to(total, 2)).is_null());
    }

    #[test]
    fn rounding_never_manufactures_an_infinity() {
        // `1e308 * 100` overflows. Every finiteness guard upstream passes on
        // this input, so if rounding produced `inf` here it would serialize as
        // `null` and read back as a fabricated zero — the exact defect those
        // guards exist to prevent, reintroduced by the display step.
        assert!(round_to(1e308, 2).is_finite());
        assert_eq!(round_to(1e308, 2), 1e308, "too large to scale ⇒ unrounded");
        assert_eq!(round_to(79_979.999_999, 2), 79_980.0);
        assert!(!round_to(f64::INFINITY, 2).is_finite());
    }

    #[test]
    fn history_rows_are_newest_first_with_the_change_against_the_previous_point() {
        let points = vec![
            json!({ "at": "t1", "total": 100.0 }),
            json!({ "at": "t2", "total": 110.0 }),
            json!({ "at": "t3", "total": 90.0 }),
        ];
        let table = history_table(&cfg(), &points);
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
    fn configuration_is_all_optional_and_the_poll_interval_is_floored() {
        let init = |values: Value| json!({ "params": { "_meta": { "dev.neige/config": { "values": values } } } });
        // An unconfigured install is a working install.
        let bare = config_from_initialize(&json!({}));
        assert_eq!(bare.quote, "USDT");
        assert_eq!(bare.poll, Duration::from_secs(30));
        assert_eq!(bare.binance_endpoint, "https://data-api.binance.vision");

        let clamped = config_from_initialize(&init(json!({ "poll_seconds": 1 })));
        assert_eq!(clamped.poll, Duration::from_secs(MIN_POLL_SECONDS));

        let custom = config_from_initialize(&init(json!({
            "quote": "usd", "binance_endpoint": "https://example.test/"
        })));
        assert_eq!(custom.quote, "USD");
        assert_eq!(
            custom.binance_endpoint, "https://example.test",
            "the trailing slash is dropped so URL building stays single-slash"
        );
    }

    #[test]
    fn timestamps_are_rfc3339_utc() {
        assert_eq!(civil_from_days(20_702), (2026, 9, 6));
        let now = now_rfc3339();
        assert_eq!(now.len(), 20, "{now}");
        assert!(now.ends_with('Z'), "{now}");
    }

    /// Every tool this plugin exposes must be callable by a Planner.
    ///
    /// Found by running it, not by reading it: the Planner produced a
    /// perfectly-formed `market.holdings.set` call and got back *"MCP tool
    /// call requires approval, but approval policy is never"*. Codex's
    /// `requires_mcp_tool_approval` short-circuits to "no approval needed"
    /// only for a read-only tool, or for one declaring BOTH
    /// `destructiveHint: false` and `openWorldHint: false` — and the kernel
    /// spawns every agent with `approval_policy: "never"`
    /// (`shared_codex_appserver.rs`), so a tool outside that set is not
    /// "gated", it is unusable.
    ///
    /// The fix is never to relabel a tool that does touch the open world.
    /// It is to make the write tool not touch it: `market.holdings.set`
    /// records state and wakes the poll thread, and the pricing happens
    /// there.
    #[test]
    fn every_exposed_tool_is_callable_under_approval_policy_never() {
        let manifest: Value =
            serde_json::from_str(include_str!("manifest.json")).expect("manifest parses");
        let tools = manifest["exposes_tools"].as_array().expect("exposes_tools");
        assert!(!tools.is_empty());
        for tool in tools {
            let name = tool["name"].as_str().unwrap_or_default();
            let annotations = &tool["annotations"];
            if annotations["readOnlyHint"] == json!(true) {
                continue;
            }
            assert_eq!(
                annotations["destructiveHint"],
                json!(false),
                "`{name}` is not read-only, so it must declare destructiveHint:false or no \
                 Planner can ever call it"
            );
            assert_eq!(
                annotations["openWorldHint"],
                json!(false),
                "`{name}` is not read-only, so it must not touch the open world — move the \
                 network call to the poll thread rather than relaxing this"
            );
        }
    }

    #[test]
    fn kv_keys_are_namespaced_per_track_and_per_kind() {
        // The two prefixes must not be prefixes OF EACH OTHER, or
        // `portfolios`'s prefix scan would pick up history documents and try
        // to price them.
        assert!(!HOLDINGS_PREFIX.starts_with(HISTORY_PREFIX));
        assert!(!HISTORY_PREFIX.starts_with(HOLDINGS_PREFIX));
        assert_eq!(holdings_key("t1"), "holdings/t1");
        assert_eq!(history_key("t1"), "history/t1");
    }
}
