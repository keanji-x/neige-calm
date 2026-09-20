//! Market portfolio plugin: prices the holdings recorded per Track and pushes `portfolio.holdings` and `portfolio.history` table overlays.
//! Binance defaults to `data-api.binance.vision` because `api.binance.com` geo-blocks many hosts; Sina needs a `Referer` and answers in GBK.
//! History is forward-only: one point per successful refresh, never back-filled.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, BufWriter, Read, Stdout, Write};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::time::Duration;

use serde_json::{Value, json};

/// `market.series` — historical bars for `chart.series` blocks.
mod series;

/// Callback ids start high enough not to be confused with kernel-originated request ids.
const FIRST_CALLBACK_ID: u64 = 1_000;
/// The kernel answers from memory or SQLite; anything slower is a wedged host, better logged than blocking the poller.
const CALLBACK_TIMEOUT: Duration = Duration::from_secs(15);
/// 500 × ~60 bytes stays inside the 64 KiB KV quota the manifest asks for.
const MAX_HISTORY_POINTS: usize = 500;
/// The lowest poll interval honoured: the endpoint is public, and hammering it rate-limits a shared IP.
const MIN_POLL_SECONDS: u64 = 5;
/// KV key prefix for each Track's holdings (`holdings/<track_id>`); also how the poll loop discovers which Tracks to price.
const HOLDINGS_PREFIX: &str = "holdings/";
/// KV key prefix for each Track's value history (`history/<track_id>`).
const HISTORY_PREFIX: &str = "history/";

/// The plugin's half of the stdio channel: `stdout` is behind a mutex and every outbound call parks a one-shot sender the read loop completes.
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
        // Answered, errored or timed out, this id is spent; leaving it would leak one entry per timeout.
        if let Ok(mut pending) = self.pending.lock() {
            pending.remove(&id);
        }
        outcome
    }

    /// Route a reply frame to its waiter; false when no one is (a late reply after a timeout).
    fn complete(&self, id: u64, frame: Value) -> bool {
        let sender = self.pending.lock().ok().and_then(|mut p| p.remove(&id));
        match sender {
            Some(tx) => tx.send(frame).is_ok(),
            None => false,
        }
    }
}

/// Where an asset trades. A name alone does not identify a security (`W` is Wayfair on the NYSE and Wormhole on a crypto exchange), and no venue is ever inferred from a name's shape.
/// Mainland China is two venues: a single `CN` would have to ask both exchanges, and the day one is halted the wrong security is accepted in silence.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum Venue {
    Crypto,
    Us,
    Hk,
    Sh,
    Sz,
    /// A migration path, not a place: stored rows may still hold `CN:600519`. It parses but is refused at pricing time, because a row that stopped parsing would be silently dropped on the next `market.holdings.set`.
    Cn,
}

impl Venue {
    fn prefix(self) -> &'static str {
        match self {
            Venue::Crypto => "CRYPTO",
            Venue::Us => "US",
            Venue::Hk => "HK",
            Venue::Sh => "SH",
            Venue::Sz => "SZ",
            Venue::Cn => "CN",
        }
    }

    fn from_prefix(prefix: &str) -> Option<Self> {
        match prefix {
            "CRYPTO" => Some(Venue::Crypto),
            "US" => Some(Venue::Us),
            "HK" => Some(Venue::Hk),
            "SH" => Some(Venue::Sh),
            "SZ" => Some(Venue::Sz),
            "CN" => Some(Venue::Cn),
            _ => None,
        }
    }
}

/// A venue plus the symbol that venue itself uses. The canonical spelling `<VENUE>:<SYMBOL>` is what gets stored, compared and cached.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct AssetId {
    venue: Venue,
    /// The symbol as the venue spells it — never prefixed; provider URLs are built from this.
    symbol: String,
}

impl AssetId {
    fn canonical(&self) -> String {
        format!("{}:{}", self.venue.prefix(), self.symbol)
    }
}

/// One string shared by both tools, so they cannot describe two different grammars.
const ASSET_SYNTAX_ERROR: &str = "`asset` must be a name like \"BTC\", or a \
    venue-qualified \"<VENUE>:<SYMBOL>\" over the venues CRYPTO, US, HK, SH \
    (Shanghai) and SZ (Shenzhen) — for example \"CRYPTO:BTC\", \"US:NVDA\", \
    \"HK:1810\", \"SH:600519\", \"SZ:000001\". A name with no venue is a \
    crypto asset. Each of those five venues has its own price source, quoting \
    in its own currency; portfolio values are converted separately. \"CN:\" also \
    parses, but only so that a holding stored under the retired mainland \
    venue can still be read back — it is never priced, and has to be recorded \
    again under the exchange that lists the code: \"SH:<code>\" for a Shanghai \
    listing, \"SZ:<code>\" for a Shenzhen one.";

/// The one place an asset name becomes an identity. Upper-cased and trimmed; `<VENUE>:<SYMBOL>` with a known prefix, else a bare name is crypto (frozen: pre-venue rows are bare names).
/// An unknown prefix is rejected outright; `CN:` parses but is refused at pricing time; the symbol is then folded by [`canonical_symbol`].
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
        symbol: canonical_symbol(venue, symbol),
    })
}

/// Fold the spellings of one security at a venue onto one symbol. Hong Kong codes are zero-padded to five digits (the spelling HKEX and the source use); leading zeros are stripped first so `HK:089988` folds too.
/// Anything longer than five digits after stripping is no Hong Kong code and is returned unchanged for [`sina_target`] to refuse.
fn canonical_symbol(venue: Venue, symbol: &str) -> String {
    match venue {
        Venue::Hk if symbol.chars().all(|c| c.is_ascii_digit()) => {
            let significant = symbol.trim_start_matches('0');
            // All zeros must still fold to one string rather than the empty one.
            let significant = if significant.is_empty() {
                "0"
            } else {
                significant
            };
            if significant.len() <= 5 {
                format!("{significant:0>5}")
            } else {
                significant.to_string()
            }
        }
        _ => symbol.to_string(),
    }
}

#[derive(Clone, Debug, PartialEq)]
struct Holding {
    /// Stored parsed so that no consumer re-splits it.
    asset: AssetId,
    quantity: f64,
}

impl Holding {
    /// Parse one stored row, normalising its asset to the canonical identity. This is a write-triggered migration: `neige.kv.set` is a whole-array overwrite with no CAS, so a batch scan would race a concurrent `set`. The KV stays a mixed space for Tracks never written again.
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
    /// The settlement currency a portfolio's total is stated in. Not a pricing input: each source quotes in its own currency and that travels with the price.
    quote: String,
    poll: Duration,
    binance_endpoint: String,
    sina_endpoint: String,
    /// Tencent's daily K-line source (`market.series`, US/HK/SH/SZ).
    tencent_endpoint: String,
    /// Test seam: when set, the plugin's wall clock is frozen at this instant for `market.series`.
    debug_clock_ms: Option<i64>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            quote: "USDT".into(),
            poll: Duration::from_secs(30),
            binance_endpoint: "https://data-api.binance.vision".into(),
            sina_endpoint: "https://hq.sinajs.cn".into(),
            tencent_endpoint: "https://web.ifzq.gtimg.cn".into(),
            debug_clock_ms: None,
        }
    }
}

/// Read the effective configuration out of the handshake's `_meta["dev.neige/config"]` envelope. Every key is optional and every default matches the manifest's.
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
    if let Some(endpoint) = values.get("sina_endpoint").and_then(Value::as_str)
        && !endpoint.trim().is_empty()
    {
        cfg.sina_endpoint = endpoint.trim().trim_end_matches('/').to_string();
    }
    if let Some(endpoint) = values.get("tencent_endpoint").and_then(Value::as_str)
        && !endpoint.trim().is_empty()
    {
        cfg.tencent_endpoint = endpoint.trim().trim_end_matches('/').to_string();
    }
    if let Some(frozen) = values.get("debug_clock_ms").and_then(Value::as_i64) {
        eprintln!(
            "market: WARNING debug_clock_ms={frozen} is set — the wall clock is frozen; \
             this is a test seam and must never be configured on a real install"
        );
        cfg.debug_clock_ms = Some(frozen);
    }
    cfg
}

/// The Track a `tools/call` was made from, read from `params._meta["dev.neige/track"].id` and nowhere else: the arguments are written by the agent, which could name someone else's Track.
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

/// A stored entry that no longer parses is dropped with a log line: one bad row must not make a Track's whole portfolio unreadable.
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

fn load_holdings(rpc: &Rpc, track_id: &str) -> Result<Vec<Holding>, String> {
    let result = rpc.call("neige.kv.get", json!({ "key": holdings_key(track_id) }))?;
    Ok(holdings_from_value(result.get("value"), track_id))
}

/// The exact JSON `store_holdings` writes, split out so it can be asserted without a fake kernel.
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

/// Every Track this plugin holds a portfolio for. Only the names are taken; [`refresh`] re-reads each document under the lock.
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

// Providers. One identity has one venue and one source: `CRYPTO` → Binance spot (`<SYMBOL>USDT`); `US`/`HK`/`SH`/`SZ` → Sina (`gb_`, `hk`, `sh`, `sz`). An identity is never handed to the other venue's source.
// Neither source states a currency, so a stock price's currency is decided here from the venue and code range, and refused where it cannot be.

/// A currency this plugin can attach to a number. An enum so that "no rate for this pair" and "not a currency this plugin knows" are not the same runtime miss.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum Currency {
    /// Binance's pinned quote leg. Its own currency here, but it does not settle: a portfolio in USDT is stated in USD at [`USDT_USD_ASSUMED_PARITY`].
    Usdt,
    Usd,
    Hkd,
    Cny,
}

impl Currency {
    fn code(self) -> &'static str {
        match self {
            Self::Usdt => "USDT",
            Self::Usd => "USD",
            Self::Hkd => "HKD",
            Self::Cny => "CNY",
        }
    }

    /// The currency a configured `quote` settles in, or `None`. Only `USD` and `CNY` settle; `USDT` settles as `USD` because it is the default every existing install has written down.
    fn settlement(quote: &str) -> Option<Self> {
        match quote.trim().to_ascii_uppercase().as_str() {
            "USD" | "USDT" => Some(Self::Usd),
            "CNY" => Some(Self::Cny),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
enum Quote {
    /// A positive, finite price and the currency the number is in — never the configured settlement currency. The currency is this plugin's own determination, since neither source states one.
    Price(f64, Currency),
    /// No valid quote came back and nothing went wrong: an unlisted name, an unspellable one, or the row of zeros a halted security answers.
    Unknown,
    Failed(String),
}

/// Price one asset: route the identity to the one source that serves its venue.
fn quote_asset(cfg: &Config, asset: &AssetId) -> Quote {
    match asset.venue {
        Venue::Crypto => binance_spot(cfg, asset),
        Venue::Us | Venue::Hk | Venue::Sh | Venue::Sz => sina_quote(cfg, asset),
        // `CN` names no exchange; the answer is a visible `Failed` carrying the two spellings that would work.
        Venue::Cn => Quote::Failed(format!(
            "{} names no exchange: `CN` was the mainland venue before this plugin \
             split it into `SH` (Shanghai) and `SZ` (Shenzhen), and the code itself \
             does not say which exchange lists it. Record this holding again under \
             the exchange that does — `SH:{symbol}` if it is listed in Shanghai, \
             `SZ:{symbol}` if it is listed in Shenzhen. The two are not \
             interchangeable: whichever of them does not list this code is refused \
             as well.",
            asset.canonical(),
            symbol = asset.symbol,
        )),
    }
}

/// The one test for "is this string a price", called by both sources. `0` and negative parse fine, and Sina's row of `0.0000` means no live price.
fn positive_price(raw: &str) -> Option<f64> {
    match raw.trim().parse::<f64>() {
        Ok(price) if price.is_finite() && price > 0.0 => Some(price),
        _ => None,
    }
}

/// One asset is priced once per pass, however many Tracks hold it; the cache lives for exactly one pass.
type PriceCache = HashMap<AssetId, Quote>;

fn quote_cached(cfg: &Config, asset: &AssetId, cache: &mut PriceCache) -> Quote {
    if let Some(hit) = cache.get(asset) {
        return hit.clone();
    }
    let quote = quote_asset(cfg, asset);
    cache.insert(asset.clone(), quote.clone());
    quote
}

/// The quote leg every Binance spot lookup is built with. Pinned, not taken from `cfg.quote`: an install settling in `CNY` would ask for `BTCCNY`, which does not exist (`-1121 Invalid symbol`).
const BINANCE_QUOTE_LEG: &str = "USDT";

/// The currency [`BINANCE_QUOTE_LEG`] denominates a price in, written next to the symbol so the two cannot disagree.
const BINANCE_QUOTE_CURRENCY: Currency = Currency::Usdt;

/// The spot symbol Binance is asked about, built from the venue-local symbol: `CRYPTO:BTCUSDT` is not one.
fn binance_symbol(asset: &AssetId) -> String {
    format!("{}{BINANCE_QUOTE_LEG}", asset.symbol)
}

/// Binance spot via `/api/v3/ticker/price`. It answers `200` with a `{"code":…}` body for an unknown symbol and for a geo-blocked caller, so the absence of `price` is the real check.
fn binance_spot(cfg: &Config, asset: &AssetId) -> Quote {
    // The quote leg priced in itself: Binance's fact about its own leg, needing no request. It answers `USDT` as the currency.
    if asset.symbol == BINANCE_QUOTE_LEG {
        return Quote::Price(1.0, BINANCE_QUOTE_CURRENCY);
    }
    let symbol = binance_symbol(asset);
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
        // `-1121 Invalid symbol` is Unknown. Any other `code` is a failure whose message is NOT quoted back: the body is attacker-influenced text that would ride into an overlay.
        return match parsed.get("code").and_then(Value::as_i64) {
            Some(-1121) => Quote::Unknown,
            Some(code) => {
                Quote::Failed(format!("{symbol}: venue refused the lookup (code {code})"))
            }
            None => Quote::Failed(format!("{symbol}: response carried no price")),
        };
    };
    match positive_price(price) {
        Some(price) => Quote::Price(price, BINANCE_QUOTE_CURRENCY),
        None => Quote::Failed(format!(
            "{symbol}: `{price}` is not a positive finite price"
        )),
    }
}

/// `hq.sinajs.cn` answers `HTTP 403 Forbidden` without this header.
const SINA_REFERER: &str = "https://finance.sina.com.cn";

/// One row is a few hundred bytes and exactly one is asked for; anything past this is a wedged or hostile endpoint.
const SINA_MAX_BODY_BYTES: u64 = 64 * 1024;

/// Which field of a Sina row is the last traded price, per market — the field orders differ. Read off live responses: `gb_` field 1, `hk` field 6 (cross-checked against the change column: `-0.960` = `27.480 - 28.440`), `sh`/`sz` field 3.
const SINA_LAST_PRICE_FIELD_US: usize = 1;
const SINA_LAST_PRICE_FIELD_HK: usize = 6;
const SINA_LAST_PRICE_FIELD_SH_SZ: usize = 3;

/// How one identity is asked for, and what currency the answer is in.
struct SinaLookup {
    symbol: String,
    price_field: usize,
    /// Not read off the wire: the source states no unit, so this is the plugin's determination from venue and code range.
    currency: Currency,
}

enum SinaTarget {
    Ask(SinaLookup),
    /// No way to spell the identity for this source (`HK:TENCENT`); no request goes out and the answer is [`Quote::Unknown`].
    Unspellable,
    /// Spellable, but the code range does not fix the currency; refused rather than priced under a guess.
    UndeterminedCurrency(String),
}

/// Build the request for one identity and decide its currency. Venue alone is not enough: `sh900932` (a B share) quotes in USD, `sz200725` in HKD, `hk89988` (a renminbi counter) in CNY.
/// So the ranges below are an allowlist; a code outside a range that fixes the currency is refused visibly.
fn sina_target(asset: &AssetId) -> SinaTarget {
    let undetermined = |asset: &AssetId, why: &str| {
        SinaTarget::UndeterminedCurrency(format!(
            "this plugin cannot determine what currency {} is quoted in ({why}), and \
             will not publish a price under a guessed one",
            asset.canonical(),
        ))
    };
    let six_digits = |symbol: &str| symbol.len() == 6 && symbol.chars().all(|c| c.is_ascii_digit());
    match asset.venue {
        // Spelled out rather than a catch-all so that adding a venue is a compile error here.
        Venue::Crypto => SinaTarget::Unspellable,
        Venue::Cn => SinaTarget::Unspellable,
        // Sina's `gb_` list is US-listed securities, quoted in US dollars.
        Venue::Us => SinaTarget::Ask(SinaLookup {
            symbol: format!("gb_{}", asset.symbol.to_ascii_lowercase()),
            price_field: SINA_LAST_PRICE_FIELD_US,
            currency: Currency::Usd,
        }),
        // A Hong Kong code arrives already five digits from [`canonical_symbol`]; only "is it a five-digit code" is asked here.
        // `8xxxx` is refused because `hk89988` is a renminbi counter (94.45 CNY beside `hk09988` at 111.00 HKD) and the row cannot say which; `9xxxx` is refused because no currency has been established for it either way.
        Venue::Hk => {
            if asset.symbol.len() != 5 || !asset.symbol.chars().all(|c| c.is_ascii_digit()) {
                return SinaTarget::Unspellable;
            }
            let code = &asset.symbol;
            if code.starts_with('8') {
                return undetermined(
                    asset,
                    "Hong Kong codes from 80000 to 89999 include renminbi counters — \
                     `hk89988` is one, quoted in CNY — and the row does not say which \
                     currency it is in",
                );
            }
            if code.starts_with('9') {
                return undetermined(
                    asset,
                    "Hong Kong codes from 90000 up are outside every range this plugin \
                     has established a quote currency for",
                );
            }
            SinaTarget::Ask(SinaLookup {
                symbol: format!("hk{code}"),
                price_field: SINA_LAST_PRICE_FIELD_HK,
                currency: Currency::Hkd,
            })
        }
        // Shanghai: `6xxxxx` (A-share main board and STAR) and `5xxxxx` (funds) are renminbi — funds by 《上海证券交易所交易规则》3.3.11, which denominates a fund order's tick in renminbi whatever it holds. `9xxxxx` is the B-share board, quoted in USD, and stays refused.
        Venue::Sh => {
            if !six_digits(&asset.symbol) {
                return SinaTarget::Unspellable;
            }
            if !(asset.symbol.starts_with('6') || asset.symbol.starts_with('5')) {
                return undetermined(
                    asset,
                    "on Shanghai this plugin prices the 6xxxxx A-share and STAR codes \
                     and the 5xxxxx fund codes, which are renminbi; 9xxxxx is the \
                     B-share board, which quotes in US dollars",
                );
            }
            SinaTarget::Ask(SinaLookup {
                symbol: format!("sh{}", asset.symbol),
                price_field: SINA_LAST_PRICE_FIELD_SH_SZ,
                currency: Currency::Cny,
            })
        }
        // Shenzhen: `00xxxx` main board, `30xxxx` ChiNext and the `15xxxx`/`16xxxx` fund ranges are renminbi (《深圳证券交易所交易规则》3.3.11 for funds). `2xxxxx` is the B-share board, quoted in HKD, and stays refused. In range is not a promise the source lists the code.
        Venue::Sz => {
            if !six_digits(&asset.symbol) {
                return SinaTarget::Unspellable;
            }
            let sz_renminbi = ["00", "30", "15", "16"]
                .iter()
                .any(|prefix| asset.symbol.starts_with(prefix));
            if !sz_renminbi {
                return undetermined(
                    asset,
                    "on Shenzhen this plugin prices the 00xxxx main-board, 30xxxx \
                     ChiNext and 15xxxx/16xxxx fund codes, which are renminbi; 2xxxxx \
                     is the B-share board, which quotes in Hong Kong dollars",
                );
            }
            SinaTarget::Ask(SinaLookup {
                symbol: format!("sz{}", asset.symbol),
                price_field: SINA_LAST_PRICE_FIELD_SH_SZ,
                currency: Currency::Cny,
            })
        }
    }
}

/// The payload of one `var hq_str_<symbol>="…";` row, or `None` when the response carried no row for it. An empty payload is `Some("")`: Sina answers an unlisted symbol with `var hq_str_gb_doge="";`.
fn sina_payload<'a>(body: &'a str, symbol: &str) -> Option<&'a str> {
    // The `=` and the opening quote are part of the needle, so `hk00001` cannot match inside a longer symbol's row.
    let head = format!("var hq_str_{symbol}=\"");
    let rest = &body[body.find(&head)? + head.len()..];
    Some(&rest[..rest.find('"')?])
}

/// One row of Sina's quote list. `Ok(None)` is an empty row (the symbol is not listed); a response missing the row altogether is an `Err`.
/// The response is GBK and is read with `from_utf8_lossy`: every byte examined is ASCII, nothing indexes by offset, and `"`/`,` are outside GBK's trailing-byte range so no name can smuggle a delimiter.
fn sina_row(cfg: &Config, symbol: &str) -> Result<Option<String>, String> {
    let url = format!("{}/list={symbol}", cfg.sina_endpoint);
    let response = match ureq::get(&url)
        .set("Referer", SINA_REFERER)
        .timeout(Duration::from_secs(10))
        .call()
    {
        Ok(response) => response,
        // Unlike Binance, a non-200 here carries no price shape (the 403 body is the literal `Forbidden`), so the body is not parsed.
        Err(ureq::Error::Status(code, _)) => return Err(format!("GET {url}: HTTP {code}")),
        Err(e) => return Err(format!("GET {url}: {e}")),
    };
    let mut bytes = Vec::new();
    response
        .into_reader()
        .take(SINA_MAX_BODY_BYTES)
        .read_to_end(&mut bytes)
        .map_err(|e| format!("reading {url}: {e}"))?;
    let body = String::from_utf8_lossy(&bytes);
    let Some(payload) = sina_payload(&body, symbol) else {
        return Err(format!("{url}: the response carried no `{symbol}` row"));
    };
    // "We do not list this." Not an error, and not a price.
    if payload.is_empty() {
        return Ok(None);
    }
    Ok(Some(payload.to_string()))
}

fn sina_quote(cfg: &Config, asset: &AssetId) -> Quote {
    let lookup = match sina_target(asset) {
        SinaTarget::Ask(lookup) => lookup,
        SinaTarget::Unspellable => return Quote::Unknown,
        SinaTarget::UndeterminedCurrency(why) => return Quote::Failed(why),
    };
    let symbol = &lookup.symbol;
    let payload = match sina_row(cfg, symbol) {
        Ok(Some(payload)) => payload,
        Ok(None) => return Quote::Unknown,
        Err(why) => return Quote::Failed(why),
    };
    let fields: Vec<&str> = payload.split(',').collect();
    let Some(raw) = fields.get(lookup.price_field) else {
        return Quote::Failed(format!(
            "{symbol}: the row has {} fields, so it has no field {}",
            fields.len(),
            lookup.price_field,
        ));
    };
    // A row of zeros is how this source spells a halted or unlisted symbol, so a non-price is `Unknown`, not a failure.
    match positive_price(raw) {
        Some(price) => Quote::Price(price, lookup.currency),
        None => Quote::Unknown,
    }
}

/// Which field of a Sina `fx_s…` row is the current rate. Field 8: the row's own change column (field 11) equals `field 8 − field 3` on every pair asked for. Field 1 is the bid on computed pairs, not the rate.
const SINA_FX_RATE_FIELD: usize = 8;

/// `USDT` is settled as `USD` one for one. This is an assumption, not a quote — the only number here no source stated. Measured on 2026-09-07 (`USDTUSD` at 0.99967) it overstates USDT-quoted value by ~3.3 bp; direction is not fixed.
/// Every exit that states a converted total names this hop as assumed; the per-row `rate` cell does not, so a machine consumer must read `conversions`.
const USDT_USD_ASSUMED_PARITY: f64 = 1.0;

/// One hop of a conversion: a quoted rate or the assumed parity, never printed alike.
#[derive(Clone, Debug, PartialEq)]
enum FxHop {
    Quoted {
        label: &'static str,
        factor: f64,
    },
    /// [`USDT_USD_ASSUMED_PARITY`]: no source was asked, and none could fail.
    AssumedParity,
}

impl FxHop {
    fn factor(&self) -> f64 {
        match self {
            Self::Quoted { factor, .. } => *factor,
            Self::AssumedParity => USDT_USD_ASSUMED_PARITY,
        }
    }

    fn describe(&self) -> String {
        match self {
            Self::Quoted { label, factor } => format!("{label} {}", round_to(*factor, 8)),
            Self::AssumedParity => {
                format!("USDT taken as {USDT_USD_ASSUMED_PARITY} USD — assumed, not quoted")
            }
        }
    }
}

/// A rate this plugin knows how to fetch; also the [`FxCache`] key.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
struct FxLeg {
    from: Currency,
    to: Currency,
}

impl FxLeg {
    /// Sina quotes every ordered pair over `USD`, `HKD` and `CNY` natively, so the symbol asked for is always the direction wanted; nothing divides into the opposite one.
    fn symbol(self) -> String {
        format!(
            "fx_s{}{}",
            self.from.code().to_ascii_lowercase(),
            self.to.code().to_ascii_lowercase()
        )
    }

    /// What the caption calls this hop. Only the four legs a configurable settlement can reach are named; the fallback names no symbol rather than a wrong one.
    fn label(self) -> &'static str {
        match (self.from, self.to) {
            (Currency::Usd, Currency::Cny) => "fx_susdcny@sina",
            (Currency::Cny, Currency::Usd) => "fx_scnyusd@sina",
            (Currency::Hkd, Currency::Usd) => "fx_shkdusd@sina",
            (Currency::Hkd, Currency::Cny) => "fx_shkdcny@sina",
            _ => "an unrouted pair",
        }
    }
}

/// One pass's exchange rates, keyed by pair; same lifetime and reason as [`PriceCache`].
type FxCache = HashMap<FxLeg, Result<f64, String>>;

/// Everything one pass fetched, so many Tracks ask each source once per distinct thing.
struct PassCache {
    prices: PriceCache,
    rates: FxCache,
}

impl PassCache {
    fn new() -> Self {
        Self {
            prices: PriceCache::new(),
            rates: FxCache::new(),
        }
    }
}

/// Fetch one pair, once per pass. A failure is cached alongside a success so two rows of one portfolio are not converted under different conditions; there is no stale-rate fallback anywhere in this plugin.
fn fx_leg(cfg: &Config, leg: FxLeg, cache: &mut FxCache) -> Result<f64, String> {
    if let Some(hit) = cache.get(&leg) {
        return hit.clone();
    }
    let fetched = fetch_fx_leg(cfg, leg);
    cache.insert(leg, fetched.clone());
    fetched
}

fn fetch_fx_leg(cfg: &Config, leg: FxLeg) -> Result<f64, String> {
    let symbol = leg.symbol();
    let Some(payload) = sina_row(cfg, &symbol)? else {
        return Err(format!(
            "the rate source does not list `{symbol}`, so there is no {}→{} rate \
             this pass",
            leg.from.code(),
            leg.to.code(),
        ));
    };
    let fields: Vec<&str> = payload.split(',').collect();
    let Some(raw) = fields.get(SINA_FX_RATE_FIELD) else {
        return Err(format!(
            "{symbol}: the row has {} fields, so it has no field {SINA_FX_RATE_FIELD}",
            fields.len(),
        ));
    };
    // Same test as a price: a row of zeros means nothing live, and a rate of zero would value a portfolio at nothing.
    positive_price(raw)
        .ok_or_else(|| format!("{symbol}: `{}` is not a positive finite rate", raw.trim()))
}

/// One step of a route before it has a number; kept apart from [`FxHop`] so the route table has no network in it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum FxStep {
    Parity,
    Fetch(FxLeg),
}

/// The steps that carry `from` to `to`, or `None`. At most one fetched rate in any route; the `USDT` step is the assumed parity. `from == to` never reaches here.
fn fx_route(from: Currency, to: Currency) -> Option<Vec<FxStep>> {
    match (from, to) {
        (Currency::Usdt, Currency::Usd) => Some(vec![FxStep::Parity]),
        (Currency::Usdt, Currency::Cny) => Some(vec![
            FxStep::Parity,
            FxStep::Fetch(FxLeg {
                from: Currency::Usd,
                to: Currency::Cny,
            }),
        ]),
        (from, to) if from != Currency::Usdt && to != Currency::Usdt => {
            Some(vec![FxStep::Fetch(FxLeg { from, to })])
        }
        _ => None,
    }
}

#[derive(Clone, Debug, PartialEq)]
struct FxPath {
    from: Currency,
    to: Currency,
    hops: Vec<FxHop>,
    factor: f64,
}

impl FxPath {
    /// One line a reader can check: which currencies, at what rate, and which step is assumed.
    fn describe(&self) -> String {
        let hops = self
            .hops
            .iter()
            .map(FxHop::describe)
            .collect::<Vec<_>>()
            .join(" × ");
        format!(
            "{}→{} {} ({hops})",
            self.from.code(),
            self.to.code(),
            round_to(self.factor, 8),
        )
    }
}

/// The multiplier from `from` into `to`, this pass. `Err` leaves a holding's converted value `null`; there is deliberately no fallback rate from an earlier pass.
fn fx_path(
    cfg: &Config,
    from: Currency,
    to: Currency,
    cache: &mut FxCache,
) -> Result<FxPath, String> {
    // Identity: no source is asked, so a portfolio already in the settlement currency prices with the rate endpoint unreachable.
    if from == to {
        return Ok(FxPath {
            from,
            to,
            hops: Vec::new(),
            factor: 1.0,
        });
    }
    let Some(steps) = fx_route(from, to) else {
        return Err(format!(
            "this plugin has no route from {} to {}",
            from.code(),
            to.code(),
        ));
    };
    let mut hops = Vec::with_capacity(steps.len());
    let mut factor = 1.0;
    for step in steps {
        let hop = match step {
            FxStep::Parity => FxHop::AssumedParity,
            FxStep::Fetch(leg) => FxHop::Quoted {
                label: leg.label(),
                factor: fx_leg(cfg, leg, cache)?,
            },
        };
        factor *= hop.factor();
        hops.push(hop);
    }
    if !factor.is_finite() || factor <= 0.0 {
        return Err(format!(
            "the {}→{} hops multiply to {factor}, which is not a rate",
            from.code(),
            to.code(),
        ));
    }
    Ok(FxPath {
        from,
        to,
        hops,
        factor,
    })
}

/// What a set of converted rows sums to, or why it does not. `Unsettleable` rows stand in several currencies and 100 USD plus 100 HKD is not 200 of anything. A row whose rate did not come back is left out and marks the pass incomplete.
#[derive(Clone, Debug, PartialEq)]
enum PortfolioTotal {
    Priced {
        amount: f64,
        currency: String,
    },
    /// `configured` is not a currency this plugin settles in, so the rows stand in more than one currency.
    Unsettleable {
        configured: String,
        currencies: Vec<String>,
    },
    NotFinite,
    /// The portfolio is empty: its total really is zero, the value of holding nothing.
    Empty,
    /// Not empty, and not one row could be priced and converted. Its total is unknown; `0` would say "worth nothing" where the truth is "could not value it".
    NonePriced,
}

impl PortfolioTotal {
    /// Whether there is a portfolio value to announce and plot, and in what unit. [`Self::Empty`] still answers `None`: there is no unit to pair its zero with, and a series of zeros for a Track holding nothing is a series about nothing.
    fn stated(&self) -> Option<(f64, &str)> {
        match self {
            Self::Priced { amount, currency } => Some((*amount, currency.as_str())),
            _ => None,
        }
    }

    /// Why there is no total, for a caller that got `null`.
    fn no_total_reason(&self) -> Option<String> {
        match self {
            Self::Priced { .. } => None,
            Self::Unsettleable {
                configured,
                currencies,
            } => Some(format!(
                "these holdings are quoted in {}, and `{configured}` — the configured \
                 settlement currency — is not one this plugin settles in (`USD` and \
                 `CNY` are)",
                currencies.join(" and "),
            )),
            Self::NotFinite => Some("the values do not sum to a finite number".into()),
            Self::Empty => Some("this Track holds nothing".into()),
            Self::NonePriced => {
                Some("not one holding could be priced and converted this pass".into())
            }
        }
    }

    /// The `value` cell of the `Total` row. [`Self::Empty`] is the only non-`Priced` variant with a number; `0` is not a spelling of unknown.
    fn value_cell(&self) -> Value {
        match self {
            Self::Priced { amount, .. } => json!(round_to(*amount, 2)),
            Self::Empty => json!(0.0),
            Self::Unsettleable { .. } | Self::NotFinite | Self::NonePriced => Value::Null,
        }
    }

    fn currency_cell(&self) -> Value {
        match self {
            Self::Priced { currency, .. } => json!(currency),
            _ => Value::Null,
        }
    }
}

/// One portfolio, priced and converted: what every exit reads from.
struct PricedPortfolio {
    /// One row per holding, in the order held, each carrying its source price and currency, its rate, and its settlement value.
    rows: Vec<Value>,
    total: PortfolioTotal,
    /// Whether every holding both priced and converted; a history point is appended only when this holds.
    complete: bool,
    /// The currency values are stated in, or `None` when the configured one does not settle — then each row's value is in its source's currency.
    settlement: Option<Currency>,
    /// One line per conversion actually used, in first-use order.
    conversions: Vec<String>,
}

/// Price every holding and convert each into the settlement currency. An unpriceable row keeps its place with `null` price and value; a row whose rate did not come back keeps its true price and currency with a `null` value. Nothing re-labels a price with the settlement currency.
fn price_holdings(cfg: &Config, holdings: &[Holding], cache: &mut PassCache) -> PricedPortfolio {
    // `None`: not a settling currency, so nothing is converted and a portfolio spanning two currencies gets no total.
    let settlement = Currency::settlement(&cfg.quote);
    let mut rows = Vec::with_capacity(holdings.len());
    let mut sum = 0.0;
    // Distinct currencies among the counted rows; more than one means the sum is a number in no currency.
    let mut currencies: Vec<&'static str> = Vec::new();
    let mut conversions: Vec<String> = Vec::new();
    let mut complete = true;
    for holding in holdings {
        // `price * quantity` can overflow to infinity with finite factors, so the product is checked too.
        let converted = match quote_cached(cfg, &holding.asset, &mut cache.prices) {
            Quote::Price(price, currency) => {
                let to = settlement.unwrap_or(currency);
                match fx_path(cfg, currency, to, &mut cache.rates) {
                    Ok(path) => {
                        let value = price * holding.quantity * path.factor;
                        if value.is_finite() {
                            Ok((price, currency, path, value))
                        } else {
                            // The conversion succeeded and the product overflowed; the rate stays on the row so readers can tell this from a rate that never came back.
                            Err((
                                Some((price, currency, Some(path.factor))),
                                format!(
                                    "{} × {} at {} is not a finite value",
                                    holding.asset.canonical(),
                                    holding.quantity,
                                    path.factor,
                                ),
                            ))
                        }
                    }
                    Err(why) => Err((
                        Some((price, currency, None)),
                        format!(
                            "{} priced in {} but no {}→{} rate came back this pass — {why}",
                            holding.asset.canonical(),
                            currency.code(),
                            currency.code(),
                            to.code(),
                        ),
                    )),
                }
            }
            Quote::Unknown => Err((
                None,
                format!(
                    "no valid quote came back for {} this pass",
                    holding.asset.canonical()
                ),
            )),
            Quote::Failed(why) => Err((None, why)),
        };
        match converted {
            Ok((price, currency, path, value)) => {
                sum += value;
                let stated = settlement.unwrap_or(currency);
                if !currencies.contains(&stated.code()) {
                    currencies.push(stated.code());
                }
                if !path.hops.is_empty() {
                    let described = path.describe();
                    if !conversions.contains(&described) {
                        conversions.push(described);
                    }
                }
                let mut row = json!({
                    "asset": holding.asset.symbol,
                    "venue": holding.asset.venue.prefix(),
                    "qty": round_to(holding.quantity, 8),
                    "price": round_to(price, 2),
                    "currency": currency.code(),
                    "value": round_to(value, 2),
                });
                // With no settlement currency a column of 1.0s would read as a conversion that happened.
                if settlement.is_some() {
                    row["rate"] = json!(round_to(path.factor, 8));
                }
                rows.push(row);
            }
            Err((priced, why)) => {
                complete = false;
                eprintln!("market: {why}");
                // A price that came back is kept even when conversion did not finish: a row with a rate and no value overflowed, one with neither had no rate.
                let (price, currency, rate) = match priced {
                    Some((price, currency, rate)) => (
                        json!(round_to(price, 2)),
                        json!(currency.code()),
                        rate.map_or(Value::Null, |rate| json!(round_to(rate, 8))),
                    ),
                    None => (Value::Null, Value::Null, Value::Null),
                };
                let mut row = json!({
                    "asset": holding.asset.symbol,
                    "venue": holding.asset.venue.prefix(),
                    "qty": round_to(holding.quantity, 8),
                    "price": price,
                    "currency": currency,
                    "value": Value::Null,
                });
                if settlement.is_some() {
                    row["rate"] = rate;
                }
                rows.push(row);
            }
        }
    }
    let total = match currencies.as_slice() {
        // Nothing was counted; the holdings themselves say whether that is a zero or an unknown.
        [] if holdings.is_empty() => PortfolioTotal::Empty,
        [] => PortfolioTotal::NonePriced,
        [only] if sum.is_finite() => PortfolioTotal::Priced {
            amount: sum,
            currency: (*only).to_string(),
        },
        [_] => PortfolioTotal::NotFinite,
        // Unreachable with a settlement currency configured; the whole point without one.
        many => PortfolioTotal::Unsettleable {
            configured: cfg.quote.clone(),
            currencies: many.iter().map(|c| (*c).to_string()).collect(),
        },
    };
    PricedPortfolio {
        rows,
        total,
        complete,
        settlement,
        conversions,
    }
}

/// Round for display. Rounding can create an infinity out of a finite input (`1e308 * 100`), which would serialize as `null` and read back as zero; a value too large to scale is returned unrounded.
fn round_to(value: f64, places: u32) -> f64 {
    let factor = 10f64.powi(places as i32);
    let scaled = value * factor;
    if !scaled.is_finite() {
        return value;
    }
    scaled.round() / factor
}

/// The holdings table. `Price` is in the source's currency (the `Priced in` column), `Value` in the settlement currency named by the heading; with no settlement currency the rate column is omitted and the heading carries no unit.
/// The table still goes out when there is no total, or the last published one would stay on screen as current.
fn holdings_table(priced: PricedPortfolio, at: &str) -> Value {
    let PricedPortfolio {
        mut rows,
        total,
        complete,
        settlement,
        conversions,
    } = priced;
    let mut total_row = json!({
        "asset": "Total",
        "venue": Value::Null,
        "qty": Value::Null,
        "price": Value::Null,
        "currency": total.currency_cell(),
        "value": total.value_cell(),
    });
    if settlement.is_some() {
        total_row["rate"] = Value::Null;
    }
    rows.push(total_row);
    let mut caption = match (&total, total.stated(), complete) {
        // An empty portfolio's Total cell is a number, so the caption explains the zero rather than denying it.
        (PortfolioTotal::Empty, _, _) => {
            format!("Priced at {at} — this Track holds nothing, so its total is 0")
        }
        (_, None, _) => format!(
            "Priced at {at} — no total is shown: {}",
            total
                .no_total_reason()
                .unwrap_or_else(|| "no reason recorded".into()),
        ),
        (_, Some((_, currency)), true) => format!("Priced at {at}, totalled in {currency}"),
        // "or rates": a row is left out both when its price and when its rate did not come back.
        (_, Some((_, currency)), false) => format!(
            "Priced at {at}, totalled in {currency} — some prices or exchange rates \
             unavailable; the total covers the rows that have both"
        ),
    };
    if !conversions.is_empty() {
        caption.push_str(". Converted at ");
        caption.push_str(&conversions.join("; "));
    }
    let mut columns = vec![
        json!({ "key": "asset", "label": "Asset" }),
        // The venue is its own column: `W` on two venues is two different companies.
        json!({ "key": "venue", "label": "Venue" }),
        json!({ "key": "qty", "label": "Quantity", "align": "right" }),
        json!({ "key": "price", "label": "Price", "align": "right" }),
        json!({ "key": "currency", "label": "Priced in" }),
    ];
    match settlement {
        Some(settlement) => {
            columns.push(json!({
                "key": "rate",
                "label": format!("Rate to {}", settlement.code()),
                "align": "right",
            }));
            columns.push(json!({
                "key": "value",
                "label": format!("Value ({})", settlement.code()),
                "align": "right",
            }));
        }
        None => columns.push(json!({ "key": "value", "label": "Value", "align": "right" })),
    }
    json!({
        "columns": columns,
        "rows": rows,
        "caption": caption,
        "highlight": "Total",
    })
}

/// The settlement currency a stored history point was written in, or `None` when it does not say. `None` is an unknowable unit, comparable to no other point — two unknowns are not the same unknown.
fn point_currency(point: &Value) -> Option<&str> {
    point.get("currency").and_then(Value::as_str)
}

/// History as a table, newest first, with the change against the previous point. The change is blank across a currency boundary, and on either side of a point that recorded no currency (its unit cannot be recovered).
fn history_table(points: &[Value]) -> Value {
    let mut rows: Vec<Value> = Vec::with_capacity(points.len());
    let mut unlabelled = 0usize;
    for (index, point) in points.iter().enumerate() {
        let total = point.get("total").and_then(Value::as_f64).unwrap_or(0.0);
        let currency = point_currency(point);
        if currency.is_none() {
            unlabelled += 1;
        }
        let previous = index.checked_sub(1).map(|i| &points[i]);
        // Both points must say what unit they are in, and say the same one; `None == None` would read two unrecorded units as one.
        let comparable = match (currency, previous.and_then(point_currency)) {
            (Some(this), Some(before)) => this == before,
            _ => false,
        };
        let change = match (comparable, previous) {
            (true, Some(previous)) => {
                let before = previous
                    .get("total")
                    .and_then(Value::as_f64)
                    .unwrap_or(total);
                json!(round_to(total - before, 2))
            }
            _ => Value::Null,
        };
        rows.push(json!({
            "at": point.get("at").and_then(Value::as_str).unwrap_or(""),
            "total": round_to(total, 2),
            "currency": currency.map_or(Value::Null, |c| json!(c)),
            "change": change,
        }));
    }
    rows.reverse();
    let mut caption = format!(
        "Total portfolio value over time, newest first — {} point{} since this plugin started watching",
        points.len(),
        if points.len() == 1 { "" } else { "s" },
    );
    if unlabelled > 0 {
        caption.push_str(&format!(
            ". {unlabelled} point{} recorded before this plugin stored a currency per point, so \
             what unit {} in cannot be recovered and no change is shown against {}",
            if unlabelled == 1 { " was" } else { "s were" },
            if unlabelled == 1 { "it is" } else { "they are" },
            if unlabelled == 1 { "it" } else { "them" },
        ));
    }
    json!({
        "columns": [
            { "key": "at", "label": "At" },
            // No unit in this heading: each row states its own.
            { "key": "total", "label": "Total", "align": "right" },
            { "key": "currency", "label": "Currency" },
            { "key": "change", "label": "Change", "align": "right" },
        ],
        "rows": rows,
        "caption": caption,
    })
}

/// Whether the overlay actually landed; a refused push leaves the previous value on display.
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
    // A never-written key answers `{"value": null}` (empty history). A failed read must not reach the writer as `[]`, which would truncate the series; `?` turns it into a skipped tick.
    Ok(result
        .get("value")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default())
}

/// Serializes [`refresh`]: the history cycle is a read-modify-write on one KV key, and the poll thread and a tool call can arrive at once. One global lock cannot deadlock against itself the way a per-Track map can.
static REFRESH_LOCK: Mutex<()> = Mutex::new(());

#[derive(Debug, PartialEq, Eq)]
enum Refreshed {
    Fully,
    /// A price was unavailable or a push was refused; the string says which.
    Partially(String),
    NothingHeld,
}

/// One refresh of one Track: price, push the holdings table, and only when the tick is complete and persisted append a history point. A tick that could not price part of the portfolio contributes no point: a subset total plotted against whole ones reads as a crash that never happened.
fn refresh(rpc: &Rpc, cfg: &Config, track_id: &str, cache: &mut PassCache) -> Refreshed {
    let _serialized = REFRESH_LOCK.lock();
    // Read the holdings here, inside the lock: a slow poll pass may reach this Track after a tool call already changed and re-published it, and publishing the caller's snapshot would revert it and append an obsolete total.
    let holdings = match load_holdings(rpc, track_id) {
        Ok(holdings) => holdings,
        Err(e) => {
            eprintln!("market: reading {track_id}'s holdings failed: {e}");
            return Refreshed::Partially("this Track's holdings could not be read".into());
        }
    };
    let at = now_rfc3339();

    // An empty portfolio still publishes, or someone who just sold out keeps seeing their old position. No history point: there is no portfolio to value.
    if holdings.is_empty() {
        return if push_overlay(
            rpc,
            track_id,
            "portfolio.holdings",
            holdings_table(price_holdings(cfg, &[], &mut PassCache::new()), &at),
        ) {
            Refreshed::NothingHeld
        } else {
            Refreshed::Partially("the (now empty) holdings table could not be published".into())
        };
    }

    let priced = price_holdings(cfg, &holdings, cache);
    let complete = priced.complete;

    // A sum of finite values can still overflow, and an unsettled currency leaves rows that do not sum; either way there is no total.
    if let Some(why) = priced.total.no_total_reason() {
        eprintln!("market: no total for {track_id} — {why}; publishing the rows without one");
    }
    // Taken before the payload consumes the portfolio, so the number appended and the number published are one value.
    let stated = priced
        .total
        .stated()
        .map(|(amount, currency)| (amount, currency.to_string()));
    let no_total_reason = priced.total.no_total_reason();
    if !push_overlay(
        rpc,
        track_id,
        "portfolio.holdings",
        holdings_table(priced, &at),
    ) {
        return Refreshed::Partially("the holdings table could not be published".into());
    }
    if !complete {
        return Refreshed::Partially(
            "some holdings could not be priced or converted; the history point was skipped".into(),
        );
    }
    // A portfolio with no total contributes no point; a figure assembled from the rows that happened to work is not the portfolio's value.
    let Some((total, currency)) = stated else {
        return Refreshed::Partially(format!(
            "there is no portfolio total — {}; the history point was skipped",
            no_total_reason.unwrap_or_else(|| "no reason recorded".into()),
        ));
    };

    let mut points = match load_history(rpc, track_id) {
        Ok(points) => points,
        Err(e) => {
            eprintln!("market: reading {track_id}'s history failed, leaving it untouched: {e}");
            return Refreshed::Partially("the history could not be read".into());
        }
    };
    // The unit is stored with the number so `history_table` can refuse to subtract across a currency boundary. Points already in the store record no currency.
    points.push(json!({ "at": at, "total": round_to(total, 2), "currency": currency }));
    if points.len() > MAX_HISTORY_POINTS {
        let drop = points.len() - MAX_HISTORY_POINTS;
        points.drain(0..drop);
    }
    // Persist before publishing: a published point the next tick reloads without reads as data loss rather than a failed write.
    if let Err(e) = rpc.call(
        "neige.kv.set",
        json!({ "key": history_key(track_id), "value": points }),
    ) {
        eprintln!("market: persisting {track_id}'s history failed: {e}");
        return Refreshed::Partially("the history point could not be persisted".into());
    }
    if !push_overlay(rpc, track_id, "portfolio.history", history_table(&points)) {
        return Refreshed::Partially("the history table could not be published".into());
    }
    Refreshed::Fully
}

/// One pass over every Track that holds something. The listing only names the Tracks; one price cache spans the pass.
fn refresh_all(rpc: &Rpc, cfg: &Config) {
    let track_ids = match portfolios(rpc) {
        Ok(portfolios) => portfolios,
        Err(e) => {
            eprintln!("market: listing portfolios failed; skipping this pass: {e}");
            return;
        }
    };
    let mut cache = PassCache::new();
    for track_id in track_ids {
        if let Refreshed::Partially(why) = refresh(rpc, cfg, &track_id, &mut cache) {
            eprintln!("market: incomplete refresh of {track_id} — {why}");
        }
    }
}

/// RFC-3339 UTC to the second, without pulling `chrono` in for one format.
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

/// Howard Hinnant's `civil_from_days`.
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

/// The one-line prose `market.holdings.list` answers with. The venue is glued to the symbol here because this exit has no columns. Three failures are told apart from the row: no price (quote did not come back), no rate (price but no rate), not a finite value (price and rate, product overflowed).
fn holdings_line(priced: &PricedPortfolio) -> String {
    priced
        .rows
        .iter()
        .map(|row| {
            let native = row["currency"].as_str();
            let unit = priced.settlement.map_or(native, |s| Some(s.code()));
            let value = match (row["value"].as_f64().zip(unit), native) {
                (Some((value, unit)), _) => format!("{value} {unit}"),
                // A rate on the row means the conversion succeeded, so what failed is the arithmetic; with no settlement currency the identity path cannot fail, so an absent value is an overflow too.
                (None, Some(native)) => match (priced.settlement, row["rate"].as_f64()) {
                    (Some(settlement), None) => format!(
                        "{} {native}, but no {native}→{} rate",
                        row["price"],
                        settlement.code(),
                    ),
                    _ => format!(
                        "{} {native} at {}, but the value is not a finite number",
                        row["price"], row["qty"],
                    ),
                },
                (None, None) => "price unavailable".into(),
            };
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

/// Every tool takes its Track from the kernel's `_meta`, never from its arguments; a call with no Track (a direct daemon connection) is refused rather than defaulted.
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
        // Same parse as the KV read path and `market.holdings.set`.
        let Some(asset) = parse_asset(raw) else {
            return tool_error(ASSET_SYNTAX_ERROR);
        };
        let canonical = asset.canonical();
        return match quote_asset(cfg, &asset) {
            // `currency`, not `quote`: the unit the source priced in is the only unit this number is true in.
            Quote::Price(price, currency) => text_result(
                format!("{canonical} = {price} {}", currency.code()),
                json!({
                    "asset": asset.symbol,
                    "venue": asset.venue.prefix(),
                    "price": price,
                    "currency": currency.code(),
                }),
            ),
            Quote::Unknown => tool_error(format!(
                "No valid quote came back for `{canonical}` — the source may not list \
                 this symbol, may have no way to spell it, or may be answering a row \
                 of zeros for a halted or delisted one."
            )),
            Quote::Failed(why) => tool_error(format!("Could not price {canonical} — {why}.")),
        };
    }

    // A read of public data that depends on the request alone: no Track is needed.
    if name == "market.series" {
        return series::handle(cfg, &args);
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
            // The same parse the KV read path uses, so what this tool accepts and what a stored row can spell cannot drift.
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
            // Both sides are canonical identities, so a legacy `BTC` row and an incoming `crypto:BTC` are one holding.
            holdings.retain(|h| h.asset != asset);
            // Zero is the spelling of "no longer held".
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
            // Recording a holding does not price it here: a tool that touches the network needs `openWorldHint: true`, which makes codex demand approval the kernel's agents (`approval_policy: "never"`) can never give. This writes state and wakes the poll thread instead.
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
            let priced = price_holdings(cfg, &holdings, &mut PassCache::new());
            let text = holdings_line(&priced);
            let PricedPortfolio {
                rows,
                total,
                complete,
                conversions,
                ..
            } = priced;
            // The prose and `structuredContent.total` are two exits on one fact and must not disagree: a partial total is a real number over the rows that priced and converted.
            let summary = match (complete, total.stated()) {
                (true, Some((amount, currency))) => {
                    format!("{text}. Total {} {currency}.", round_to(amount, 2))
                }
                (false, Some((amount, currency))) => format!(
                    "{text}. Partial total {} {currency} — it covers only the holdings that \
                     both priced and converted.",
                    round_to(amount, 2),
                ),
                (_, None) => format!(
                    "{text}. No total — {}.",
                    total
                        .no_total_reason()
                        .unwrap_or_else(|| "no reason recorded".into()),
                ),
            };
            // The conversions ride along so a caller can check what the total was converted at.
            let summary = if conversions.is_empty() {
                summary
            } else {
                format!("{summary} Converted at {}.", conversions.join("; "))
            };
            text_result(
                summary,
                json!({
                    "holdings": rows,
                    "total": total.value_cell(),
                    // `null` whenever `total` is; a number here must never be read against a unit from elsewhere.
                    "currency": total.currency_cell(),
                    // A per-row `rate` cannot say whether it was quoted or assumed; a machine consumer reads that here.
                    "conversions": conversions,
                    "complete": complete,
                }),
            )
        }
        other => tool_error(format!("unknown tool `{other}`")),
    }
}

fn main() {
    let rpc = Arc::new(Rpc::new());
    let reader = BufReader::new(std::io::stdin());

    // One configuration, shared: a second `initialize` must replace what everyone reads, or two pollers on different configs would publish alternating totals to the same overlays.
    let config = Arc::new(Mutex::new(Config::default()));
    let mut polling = false;

    // One worker for every tool call. Not the read loop: a tool call issues `neige.*` callbacks whose replies arrive on the stdin this loop reads, so handling it inline waits out its own timeout. Not a thread each: unbounded threads, and a failed `thread::spawn` would panic the reader. The single worker also serialises tool calls.
    let (tool_calls, tool_queue) = mpsc::channel::<Value>();
    // Recording a holding wakes the poll thread instead of pricing inline. `wake_tx` stays alive for the process's lifetime: if every sender dropped, `recv_timeout` would return `Disconnected` immediately and spin.
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
                // Read per call, so a call queued before a re-initialize still runs on the current configuration.
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
                    "market: configured — quote={} poll={}s binance={} sina={} tencent={}",
                    parsed.quote,
                    parsed.poll.as_secs(),
                    parsed.binance_endpoint,
                    parsed.sina_endpoint,
                    parsed.tencent_endpoint,
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
                            // Sleep, but wake early when a tool records a holding; draining the backlog keeps a burst of edits to one pass.
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

    /// Written through `parse_asset` on purpose: a hand-built `AssetId` would stop proving the parser agrees with it.
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

    /// The parser is a superset of what could already be stored: every pre-venue `[A-Z0-9]+` name must still parse, or read-side normalisation drops the row and the next `set` makes that permanent. A sample plus a bounded sweep, not a proof.
    #[test]
    fn i1_the_legacy_shapes_sampled_here_and_every_short_name_still_parse() {
        // Real names that have each broken a naive rule: leading digit, all digits, digits-then-letters, a bare quote asset.
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
        // Asserted against a literal expectation, never against `id(name)`, which would hold for any parser at all.
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
        assert_eq!(parse_asset(" btc "), Some(crypto("BTC")));
    }

    /// Binance's quote leg is pinned whatever the install settles in: the request target is `<SYMBOL>USDT`, read off the wire. A leg built from `cfg.quote` would ask for `BTCCNY`, which Binance does not list.
    #[test]
    fn binance_is_asked_for_the_usdt_leg_under_four_settlement_currencies() {
        for settlement in ["USDT", "CNY", "USD", "BUSD"] {
            let (endpoint, targets) = recording_endpoint("2.5");
            let cfg = Config {
                binance_endpoint: endpoint,
                quote: settlement.into(),
                ..cfg()
            };
            assert_eq!(
                quote_asset(&cfg, &id("BTC")),
                Quote::Price(2.5, Currency::Usdt),
                "settling in {settlement} must not change what the source quotes in"
            );
            let target = targets
                .recv_timeout(Duration::from_secs(5))
                .unwrap_or_else(|e| panic!("no request under {settlement}: {e}"));
            assert_eq!(target, "/api/v3/ticker/price?symbol=BTCUSDT");
        }
    }

    /// The leg prices itself with no request, under a settlement currency that is not `USDT` too; falling through to `binance_symbol` would ask for `USDTCNY`, which does not exist.
    #[test]
    fn the_binance_quote_leg_prices_itself_without_a_request() {
        for settlement in ["USDT", "CNY"] {
            // Both endpoints dead: anything that reached the network would come back `Failed`, so a green assertion means no request.
            let cfg = Config {
                binance_endpoint: "http://127.0.0.1:1".into(),
                sina_endpoint: "http://127.0.0.1:1".into(),
                quote: settlement.into(),
                ..cfg()
            };
            assert_eq!(
                quote_asset(&cfg, &id("USDT")),
                Quote::Price(1.0, Currency::Usdt),
                "the leg prices itself under settlement {settlement}"
            );
            // Only a CRYPTO identity: `US:USDT` is a different asset and goes to the stock source.
            assert!(
                matches!(quote_asset(&cfg, &id("US:USDT")), Quote::Failed(_)),
                "US:USDT must not borrow the crypto leg's 1.0"
            );
        }
    }

    /// A loopback stand-in for `hq.sinajs.cn`: the response is GBK (with real bytes including `B0 5C`, whose second byte is ASCII `\`) and `403 Forbidden` without the `Referer` header.
    fn sina_server<F>(respond: F) -> (String, mpsc::Receiver<String>)
    where
        F: Fn(&str) -> Vec<u8> + Send + 'static,
    {
        sina_server_with(move |target, has_referer| {
            if has_referer {
                ("200 OK", respond(target))
            } else {
                ("403 Forbidden", b"Forbidden".to_vec())
            }
        })
    }

    /// The transport half of [`sina_server`], with the status line left to the caller.
    fn sina_server_with<F>(respond: F) -> (String, mpsc::Receiver<String>)
    where
        F: Fn(&str, bool) -> (&'static str, Vec<u8>) + Send + 'static,
    {
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
                let head = String::from_utf8_lossy(&head).to_string();
                let target = head
                    .lines()
                    .next()
                    .unwrap_or_default()
                    .split_whitespace()
                    .nth(1)
                    .unwrap_or_default()
                    .to_string();
                if tx.send(target.clone()).is_err() {
                    return;
                }
                let has_referer = head
                    .to_ascii_lowercase()
                    .contains("referer: https://finance.sina.com.cn");
                let (status, body) = respond(&target, has_referer);
                let _ = write!(
                    stream,
                    "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len(),
                );
                let _ = stream.write_all(&body);
                let _ = stream.flush();
            }
        });
        (base, rx)
    }

    /// The real endpoint's answer to a request with no `Referer`, served unconditionally.
    fn sina_forbidden_server() -> (String, mpsc::Receiver<String>) {
        sina_server_with(|_, _| ("403 Forbidden", b"Forbidden".to_vec()))
    }

    // The fixture is shared with `crates/calm-server/tests/cases/market_plugin_process.rs`; two copies of a wire-format fixture drift apart.
    include!("sina_fixture.rs");

    fn sina_cfg(endpoint: String) -> Config {
        Config {
            sina_endpoint: endpoint,
            // Nothing crypto may reach the network in these tests.
            binance_endpoint: "http://127.0.0.1:1".into(),
            ..cfg()
        }
    }

    /// Each market's last price is read from that market's own field; a parser using one index everywhere reads a previous close or a company name as a price.
    #[test]
    fn each_market_is_priced_from_its_own_field_in_its_own_currency() {
        let (endpoint, targets) =
            sina_server(|target| sina_fixture_body(target, SINA_FIXTURE_ROWS));
        let cfg = sina_cfg(endpoint);
        for (name, expected, target) in [
            (
                "US:NVDA",
                Quote::Price(230.36, Currency::Usd),
                "/list=gb_nvda",
            ),
            (
                "HK:1810",
                Quote::Price(27.48, Currency::Hkd),
                "/list=hk01810",
            ),
            (
                "SH:600519",
                Quote::Price(1316.94, Currency::Cny),
                "/list=sh600519",
            ),
            (
                "SZ:000001",
                Quote::Price(11.70, Currency::Cny),
                "/list=sz000001",
            ),
        ] {
            assert_eq!(quote_asset(&cfg, &id(name)), expected, "{name}");
            assert_eq!(
                targets.recv_timeout(Duration::from_secs(5)).as_deref(),
                Ok(target),
                "{name} must be asked for under its own market prefix"
            );
        }
    }

    /// The successful lookup proves the shipping code sends `Referer`; the unconditional 403 proves a 403 is `Failed`, not `Unknown`.
    #[test]
    fn a_forbidden_response_is_a_failed_lookup_and_the_referer_is_what_avoids_it() {
        let (endpoint, _targets) =
            sina_server(|target| sina_fixture_body(target, SINA_FIXTURE_ROWS));
        assert_eq!(
            quote_asset(&sina_cfg(endpoint), &id("US:NVDA")),
            Quote::Price(230.36, Currency::Usd),
            "the shipping request must carry the Referer this server demands"
        );

        let (endpoint, _targets) = sina_forbidden_server();
        let refused = quote_asset(&sina_cfg(endpoint), &id("US:NVDA"));
        assert!(
            matches!(&refused, Quote::Failed(why) if why.contains("403")),
            "a refused request is a failure, not an unknown name: {refused:?}"
        );
    }

    /// Sina's three ways of saying "no price": an empty payload (unlisted) is `Unknown`; a row of `0.0000` (halted, or nonexistent) is `Unknown`, not a price; a response with no row at all is `Failed`.
    #[test]
    fn an_empty_row_a_zero_row_and_a_missing_row_are_three_different_answers() {
        let (endpoint, _targets) = sina_server(|target| {
            sina_fixture_body(
                target,
                &[("gb_ena", "ENA,0.0000,0.00,2014-04-19 10:06:28,0.0000")],
            )
        });
        let cfg = sina_cfg(endpoint);
        assert_eq!(quote_asset(&cfg, &id("US:DOGE")), Quote::Unknown, "empty");
        assert_eq!(quote_asset(&cfg, &id("US:ENA")), Quote::Unknown, "0.0000");

        let (endpoint, _targets) =
            sina_server(|_| b"var hq_str_gb_other=\"OTHER,1.0\";\n".to_vec());
        let cfg = sina_cfg(endpoint);
        let answered = quote_asset(&cfg, &id("US:NVDA"));
        assert!(
            matches!(&answered, Quote::Failed(why) if why.contains("gb_nvda")),
            "a response with no row for the symbol asked about is malformed: {answered:?}"
        );
    }

    /// A code whose range does not fix its currency is refused, `Failed` and without a request. A cross-currency total cannot catch this: `sh900932` (USD), `sz200725` (HKD) and `hk89988` (CNY) are each listed on one exchange and would all be published as `CNY`.
    #[test]
    fn a_code_whose_currency_the_code_does_not_fix_is_refused_before_any_request() {
        // The fixture would answer all three with a price, so an asking plugin would come back `Price`, not `Failed`.
        let (endpoint, targets) = sina_server(|target| {
            sina_fixture_body(
                target,
                &[
                    ("sh900932", "<NAME>,0.386,0.385,0.385,0.388,0.383"),
                    ("sz200725", "<NAME>,4.750,4.750,4.770,4.810,4.750"),
                    (
                        "hk89988",
                        "BABA-WR,<NAME>,94.150,94.450,93.850,93.900,94.450",
                    ),
                ],
            )
        });
        let cfg = sina_cfg(endpoint);
        for (name, currency_it_is_really_in) in [
            ("SH:900932", "USD"),
            ("SZ:200725", "HKD"),
            ("HK:89988", "CNY"),
        ] {
            let answered = quote_asset(&cfg, &id(name));
            assert!(
                matches!(&answered, Quote::Failed(why) if why.contains("cannot determine what currency")),
                "{name} is quoted in {currency_it_is_really_in}, not in its venue's \
                 default; it must be refused rather than priced: {answered:?}"
            );
        }
        assert!(
            targets.try_recv().is_err(),
            "a code with no determined currency must not even be asked about"
        );
    }

    /// `CN:` parses so that a row written under it cannot vanish (a non-parsing row is dropped silently and erased on the next `set`), and is never priced: the answer is a `Failed` naming the two prefixes.
    #[test]
    fn a_stored_cn_holding_reads_back_and_is_refused_out_loud() {
        assert_eq!(id("cn:600519").canonical(), "CN:600519");
        let stored = json!([
            { "asset": "CN:600519", "quantity": 2.0 },
            { "asset": "BTC", "quantity": 1.0 },
        ]);
        assert_eq!(
            holdings_from_value(Some(&stored), "trk"),
            vec![holding("CN:600519", 2.0), holding("CRYPTO:BTC", 1.0)],
            "a CN row must survive the read path that rewrites the store",
        );

        // The fixture would answer `sh600519`, so a `CN` that fell back to asking Shanghai would come back `Price` here.
        let (endpoint, targets) = sina_server(|target| {
            sina_fixture_body(
                target,
                &[
                    ("sh600519", "<NAME>,1324.000,1330.000,1316.940"),
                    ("sz600519", "<NAME>,1.000,1.000,1.000"),
                ],
            )
        });
        let cfg = sina_cfg(endpoint);
        let answered = quote_asset(&cfg, &id("CN:600519"));
        assert!(
            matches!(
                &answered,
                Quote::Failed(why)
                    if why.contains("CN:600519")
                        && why.contains("SH:600519")
                        && why.contains("SZ:600519")
            ),
            "a CN holding must fail visibly and name both prefixes it could be \
             re-recorded under: {answered:?}",
        );
        assert!(
            targets.try_recv().is_err(),
            "`CN` names no exchange, so there is nothing to ask",
        );
    }

    /// The mainland fund ranges are priced in renminbi. Being inside an allowed range is not a promise the source lists the code: `sz162201` answers an empty row, which is `Unknown` after a real request.
    #[test]
    fn the_mainland_fund_ranges_price_in_renminbi() {
        let (endpoint, targets) = sina_server(|target| {
            sina_fixture_body(
                target,
                &[
                    // Shanghai `5xxxxx`, including a money-market fund whose ~100 unit price is real.
                    ("sh510300", "<NAME>,4.620,4.630,4.635,4.640,4.610"),
                    ("sh563210", "<NAME>,1.940,1.945,1.949,1.955,1.938"),
                    ("sh511990", "<NAME>,99.990,99.995,99.999,100.000,99.980"),
                    ("sz159915", "<NAME>,3.330,3.335,3.338,3.350,3.320"),
                ],
            )
        });
        let cfg = sina_cfg(endpoint);
        for (name, target, price) in [
            ("SH:510300", "/list=sh510300", 4.635),
            ("SH:563210", "/list=sh563210", 1.949),
            ("SH:511990", "/list=sh511990", 99.999),
            ("SZ:159915", "/list=sz159915", 3.338),
        ] {
            assert_eq!(
                quote_asset(&cfg, &id(name)),
                Quote::Price(price, Currency::Cny),
                "{name} is a renminbi fund and must be priced",
            );
            assert_eq!(
                targets.recv_timeout(Duration::from_secs(5)).as_deref(),
                Ok(target),
            );
        }
        // `16xxxx` is an allowed range too; this one is simply not listed.
        assert_eq!(quote_asset(&cfg, &id("SZ:162201")), Quote::Unknown);
        assert_eq!(
            targets.recv_timeout(Duration::from_secs(5)).as_deref(),
            Ok("/list=sz162201"),
            "an allowed range is asked about; only the answer is empty",
        );
    }

    /// Each A-share range resolves against its own exchange, from the identity rather than the digits; the fixture answers neither wrong-exchange code, so a misrouted request comes back `Unknown`.
    #[test]
    fn a_mainland_code_is_asked_of_the_exchange_the_identity_names() {
        let (endpoint, targets) = sina_server(|target| {
            sina_fixture_body(
                target,
                &[
                    // Both exist live; neither is what the identities below name.
                    ("sh000001", "<NAME>,3942.5093,3930.1164,3933.2397"),
                    ("sz600519", "<NAME>,1.000,1.000,1.000"),
                ],
            )
        });
        let cfg = sina_cfg(endpoint);
        // `SH:600519` asks Shanghai and must not fall through to the `sz600519` row sitting right there.
        assert_eq!(quote_asset(&cfg, &id("SH:600519")), Quote::Unknown);
        assert_eq!(
            targets.recv_timeout(Duration::from_secs(5)).as_deref(),
            Ok("/list=sh600519"),
            "one exchange, one symbol, one request",
        );
        assert_eq!(quote_asset(&cfg, &id("SZ:000001")), Quote::Unknown);
        assert_eq!(
            targets.recv_timeout(Duration::from_secs(5)).as_deref(),
            Ok("/list=sz000001"),
        );
    }

    #[test]
    fn chinext_prices_in_renminbi_and_a_non_six_digit_code_is_not_requested() {
        let (endpoint, targets) =
            sina_server(|target| sina_fixture_body(target, SINA_FIXTURE_ROWS));
        let cfg = sina_cfg(endpoint);
        assert_eq!(
            quote_asset(&cfg, &id("SZ:300750")),
            Quote::Price(348.20, Currency::Cny)
        );
        assert_eq!(
            targets.recv_timeout(Duration::from_secs(5)).as_deref(),
            Ok("/list=sz300750"),
        );
        assert_eq!(quote_asset(&cfg, &id("SH:60051")), Quote::Unknown);
        assert_eq!(quote_asset(&cfg, &id("SZ:MAOTAI")), Quote::Unknown);
        assert!(
            targets.try_recv().is_err(),
            "a name this source cannot spell must not reach it"
        );
    }

    /// The symbol reaches the URL unaltered as `hk<five digits>`, and a non-code gets no request. `HK:1` is `hk00001` (CKH Holdings), a real case.
    #[test]
    fn a_hong_kong_code_is_padded_and_a_non_code_is_not_requested() {
        let (endpoint, targets) = sina_server(|target| {
            sina_fixture_body(
                target,
                &[
                    (
                        "hk01810",
                        "XIAOMI-W,<NAME>,28.220,28.440,28.400,27.120,27.480",
                    ),
                    (
                        "hk00001",
                        "CKH HOLDINGS,<NAME>,70.150,69.850,70.150,69.200,69.300",
                    ),
                ],
            )
        });
        let cfg = sina_cfg(endpoint);
        assert_eq!(
            quote_asset(&cfg, &id("HK:1810")),
            Quote::Price(27.48, Currency::Hkd)
        );
        assert_eq!(
            targets.recv_timeout(Duration::from_secs(5)).as_deref(),
            Ok("/list=hk01810"),
        );
        assert_eq!(
            quote_asset(&cfg, &id("HK:1")),
            Quote::Price(69.30, Currency::Hkd)
        );
        assert_eq!(
            targets.recv_timeout(Duration::from_secs(5)).as_deref(),
            Ok("/list=hk00001"),
            "`HK:1` is the identity `HK:00001`, and it is asked for, not refused",
        );
        // `TENCENT` is a legal identity this source cannot spell: `Unknown` without a request rather than a padded guess.
        assert_eq!(quote_asset(&cfg, &id("HK:TENCENT")), Quote::Unknown);
        // Longer than five digits with nothing to strip: no Hong Kong code.
        assert_eq!(quote_asset(&cfg, &id("HK:123456")), Quote::Unknown);
        assert!(
            targets.try_recv().is_err(),
            "a name this source cannot spell must not reach it"
        );
    }

    /// Every spelling of one Hong Kong code is one identity: as two `AssetId`s, `holdings.retain` matches neither and a re-recorded position is held twice in a total with nothing visibly wrong.
    #[test]
    fn hong_kong_spellings_of_one_code_are_one_identity() {
        for spelling in ["HK:1810", "HK:01810", "HK:001810", "hk:0001810"] {
            assert_eq!(
                id(spelling),
                id("HK:01810"),
                "`{spelling}` names the same security"
            );
            assert_eq!(id(spelling).canonical(), "HK:01810", "`{spelling}`");
        }

        // The fold reaches past five characters, so a refused range answers the same however it was written.
        let (endpoint, targets) = sina_server(|target| {
            sina_fixture_body(
                target,
                &[(
                    "hk89988",
                    "BABA-WR,<NAME>,94.150,94.450,93.850,93.900,94.450",
                )],
            )
        });
        let cfg = sina_cfg(endpoint);
        assert_eq!(id("HK:089988"), id("HK:89988"));
        let padded = quote_asset(&cfg, &id("HK:089988"));
        assert_eq!(padded, quote_asset(&cfg, &id("HK:89988")));
        assert!(
            matches!(&padded, Quote::Failed(why) if why.contains("cannot determine what currency")),
            "both spellings must reach the SAME refusal, not one refusal and \
             one `Unknown`: {padded:?}"
        );
        assert!(
            targets.try_recv().is_err(),
            "a code with no determined currency must not be asked about under \
             either spelling",
        );
    }

    /// Stocks and rates off one Sina fixture, with Binance unreachable: `USDT` prices at 1.0 off the pinned leg and settles at the assumed parity, neither a request.
    fn converting_cfg(quote: &str) -> (Config, mpsc::Receiver<String>) {
        let (sina, sina_targets) =
            sina_server(|target| sina_fixture_body(target, &sina_fixture_all_rows()));
        (
            Config {
                quote: quote.into(),
                sina_endpoint: sina,
                binance_endpoint: "http://127.0.0.1:1".into(),
                ..cfg()
            },
            sina_targets,
        )
    }

    /// Four rows quoted in USDT, USD, HKD and CNY, settling in CNY, one total. Every rate is read off the fixture's FX rows except the USDT parity step.
    #[test]
    fn a_four_currency_portfolio_totals_through_real_rates() {
        let (cfg, sina_targets) = converting_cfg("CNY");
        let priced = price_holdings(
            &cfg,
            &[
                holding("USDT", 1.0),
                holding("US:NVDA", 1.0),
                holding("HK:1810", 100.0),
                holding("SH:600519", 2.0),
            ],
            &mut PassCache::new(),
        );
        assert!(priced.complete, "{:?}", priced.rows);
        assert_eq!(priced.settlement, Some(Currency::Cny));

        let quoted: Vec<(&Value, &Value)> = priced
            .rows
            .iter()
            .map(|row| (&row["price"], &row["currency"]))
            .collect();
        assert_eq!(
            quoted,
            vec![
                (&json!(1.0), &json!("USDT")),
                (&json!(230.36), &json!("USD")),
                (&json!(27.48), &json!("HKD")),
                (&json!(1316.94), &json!("CNY")),
            ],
            "a converted row must not be relabelled with the settlement currency",
        );
        let rates: Vec<f64> = priced
            .rows
            .iter()
            .map(|row| row["rate"].as_f64().expect("a rate"))
            .collect();
        assert_eq!(
            rates,
            vec![6.7111, 6.7111, 0.85601781, 1.0],
            "USDT rides USD's rate at par; CNY→CNY is an identity",
        );
        let values: Vec<f64> = priced
            .rows
            .iter()
            .map(|row| row["value"].as_f64().expect("a value"))
            .collect();
        assert_eq!(values, vec![6.71, 1545.97, 2352.34, 2633.88]);
        assert_eq!(
            priced.total,
            PortfolioTotal::Priced {
                amount: 6538.897024689601,
                currency: "CNY".into(),
            },
            "the total this Track had no way to state before this slice",
        );

        // The reader is told what the numbers were converted at, hop by hop, and which hop is not a quote.
        let table = holdings_table(priced, "2026-09-07T12:00:00Z");
        assert_eq!(validate_payload(KIND_TABLE, &table), Ok(()));
        let caption = table["caption"].as_str().expect("caption");
        assert!(caption.contains("totalled in CNY"), "{caption}");
        assert!(
            caption.contains("USD→CNY 6.7111 (fx_susdcny@sina 6.7111)"),
            "{caption}",
        );
        assert!(
            caption.contains("HKD→CNY 0.85601781 (fx_shkdcny@sina 0.85601781)"),
            "{caption}",
        );
        assert!(
            caption.contains(
                "USDT→CNY 6.7111 (USDT taken as 1 USD — assumed, not quoted × \
                 fx_susdcny@sina 6.7111)"
            ),
            "the assumed step is named as assumed, and the quoted step names its \
             symbol: {caption}",
        );
        let columns: Vec<&str> = table["columns"]
            .as_array()
            .expect("columns")
            .iter()
            .map(|column| column["label"].as_str().unwrap_or_default())
            .collect();
        assert_eq!(
            columns,
            vec![
                "Asset",
                "Venue",
                "Quantity",
                "Price",
                "Priced in",
                "Rate to CNY",
                "Value (CNY)",
            ],
            "the two units on this table are labelled apart: {table}",
        );
        let total_row = table["rows"]
            .as_array()
            .expect("rows")
            .last()
            .expect("total");
        assert_eq!(total_row["value"], json!(6538.9));
        assert_eq!(total_row["currency"], json!("CNY"));

        // Sina lists every ordered pair, so a route that divided into the opposite one shows up as the wrong symbol rather than a failure; one `fx_susdcny` serves both the USD and the USDT row.
        let mut asked: Vec<String> = sina_targets.try_iter().collect();
        asked.sort();
        assert_eq!(
            asked,
            vec![
                "/list=fx_shkdcny",
                "/list=fx_susdcny",
                "/list=gb_nvda",
                "/list=hk01810",
                "/list=sh600519",
            ],
            "no `fx_scnyusd`, no `fx_scnyhkd`, and no second request for a pair \
             two rows share",
        );
    }

    /// `USDT` settles as `USD` at an assumed parity, and every exit says the parity is assumed.
    #[test]
    fn the_usdt_parity_is_published_as_an_assumption_not_as_a_rate() {
        let (cfg, sina_targets) = converting_cfg("USD");
        let priced = price_holdings(&cfg, &[holding("USDT", 1000.0)], &mut PassCache::new());
        assert!(priced.complete);
        assert_eq!(priced.settlement, Some(Currency::Usd));
        assert_eq!(priced.rows[0]["currency"], json!("USDT"));
        assert_eq!(priced.rows[0]["rate"].as_f64(), Some(1.0));
        assert_eq!(priced.rows[0]["value"].as_f64(), Some(1000.0));
        assert_eq!(
            priced.total,
            PortfolioTotal::Priced {
                amount: 1000.0,
                currency: "USD".into(),
            },
            "the total is stated in USD — the unit the rates are in — not in USDT",
        );
        assert_eq!(
            priced.conversions,
            vec!["USDT→USD 1 (USDT taken as 1 USD — assumed, not quoted)".to_string()],
            "the word `assumed` is what a reader has to be able to see",
        );
        assert_eq!(
            sina_targets.try_iter().count(),
            0,
            "the parity is not fetched, so nothing was asked",
        );

        // The default `quote` resolves to the same settlement, so an existing install keeps its total.
        assert_eq!(Currency::settlement("USDT"), Some(Currency::Usd));
        assert_eq!(
            Currency::settlement(&Config::default().quote),
            Some(Currency::Usd)
        );
    }

    /// A fiat pair is asked for in the direction it is wanted: `fx_susdcny` (6.7111) and `fx_scnyusd` (0.149007) are not reciprocals, having been updated an hour apart.
    #[test]
    fn each_fiat_direction_is_its_own_quote_rather_than_a_reciprocal() {
        let (cfg, sina_targets) = converting_cfg("CNY");
        let priced = price_holdings(&cfg, &[holding("US:NVDA", 1.0)], &mut PassCache::new());
        assert_eq!(priced.rows[0]["rate"].as_f64(), Some(6.7111));
        assert_eq!(priced.rows[0]["value"].as_f64(), Some(1545.97));
        let asked: Vec<String> = sina_targets.try_iter().collect();
        assert!(
            asked.contains(&"/list=fx_susdcny".to_string())
                && !asked.contains(&"/list=fx_scnyusd".to_string()),
            "USD→CNY asks for the USD→CNY row: {asked:?}",
        );

        let (cfg, _targets) = converting_cfg("USD");
        let priced = price_holdings(&cfg, &[holding("SH:600519", 1.0)], &mut PassCache::new());
        assert_eq!(
            priced.rows[0]["rate"].as_f64(),
            Some(0.149007),
            "0.14900538 would be the reciprocal of the other row",
        );

        // Hong Kong has its own pair per settlement currency rather than being routed through the other one.
        let (cfg, sina_targets) = converting_cfg("USD");
        let priced = price_holdings(&cfg, &[holding("HK:1810", 100.0)], &mut PassCache::new());
        assert_eq!(priced.rows[0]["rate"].as_f64(), Some(0.12755265));
        assert_eq!(priced.rows[0]["value"].as_f64(), Some(350.51));
        let asked: Vec<String> = sina_targets.try_iter().collect();
        assert!(
            asked.contains(&"/list=fx_shkdusd".to_string()),
            "HKD→USD is one quote, not HKD→CNY→USD: {asked:?}",
        );
    }

    /// A rate that did not come back leaves its row with its true price and currency, no value, no rate and no place in the total; the pass is incomplete.
    #[test]
    fn a_holding_whose_rate_did_not_come_back_is_left_out_of_the_total() {
        let (sina, _targets) = sina_server(|target| sina_fixture_body(target, SINA_FIXTURE_ROWS));
        let cfg = Config {
            quote: "CNY".into(),
            sina_endpoint: sina,
            binance_endpoint: "http://127.0.0.1:1".into(),
            ..cfg()
        };
        let priced = price_holdings(
            &cfg,
            &[holding("US:NVDA", 1.0), holding("SH:600519", 2.0)],
            &mut PassCache::new(),
        );
        assert!(!priced.complete, "the USD row could not be converted");
        assert_eq!(
            priced.rows[0]["price"].as_f64(),
            Some(230.36),
            "the price came back and is kept: what failed is the conversion",
        );
        assert_eq!(priced.rows[0]["currency"], json!("USD"));
        assert!(priced.rows[0]["rate"].is_null(), "{:?}", priced.rows[0]);
        assert!(priced.rows[0]["value"].is_null(), "{:?}", priced.rows[0]);
        assert_eq!(
            priced.total,
            PortfolioTotal::Priced {
                amount: 2633.88,
                currency: "CNY".into(),
            },
            "the CNY row needs no conversion and is counted; the USD row is not",
        );
        let line = holdings_line(&priced);
        assert!(
            line.contains("230.36 USD, but no USD→CNY rate"),
            "the reader is told which of the two lookups failed: {line}",
        );
        let table = holdings_table(priced, "2026-09-07T12:00:00Z");
        assert_eq!(validate_payload(KIND_TABLE, &table), Ok(()));
        let caption = table["caption"].as_str().expect("caption");
        assert!(
            caption.contains("some prices or exchange rates unavailable"),
            "{caption}",
        );
    }

    /// No stale rates: a pass whose rate source is unreachable converts nothing, even when an earlier pass converted the same pair. The second pass still prices the holding, so the fallback path is actually reached.
    #[test]
    fn a_rate_from_an_earlier_pass_is_never_reused() {
        let (working, _targets) = converting_cfg("CNY");
        let first = price_holdings(&working, &[holding("US:NVDA", 1.0)], &mut PassCache::new());
        assert_eq!(first.rows[0]["rate"].as_f64(), Some(6.7111));
        assert_eq!(first.rows[0]["value"].as_f64(), Some(1545.97));

        // Same asset, same pair, a pass later; the endpoint no longer lists any `fx_` row.
        let (stocks_only, _stock_targets) =
            sina_server(|target| sina_fixture_body(target, SINA_FIXTURE_ROWS));
        let broken = Config {
            sina_endpoint: stocks_only,
            ..working
        };
        let second = price_holdings(&broken, &[holding("US:NVDA", 1.0)], &mut PassCache::new());
        assert_eq!(
            second.rows[0]["price"].as_f64(),
            Some(230.36),
            "the price came back, so what follows is about the rate alone: {:?}",
            second.rows[0],
        );
        assert!(
            second.rows[0]["rate"].is_null(),
            "6.7111 here would be last pass's number: {:?}",
            second.rows[0],
        );
        assert!(
            second.rows[0]["value"].is_null(),
            "1545.97 here would be this pass's price at last pass's rate: {:?}",
            second.rows[0],
        );
        assert_eq!(second.total, PortfolioTotal::NonePriced);
        assert!(!second.complete);
    }

    /// An empty portfolio's total is 0; a portfolio nothing could be valued in has none. Each case is asserted on the cell and the caption together, since checking either alone passes when they contradict.
    #[test]
    fn an_empty_portfolio_totals_zero_and_an_unvalued_one_totals_nothing() {
        let priced = price_holdings(&cfg(), &[], &mut PassCache::new());
        assert_eq!(priced.total, PortfolioTotal::Empty);
        assert_eq!(priced.total.value_cell(), json!(0.0));
        assert!(priced.complete, "nothing failed: there was nothing to do");
        let table = holdings_table(priced, "2026-09-07T12:00:00Z");
        assert_eq!(validate_payload(KIND_TABLE, &table), Ok(()));
        let total_row = table["rows"]
            .as_array()
            .expect("rows")
            .last()
            .expect("total");
        assert_eq!(total_row["value"], json!(0.0));
        let caption = table["caption"].as_str().expect("caption");
        assert!(
            caption.contains("this Track holds nothing, so its total is 0"),
            "the caption explains the zero: {caption}",
        );
        assert!(
            !caption.contains("no total is shown"),
            "a 0 in the cell and `no total is shown` in the caption is the \
             contradiction this split removed: {caption}",
        );

        // Non-empty and nothing priced: the total is unknown, and 0 is a wrong answer for it.
        let cfg = Config {
            binance_endpoint: "http://127.0.0.1:1".into(),
            sina_endpoint: "http://127.0.0.1:1".into(),
            ..cfg()
        };
        let priced = price_holdings(
            &cfg,
            &[holding("ZZZZ", 1.0), holding("US:NVDA", 1.0)],
            &mut PassCache::new(),
        );
        assert_eq!(priced.total, PortfolioTotal::NonePriced);
        assert!(
            priced.total.value_cell().is_null(),
            "0 would read as `this portfolio is worth nothing`",
        );
        assert!(!priced.complete);
        assert_eq!(priced.rows.len(), 2, "the rows survive: {:?}", priced.rows);
        let table = holdings_table(priced, "2026-09-07T12:00:00Z");
        assert_eq!(validate_payload(KIND_TABLE, &table), Ok(()));
        let total_row = table["rows"]
            .as_array()
            .expect("rows")
            .last()
            .expect("total");
        assert!(total_row["value"].is_null(), "{total_row}");
        let caption = table["caption"].as_str().expect("caption");
        assert!(caption.contains("no total is shown"), "{caption}");
        assert!(
            caption.contains("not one holding could be priced and converted"),
            "{caption}",
        );
    }

    /// A non-empty portfolio whose every rate is unavailable states no total, not a zero.
    #[test]
    fn a_portfolio_whose_every_rate_is_unavailable_states_no_total_not_a_zero() {
        let (sina, _targets) = sina_server(|target| sina_fixture_body(target, SINA_FIXTURE_ROWS));
        let cfg = Config {
            quote: "CNY".into(),
            sina_endpoint: sina,
            binance_endpoint: "http://127.0.0.1:1".into(),
            ..cfg()
        };
        let priced = price_holdings(
            &cfg,
            &[holding("US:NVDA", 1.0), holding("HK:1810", 100.0)],
            &mut PassCache::new(),
        );
        assert_eq!(
            priced.rows[0]["price"].as_f64(),
            Some(230.36),
            "both holdings priced: what is missing is only the rates",
        );
        assert_eq!(priced.rows[1]["price"].as_f64(), Some(27.48));
        assert_eq!(priced.total, PortfolioTotal::NonePriced);
        assert!(
            priced.total.value_cell().is_null(),
            "this portfolio is worth thousands; `0` would be a wrong number, \
             not a missing one",
        );
        assert!(priced.total.stated().is_none());
        assert!(!priced.complete);
        let table = holdings_table(priced, "2026-09-07T12:00:00Z");
        assert_eq!(validate_payload(KIND_TABLE, &table), Ok(()));
        let total_row = table["rows"]
            .as_array()
            .expect("rows")
            .last()
            .expect("total");
        assert!(total_row["value"].is_null(), "{total_row}");
        let caption = table["caption"].as_str().expect("caption");
        assert!(caption.contains("no total is shown"), "{caption}");
        assert!(
            caption.contains("not one holding could be priced and converted"),
            "{caption}",
        );
    }

    /// An overflow is not a missing rate: settling USD in USD is the identity path, and what fails is `price × quantity` at `1e308` shares.
    #[test]
    fn a_value_that_overflows_is_not_reported_as_a_missing_rate() {
        let (cfg, sina_targets) = converting_cfg("USD");
        let priced = price_holdings(&cfg, &[holding("US:NVDA", 1e308)], &mut PassCache::new());
        assert!(!priced.complete);
        assert_eq!(priced.rows[0]["price"].as_f64(), Some(230.36));
        assert_eq!(priced.rows[0]["currency"], json!("USD"));
        assert_eq!(
            priced.rows[0]["rate"].as_f64(),
            Some(1.0),
            "the conversion succeeded — it is the identity — so its rate stays \
             on the row: {:?}",
            priced.rows[0],
        );
        assert!(priced.rows[0]["value"].is_null(), "{:?}", priced.rows[0]);
        assert_eq!(
            sina_targets
                .try_iter()
                .filter(|target| target.contains("fx_"))
                .count(),
            0,
            "no rate was ever looked for, so none can be missing",
        );
        let line = holdings_line(&priced);
        assert!(
            line.contains("not a finite number"),
            "the reason has to be the one that happened: {line}",
        );
        assert!(
            !line.contains("rate"),
            "`no USD→USD rate` names a lookup that never happened: {line}",
        );

        // The same holding with no settlement currency: still an overflow, still not a rate.
        let (cfg, _targets) = converting_cfg("HKD");
        let priced = price_holdings(&cfg, &[holding("US:NVDA", 1e308)], &mut PassCache::new());
        assert_eq!(priced.settlement, None);
        assert!(priced.rows[0]["value"].is_null());
        let line = holdings_line(&priced);
        assert!(line.contains("not a finite number"), "{line}");
        assert!(!line.contains('?'), "{line}");
    }

    /// A settlement currency this plugin does not settle in (`HKD`, or any unvalidated string) converts nothing: every row stays in its source's currency, and a portfolio spanning two gets no total.
    #[test]
    fn a_settlement_currency_this_plugin_does_not_settle_in_converts_nothing() {
        assert_eq!(Currency::settlement("HKD"), None);
        let (cfg, sina_targets) = converting_cfg("HKD");
        let priced = price_holdings(
            &cfg,
            &[holding("USDT", 1.0), holding("US:NVDA", 1.0)],
            &mut PassCache::new(),
        );
        assert!(priced.complete, "both rows priced: {:?}", priced.rows);
        assert_eq!(priced.settlement, None);
        assert!(priced.conversions.is_empty());
        assert_eq!(
            sina_targets
                .try_iter()
                .filter(|t| t.contains("fx_"))
                .count(),
            0,
            "with nothing to settle into, no rate is fetched",
        );
        assert_eq!(
            priced.total,
            PortfolioTotal::Unsettleable {
                configured: "HKD".into(),
                currencies: vec!["USDT".into(), "USD".into()],
            },
        );
        assert_eq!(priced.rows[0]["currency"], json!("USDT"));
        assert_eq!(priced.rows[1]["currency"], json!("USD"));
        assert_eq!(priced.rows[1]["value"].as_f64(), Some(230.36));

        assert!(priced.total.stated().is_none());
        assert!(priced.total.value_cell().is_null());
        assert!(priced.total.currency_cell().is_null());
        let line = holdings_line(&priced);
        assert!(line.contains("1 USDT"), "{line}");
        assert!(line.contains("230.36 USD"), "{line}");
        assert!(!line.contains("231.36"), "{line}");

        let table = holdings_table(priced, "2026-09-07T12:00:00Z");
        assert_eq!(validate_payload(KIND_TABLE, &table), Ok(()));
        let table_rows = table["rows"].as_array().expect("rows");
        assert!(
            table_rows.last().expect("total row")["value"].is_null(),
            "231.36 is not the value of this portfolio: {table_rows:?}"
        );
        let caption = table["caption"].as_str().expect("caption");
        assert!(caption.contains("quoted in USDT and USD"), "{caption}");
        assert!(caption.contains("`HKD`"), "{caption}");
        assert!(caption.contains("`USD` and `CNY` are"), "{caption}");
        let columns: Vec<&str> = table["columns"]
            .as_array()
            .expect("columns")
            .iter()
            .map(|column| column["label"].as_str().unwrap_or_default())
            .collect();
        assert_eq!(
            columns,
            vec!["Asset", "Venue", "Quantity", "Price", "Priced in", "Value"],
            "no rate column and no unit on `Value`: nothing was converted",
        );
    }

    /// A portfolio already in the settlement currency needs no rate at all. The stock half asserts no `fx_` request appears in the log of a server that would answer one; the crypto half reaches no endpoint.
    #[test]
    fn a_portfolio_already_in_the_settlement_currency_asks_for_no_rate() {
        let (refusing, refused) = sina_forbidden_server();
        let cfg = Config {
            quote: "CNY".into(),
            binance_endpoint: "http://127.0.0.1:1".into(),
            sina_endpoint: refusing,
            ..cfg()
        };
        // This table carries the FX rows too, so the absence below is the plugin not asking, not the fixture refusing.
        let (fixture, fixture_targets) =
            sina_server(|target| sina_fixture_body(target, &sina_fixture_all_rows()));
        let priced = price_holdings(
            &Config {
                sina_endpoint: fixture,
                ..cfg.clone()
            },
            &[holding("SH:600519", 2.0), holding("SZ:000001", 2.0)],
            &mut PassCache::new(),
        );
        assert!(priced.complete);
        assert!(
            priced.conversions.is_empty(),
            "two CNY rows settling in CNY convert nothing",
        );
        let mut asked: Vec<String> = fixture_targets.try_iter().collect();
        asked.sort();
        assert_eq!(
            asked,
            vec!["/list=sh600519", "/list=sz000001"],
            "the two prices and not one rate, off the server this half used",
        );
        assert_eq!(
            priced.total,
            PortfolioTotal::Priced {
                amount: 2657.28,
                currency: "CNY".into()
            },
        );
        let table = holdings_table(priced, "2026-09-07T12:00:00Z");
        assert!(
            table["caption"]
                .as_str()
                .expect("caption")
                .contains("totalled in CNY"),
            "{table}"
        );

        // The shape every existing install has: all crypto, settling in the default `USDT`; neither source reachable and it still totals.
        let priced = price_holdings(
            &Config {
                quote: "USDT".into(),
                ..cfg
            },
            &[holding("USDT", 3.0), holding("USDT", 4.0)],
            &mut PassCache::new(),
        );
        assert!(priced.complete && priced.rows.len() == 2);
        assert_eq!(
            priced.total,
            PortfolioTotal::Priced {
                amount: 7.0,
                currency: "USD".into()
            },
            "the same number it always had, now labelled in the unit the rates \
             are in",
        );
        assert_eq!(
            refused.try_iter().count(),
            0,
            "the crypto half reached no endpoint at all: `USDT` prices off \
             Binance's pinned leg with no request, and settling it is the \
             assumed parity rather than a lookup",
        );
    }

    /// A loopback endpoint that answers every request with one price and reports the target it saw, so the assertion is about the URL the shipping code builds.
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

    /// A venue prefix is a prefix only when the colon is there; every guessing rule has counterexamples among real names.
    #[test]
    fn a_name_without_a_colon_is_never_read_as_a_prefix() {
        for name in [
            "USNVDA",
            "HK1810",
            "CRYPTOBTC",
            "SH600519",
            "SZ000001",
            "CNW",
            "US",
            "HK",
            "SH",
            "SZ",
            "CRYPTO",
        ] {
            let parsed = id(name);
            assert_eq!(parsed.venue, Venue::Crypto, "`{name}`");
            assert_eq!(parsed.symbol, name);
            assert_eq!(parsed.canonical(), format!("CRYPTO:{name}"));
        }
    }

    #[test]
    fn a_qualified_name_names_a_venue_and_folds_to_one_canonical_form() {
        assert_eq!(id("us:nvda").canonical(), "US:NVDA");
        assert_eq!(id("HK:1810").canonical(), "HK:01810");
        assert_eq!(id("sh:600519").canonical(), "SH:600519");
        assert_eq!(id("SZ:000001").canonical(), "SZ:000001");
        assert_eq!(id(" CRYPTO:btc ").canonical(), "CRYPTO:BTC");
        // A prefix that names no venue is refused rather than swallowed into a bare name. `CN:600519` is deliberately not in this list.
        for bad in [
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

    /// A stock identity is never handed to the crypto provider, nor a crypto identity to the stock one; the two endpoints are separate servers, so which one a name reaches is observable.
    #[test]
    fn a_crypto_and_a_us_identity_reach_two_different_sources() {
        let (endpoint, targets) = sina_server(|target| {
            sina_fixture_body(
                target,
                &[
                    // A `gb_btc` row exists on purpose: `CRYPTO:BTC` routed here would come back priced.
                    ("gb_btc", "<NAME>,1.0,0,2026-09-07 00:00:00"),
                    ("gb_nvda", "<NAME>,230.3600,0.84,2026-09-05 09:46:13"),
                ],
            )
        });
        let cfg = sina_cfg(endpoint);
        // A crypto name goes to the dead Binance endpoint and fails rather than picking up the 1.0 in the stock source.
        assert!(
            matches!(quote_asset(&cfg, &id("BTC")), Quote::Failed(_)),
            "a crypto identity still goes to Binance"
        );
        assert_eq!(
            quote_asset(&cfg, &id("US:BTC")),
            Quote::Price(1.0, Currency::Usd)
        );
        assert_eq!(
            quote_asset(&cfg, &id("US:NVDA")),
            Quote::Price(230.36, Currency::Usd)
        );
        let asked: Vec<String> = std::iter::from_fn(|| targets.try_recv().ok()).collect();
        assert_eq!(asked, vec!["/list=gb_btc", "/list=gb_nvda"], "{asked:?}");
    }

    /// `W` is Wayfair on a US exchange and Wormhole in crypto; bare `W` and `CRYPTO:W` are one holding, `US:W` is a different one.
    #[test]
    fn bare_w_is_the_crypto_w_and_us_w_is_a_third_string_but_a_second_identity() {
        assert_eq!(id("W"), id("CRYPTO:W"), "the bare form is frozen as crypto");
        assert_eq!(id("W").canonical(), "CRYPTO:W");
        assert_ne!(id("US:W"), id("W"), "two venues, two assets");
        assert_eq!(id("US:W").canonical(), "US:W");
        // The registered gap: a holding mis-recorded as bare `W` and re-recorded as `US:W` leaves both rows standing.
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

    /// Read-side normalisation at the seam a `set` goes through: a legacy `BTC` row and an incoming `crypto:BTC` collapse to one holding rather than being summed.
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

    /// The venue reaches the reader at all three exits, as its own column rather than a prefix.
    #[test]
    fn every_exit_echoes_the_venue() {
        let (endpoint, _targets) = sina_server(|target| sina_fixture_body(target, &[]));
        let cfg = sina_cfg(endpoint);
        let priced = price_holdings(
            &cfg,
            &[holding("USDT", 3.0), holding("US:W", 1.0)],
            &mut PassCache::new(),
        );
        let rows = &priced.rows;
        assert_eq!(rows[0]["asset"], json!("USDT"));
        assert_eq!(rows[0]["venue"], json!("CRYPTO"));
        assert_eq!(rows[1]["asset"], json!("W"));
        assert_eq!(rows[1]["venue"], json!("US"));

        // Exit 2: `market.holdings.list`'s human-readable line has no columns and must name the venue too.
        let line = holdings_line(&priced);
        assert!(line.contains("CRYPTO:USDT"), "{line}");
        assert!(line.contains("US:W"), "{line}");

        // Exit 3: `market.quote` through the tool dispatcher. The quote asset prices without a network call, so the `Rpc` is never used.
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
        assert_eq!(
            quoted["structuredContent"]["currency"],
            json!("USDT"),
            "the unit of the number, from the source that produced it"
        );

        let table = holdings_table(priced, "2026-09-06T12:00:00Z");
        let columns: Vec<&str> = table["columns"]
            .as_array()
            .expect("columns")
            .iter()
            .filter_map(|c| c["key"].as_str())
            .collect();
        assert!(columns.contains(&"venue"), "{columns:?}");
        assert!(columns.contains(&"currency"), "{columns:?}");
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

    // The kernel fills `_meta["dev.neige/track"]`; the caller fills `arguments`. Reading the wrong one writes to whichever Track a poisoned instruction named.
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
        let empty = json!({ "params": { "_meta": { "dev.neige/track": { "id": "" } } } });
        assert_eq!(track_from_call(&empty), None);
    }

    #[test]
    fn pushed_payloads_are_valid_report_table_blocks() {
        // Whatever this plugin pushes is read back by a report `table` block, so it must satisfy the kernel's validator, not a second opinion written here.
        let cfg = cfg();
        let priced = price_holdings(&cfg, &[holding("USDT", 3.0)], &mut PassCache::new());
        assert_eq!(
            priced.total,
            PortfolioTotal::Priced {
                amount: 3.0,
                // `USD`, not `USDT`: the default `quote` settles in USD at the assumed parity.
                currency: "USD".into()
            }
        );
        assert!(priced.complete);
        let holdings = holdings_table(priced, "2026-09-06T12:00:00Z");
        assert_eq!(validate_payload(KIND_TABLE, &holdings), Ok(()));

        let points = vec![
            json!({ "at": "2026-09-06T12:00:00Z", "total": 100.0, "currency": "USDT" }),
            json!({ "at": "2026-09-06T12:00:30Z", "total": 110.0, "currency": "USDT" }),
        ];
        assert_eq!(
            validate_payload(KIND_TABLE, &history_table(&points)),
            Ok(())
        );
    }

    #[test]
    fn an_unpriceable_holding_keeps_its_row_and_leaves_the_total_alone() {
        // `ZZZZ` goes to the unreachable venue and fails; the priced half must still be priced, and the total must cover only it.
        let cfg = Config {
            binance_endpoint: "http://127.0.0.1:1".into(),
            ..cfg()
        };
        let priced = price_holdings(
            &cfg,
            &[holding("USDT", 3.0), holding("ZZZZ", 1.0)],
            &mut PassCache::new(),
        );
        assert!(!priced.complete);
        assert_eq!(
            priced.total,
            PortfolioTotal::Priced {
                amount: 3.0,
                currency: "USD".into()
            },
            "the total covers the priced rows only"
        );
        assert_eq!(
            priced.rows.len(),
            2,
            "the unpriceable holding keeps its row"
        );
        assert!(priced.rows[1]["price"].is_null() && priced.rows[1]["value"].is_null());
    }

    #[test]
    fn an_unsummable_portfolio_still_publishes_its_rows() {
        // Suppressing the whole table would leave the previous overlay on screen, read as current.
        let table = holdings_table(
            PricedPortfolio {
                rows: vec![json!({ "asset": "BTC", "qty": 1.0, "price": 2.0, "value": 2.0 })],
                total: PortfolioTotal::NotFinite,
                complete: true,
                // `Usd`, not `Usdt`: `Currency::settlement` never returns `Usdt`.
                settlement: Some(Currency::Usd),
                conversions: Vec::new(),
            },
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
        // Each row's value is finite while their sum is not; the per-row check alone would pass this into the history.
        let priced = price_holdings(
            &cfg(),
            &[holding("USDT", 1e308), holding("USDT", 1e308)],
            &mut PassCache::new(),
        );
        assert!(priced.complete, "both rows price fine on their own");
        assert_eq!(priced.rows.len(), 2);
        assert_eq!(
            priced.total,
            PortfolioTotal::NotFinite,
            "the sum is what `refresh` must refuse"
        );
        assert!(priced.total.value_cell().is_null());
        assert!(priced.total.stated().is_none());
    }

    #[test]
    fn rounding_never_manufactures_an_infinity() {
        // `1e308 * 100` overflows; `inf` here would serialize as `null` and read back as a fabricated zero.
        assert!(round_to(1e308, 2).is_finite());
        assert_eq!(round_to(1e308, 2), 1e308, "too large to scale ⇒ unrounded");
        assert_eq!(round_to(79_979.999_999, 2), 79_980.0);
        assert!(!round_to(f64::INFINITY, 2).is_finite());
    }

    #[test]
    fn history_rows_are_newest_first_with_the_change_against_the_previous_point() {
        let points = vec![
            json!({ "at": "t1", "total": 100.0, "currency": "USDT" }),
            json!({ "at": "t2", "total": 110.0, "currency": "USDT" }),
            json!({ "at": "t3", "total": 90.0, "currency": "USDT" }),
        ];
        let table = history_table(&points);
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

    /// The change column is blank wherever two adjacent points are not known to share a unit: the first point, a currency change (`USD` → `CNY` would draw a jump of about six times the portfolio), and a point that records no currency.
    #[test]
    fn the_change_column_breaks_wherever_two_points_are_in_different_units() {
        let points = vec![
            // Unit not recorded, not recoverable.
            json!({ "at": "t1", "total": 100.0 }),
            json!({ "at": "t2", "total": 110.0 }),
            json!({ "at": "t3", "total": 120.0, "currency": "USD" }),
            json!({ "at": "t4", "total": 130.0, "currency": "USD" }),
            json!({ "at": "t5", "total": 900.0, "currency": "CNY" }),
            json!({ "at": "t6", "total": 910.0, "currency": "CNY" }),
        ];
        let table = history_table(&points);
        let rows = table["rows"].as_array().expect("rows");
        let changes: Vec<&Value> = rows.iter().map(|row| &row["change"]).collect();
        assert_eq!(
            changes,
            vec![
                &json!(10.0), // t6 − t5, both CNY
                &Value::Null, // t5 against a USD point: not a move
                &json!(10.0), // t4 − t3, both USD
                &Value::Null, // t3 against a point with no unit
                &Value::Null, // t2 against another point with no unit
                &Value::Null, // t1 has nothing before it
            ],
            "{rows:?}",
        );
        let currencies: Vec<&Value> = rows.iter().map(|row| &row["currency"]).collect();
        assert_eq!(
            currencies,
            vec![
                &json!("CNY"),
                &json!("CNY"),
                &json!("USD"),
                &json!("USD"),
                &Value::Null,
                &Value::Null,
            ],
            "an unrecorded unit is stated as unrecorded, not as today's: {rows:?}",
        );
        let caption = table["caption"].as_str().expect("caption");
        assert!(
            caption.contains("2 points were recorded before this plugin stored a currency"),
            "{caption}"
        );
        assert_eq!(validate_payload(KIND_TABLE, &table), Ok(()));
    }

    #[test]
    fn configuration_is_all_optional_and_the_poll_interval_is_floored() {
        let init = |values: Value| json!({ "params": { "_meta": { "dev.neige/config": { "values": values } } } });
        let bare = config_from_initialize(&json!({}));
        assert_eq!(bare.quote, "USDT");
        assert_eq!(bare.poll, Duration::from_secs(30));
        assert_eq!(bare.binance_endpoint, "https://data-api.binance.vision");
        assert_eq!(bare.sina_endpoint, "https://hq.sinajs.cn");

        let clamped = config_from_initialize(&init(json!({ "poll_seconds": 1 })));
        assert_eq!(clamped.poll, Duration::from_secs(MIN_POLL_SECONDS));

        let custom = config_from_initialize(&init(json!({
            "quote": "usd",
            "binance_endpoint": "https://example.test/",
            "sina_endpoint": "https://sina.example.test/"
        })));
        assert_eq!(custom.quote, "USD");
        assert_eq!(
            custom.binance_endpoint, "https://example.test",
            "the trailing slash is dropped so URL building stays single-slash"
        );
        assert_eq!(custom.sina_endpoint, "https://sina.example.test");
    }

    #[test]
    fn timestamps_are_rfc3339_utc() {
        assert_eq!(civil_from_days(20_702), (2026, 9, 6));
        let now = now_rfc3339();
        assert_eq!(now.len(), 20, "{now}");
        assert!(now.ends_with('Z'), "{now}");
    }

    /// Every tool must be callable by a Planner: codex's `requires_mcp_tool_approval` skips approval only for a read-only tool or one declaring both `destructiveHint: false` and `openWorldHint: false`, and the kernel spawns agents with `approval_policy: "never"`. The write tool therefore records state and wakes the poll thread rather than touching the network.
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
        // The two prefixes must not be prefixes of each other, or `portfolios`'s prefix scan would try to price history documents.
        assert!(!HOLDINGS_PREFIX.starts_with(HISTORY_PREFIX));
        assert!(!HISTORY_PREFIX.starts_with(HOLDINGS_PREFIX));
        assert_eq!(holdings_key("t1"), "holdings/t1");
        assert_eq!(history_key("t1"), "history/t1");
    }
}
