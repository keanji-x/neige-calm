//! Market portfolio plugin — the kernel's first *pushing* plugin.
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
//! **Endpoints.** Two sources answer here, one per family of venues.
//!
//! *Binance* (the `CRYPTO` venue). The default is `data-api.binance.vision`,
//! not `api.binance.com`. The latter answers `HTTP 451`-style
//! `"Service unavailable from a restricted location"` from many hosts
//! (verified from this project's own deploy host), while the former serves the
//! identical `/api/v3` market-data paths with no key and no geo gate.
//!
//! *Sina* (`hq.sinajs.cn`, the `US`/`HK`/`SH`/`SZ` venues). Its `/list=` path
//! serves nothing without a `Referer` header and answers in GBK; see
//! [`sina_quote`].
//!
//! Both are configurable; nothing here assumes either.
//!
//! **History is forward-only.** The series starts empty and grows one point
//! per successful refresh; the plugin never back-fills from klines. That is
//! the deliberate scope of the first slice — "how has my total moved since I
//! started watching", not "what was it last year".

use std::collections::HashMap;
use std::io::{BufRead, BufReader, BufWriter, Read, Stdout, Write};
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
///
/// Mainland China is TWO venues, `SH` and `SZ`, not one `CN`. A single `CN`
/// venue could not say which exchange lists a code, so it had to ask both and
/// take whichever answered — and that is unsound whenever only one of them
/// answers with a price for a code both list. `CN:000001` is the Shanghai
/// Composite index at 3933 on `sh` and Ping An Bank at 11.87 on `sz`; the day
/// one of the two is halted and returns the source's `0.0000` row, the
/// positive-price filter leaves exactly one answer and the wrong security is
/// accepted in silence. The caller names the exchange instead.
///
/// [`Venue::Cn`] is still in this list, and it is NOT a sixth place to trade.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum Venue {
    Crypto,
    Us,
    Hk,
    /// Shanghai Stock Exchange.
    Sh,
    /// Shenzhen Stock Exchange.
    Sz,
    /// **A migration path, not a place.** `CN:` was the mainland venue in the
    /// slice that shipped before the Shanghai/Shenzhen split, so a KV document
    /// written by that version can hold `CN:600519` today. It is never priced:
    /// [`quote_asset`] answers it [`Quote::Failed`] with a message naming the
    /// two prefixes and asking the caller which exchange lists the code, and
    /// no request goes out. Only one of the two prefixes works for any given
    /// code — the mainland ranges are split between the exchanges, so
    /// `SZ:600519` is refused as surely as `CN:600519` is — and the message
    /// says so rather than presenting them as interchangeable.
    ///
    /// It stays in the grammar because of what removing it would do to those
    /// stored rows, which is worse than an unpriceable holding. `CN:600519`
    /// would stop parsing; [`holdings_from_value`] drops a row it cannot parse
    /// WITHOUT saying so; and the next `market.holdings.set` writes the whole
    /// array back, so the dropped row is gone from the store for good. The
    /// user is never told, and there is nothing left to tell them from. One
    /// holding that visibly cannot be priced replaces one that silently
    /// disappears.
    Cn,
}

impl Venue {
    /// The prefix a caller writes, and the one a canonical identity carries.
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
    venue-qualified \"<VENUE>:<SYMBOL>\" over the venues CRYPTO, US, HK, SH \
    (Shanghai) and SZ (Shenzhen) — for example \"CRYPTO:BTC\", \"US:NVDA\", \
    \"HK:1810\", \"SH:600519\", \"SZ:000001\". A name with no venue is a \
    crypto asset. Each of those five venues has its own price source, quoting \
    in its own currency, with nothing converted between them. \"CN:\" also \
    parses, but only so that a holding stored under the retired mainland \
    venue can still be read back — it is never priced, and has to be recorded \
    again under the exchange that lists the code: \"SH:<code>\" for a Shanghai \
    listing, \"SZ:<code>\" for a Shenzhen one.";

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
/// - `CN:` is a KNOWN prefix here, so `CN:600519` parses, round-trips and can
///   be read back out of the store. It is refused at PRICING time instead —
///   see [`Venue::Cn`] for why the refusal is placed there and not here.
/// - The accepted symbol is then folded to one spelling per venue by
///   [`canonical_symbol`]. Upper-casing is not enough on its own: Hong Kong
///   codes are written both padded and unpadded, and two spellings of one
///   security are two identities everywhere downstream.
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

/// Fold the spellings of ONE security at a venue onto one symbol.
///
/// Only Hong Kong needs this today, and it needs it because leading zeros are
/// optional in every human-facing spelling of a Hong Kong code while the
/// security is the same one. `HK:1810` and `HK:01810` are both Xiaomi, both
/// resolve to `hk01810` at the source and both come back 27.48 HKD — but as
/// two `AssetId`s they are two identities, and every comparison downstream is
/// on identity: `holdings.retain` in `market.holdings.set` would not match one
/// against the other, so a user who recorded a position one way and later
/// re-recorded it the other would end up holding the SAME shares twice, in one
/// currency, in a total that looks entirely ordinary. That is the same defect
/// read-side normalisation already closes for bare crypto names (see
/// [`Holding::from_json`]), one venue over.
///
/// **The canonical form is five digits, zero-padded.** Both directions fold
/// equally well; this one is chosen because it is the spelling the exchange
/// and the source both use — HKEX publishes five-digit securities codes, and
/// `hq.sinajs.cn` keys on `hk01810` — which keeps [`AssetId::symbol`]'s
/// contract (the symbol as the venue spells it) true rather than inventing a
/// third form that only this plugin uses.
///
/// Leading zeros are stripped BEFORE padding, so the fold reaches spellings
/// longer than five characters too: `HK:089988` and `HK:89988` are one
/// identity, and get one answer, where previously the first was over five
/// characters and unspellable while the second was a currency refusal. What is
/// left longer than five digits after stripping — `HK:123456` — is no Hong
/// Kong code at all; it is returned unchanged and [`sina_target`] refuses it.
///
/// Non-digit Hong Kong symbols (`HK:TENCENT`) and every other venue are
/// returned unchanged. Mainland codes are six digits at both exchanges and
/// this plugin asks for them verbatim (`sh600519`), with no padding anywhere,
/// so no two spellings of one mainland code exist to fold; US symbols and
/// crypto names are already folded by the upper-casing in [`parse_asset`].
///
/// A row already in the KV under a legacy spelling is rewritten to the
/// canonical one by the write-triggered migration described on
/// [`Holding::from_json`] — the read path normalises, and the next
/// `market.holdings.set` overwrites the whole array. There is no separate
/// migration.
fn canonical_symbol(venue: Venue, symbol: &str) -> String {
    match venue {
        Venue::Hk if symbol.chars().all(|c| c.is_ascii_digit()) => {
            let significant = symbol.trim_start_matches('0');
            // All zeros is not a code, but it must still fold to one string
            // rather than to the empty one.
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
    /// The SETTLEMENT currency: the one a portfolio's total is meant to be
    /// stated in.
    ///
    /// It is not a pricing input. Each source quotes in its own currency
    /// (Binance in `USDT`, Sina in USD/HKD/CNY) and that currency travels with
    /// the number; nothing in this slice converts between them, because this
    /// slice has no exchange rates. So a total is stated only when the priced
    /// holdings already share one currency, and this value is what a later
    /// slice will convert INTO. It used to be three things at once — display
    /// unit, Binance's quote leg, and "this asset is the unit, worth 1" — and
    /// the last two have moved to [`binance_spot`], where they are facts about
    /// that exchange rather than about the operator's preference.
    quote: String,
    poll: Duration,
    binance_endpoint: String,
    sina_endpoint: String,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            quote: "USDT".into(),
            poll: Duration::from_secs(30),
            binance_endpoint: "https://data-api.binance.vision".into(),
            sina_endpoint: "https://hq.sinajs.cn".into(),
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
    if let Some(endpoint) = values.get("sina_endpoint").and_then(Value::as_str)
        && !endpoint.trim().is_empty()
    {
        cfg.sina_endpoint = endpoint.trim().trim_end_matches('/').to_string();
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
// the caller's — an identity that is already qualified with its venue does not
// have to be renamed the day its venue gains a source: a name that used to
// resolve nowhere starts resolving.
//
// What the caller DOES say is the venue, because a name alone does not
// identify a security (see [`parse_asset`]). Still no per-provider priority
// list and no configuration surface for routing: one identity has one venue,
// and the venue is written down rather than guessed.
//
// Two sources now, one venue each way:
//
// | venue | source | symbol |
// | --- | --- | --- |
// | `CRYPTO` | Binance spot | `<SYMBOL>USDT` |
// | `US` | Sina `hq.sinajs.cn` | `gb_<symbol>` |
// | `HK` | Sina `hq.sinajs.cn` | `hk<symbol>`, five digits by [`canonical_symbol`] |
// | `SH` | Sina `hq.sinajs.cn` | `sh<symbol>` |
// | `SZ` | Sina `hq.sinajs.cn` | `sz<symbol>` |
//
// An identity is never handed to the other venue's source. Routing `US:BTC` to
// Binance would come back with bitcoin's price attached to a US-listing
// identity, and a fabricated number in a total is the failure this whole
// identity layer exists to prevent.
//
// **The prices are in different currencies, and nothing here converts them.**
// A [`Quote::Price`] carries the currency this plugin determined the number is
// in, and that currency travels with the number to every exit. Summing across
// two of them is the one thing this slice must not do; see [`PortfolioTotal`].
//
// **Neither source states a currency.** Binance's `/ticker/price` answers a
// bare number for a pair whose quote leg this plugin pinned itself (`USDT`),
// and Sina's `/list=` rows carry no unit at all. So the currency of a stock
// price is DECIDED HERE, from the venue and the code range — and where that
// decision cannot be made the quote is refused rather than labelled with a
// guess. See [`sina_target`] for which ranges are priced and why.

/// What a provider answered, or why it could not.
#[derive(Clone, Debug, PartialEq)]
enum Quote {
    /// A positive, finite price, and the currency the number is in — never the
    /// configured settlement currency. The two are the same only by
    /// coincidence, and labelling an HKD price `USDT` because that is what the
    /// operator configured is the defect this payload exists to prevent.
    ///
    /// Neither source states its currency, so this string is this plugin's own
    /// determination — Binance's pinned quote leg for crypto, the venue and
    /// code range for a stock. A code whose currency cannot be determined that
    /// way is [`Quote::Failed`], not a `Price` with a guessed unit.
    Price(f64, &'static str),
    /// No valid quote came back, and nothing went wrong to explain it. That
    /// covers a name the source does not list, a name this plugin has no way
    /// to spell for the source, and a listed name answering the row of zeros
    /// this source serves for a halted or delisted security. Distinct from a
    /// failure: the lookup worked, so a caller acts on the name rather than on
    /// the transport.
    Unknown,
    /// The lookup itself failed — network, malformed response, a venue error.
    Failed(String),
}

/// Price one asset: route the identity to the one source that serves its
/// venue, and answer whatever that source answered.
///
/// There is no pre-source shortcut here any more. The `1.0` a settlement asset
/// used to get was decided HERE, from `cfg.quote`, which made the identity of
/// an asset depend on how the install was configured — `USDT` was "the
/// settlement asset, worth 1" under the default and an ordinary coin under
/// `quote = "CNY"`. That fact belongs to Binance's quote leg, not to the
/// configuration, and it now lives in [`binance_spot`].
fn quote_asset(cfg: &Config, asset: &AssetId) -> Quote {
    match asset.venue {
        Venue::Crypto => binance_spot(cfg, asset),
        Venue::Us | Venue::Hk | Venue::Sh | Venue::Sz => sina_quote(cfg, asset),
        // `CN` names no exchange, so there is no source to route it to and
        // nothing to ask. The answer is a visible `Failed` carrying the two
        // spellings that would work — see [`Venue::Cn`] for what this refusal
        // replaced.
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

/// The one test for "is this string a price", called by BOTH sources.
///
/// Finite is not enough. `0` and negative parse fine and would price a holding
/// at or below nothing, which no market means — and Sina's way of saying it
/// has no live price for a symbol is a row of `0.0000`, observed both for a
/// halted listing (`gb_ena`) and for a ticker that does not exist
/// (`gb_zzzzz`). A second copy of this condition written next to the second
/// source is how the two would come to disagree.
fn positive_price(raw: &str) -> Option<f64> {
    match raw.trim().parse::<f64>() {
        Ok(price) if price.is_finite() && price > 0.0 => Some(price),
        _ => None,
    }
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

/// The quote leg every Binance spot lookup is built with.
///
/// **Pinned, and no longer taken from `cfg.quote`.** A Binance spot symbol is
/// `<BASE><QUOTE>` over the legs Binance itself lists, which is a fact about
/// that exchange and not a preference an operator holds. Wiring the settlement
/// currency into it meant an install settling in `CNY` asked for `BTCCNY` — a
/// pair that does not exist, verified: the endpoint answers
/// `{"code":-1121,"msg":"Invalid symbol."}`, so every crypto row went
/// unpriced, `complete` never held, and the Track's whole history series stood
/// still.
const BINANCE_QUOTE_LEG: &str = "USDT";

/// The spot symbol Binance is asked about.
///
/// Built from the VENUE-LOCAL symbol, never from the canonical identity: a
/// Binance spot symbol is `<ASSET><QUOTE>`, and `CRYPTO:BTCUSDT` is not one.
fn binance_symbol(asset: &AssetId) -> String {
    format!("{}{BINANCE_QUOTE_LEG}", asset.symbol)
}

/// Binance spot, via `/api/v3/ticker/price`.
///
/// The endpoint answers `200` with a `{"code":…,"msg":…}` body for an unknown
/// symbol *and* for a geo-blocked caller, so the status code says nothing and
/// the absence of `price` is the real check.
fn binance_spot(cfg: &Config, asset: &AssetId) -> Quote {
    // The quote leg priced in itself. This is the whole of what used to be
    // `quote_asset_shortcut`: it is Binance's fact about Binance's own leg,
    // and it needs neither a request nor a configuration value. It answers
    // `USDT` as the currency, so the 1.0 carries the same unit as every other
    // number this source returns.
    if asset.symbol == BINANCE_QUOTE_LEG {
        return Quote::Price(1.0, BINANCE_QUOTE_LEG);
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
    match positive_price(price) {
        Some(price) => Quote::Price(price, BINANCE_QUOTE_LEG),
        None => Quote::Failed(format!(
            "{symbol}: `{price}` is not a positive finite price"
        )),
    }
}

// ---------------------------------------------------------------------------
// Sina — the US, HK, SH and SZ source
// ---------------------------------------------------------------------------

/// `hq.sinajs.cn` serves nothing without this header: the response to a
/// request that omits it is `HTTP 403` with the body `Forbidden` — not an
/// empty list, not JSON. Verified from this host.
const SINA_REFERER: &str = "https://finance.sina.com.cn";

/// A ceiling on how much of a Sina response is read. One row is a few hundred
/// bytes and exactly one is asked for, so anything past this is a wedged or
/// hostile endpoint. What the ceiling buys is bounded memory for one response;
/// it says nothing about the rest of the process.
const SINA_MAX_BODY_BYTES: u64 = 64 * 1024;

/// Which field of a Sina row is the last traded price, PER MARKET. The field
/// orders differ, and reading `gb_`'s index out of an `hk` row would publish
/// that stock's previous close — or its English name — as its price.
///
/// Each index below was read off a live response (2026-09-07):
///
/// ```text
/// gb_nvda  = "英伟达,230.3600,0.84,2026-09-05 09:46:13,…"
///             0        1 ← last
/// hk01810  = "XIAOMI-W,小米集团－Ｗ,28.220,28.440,28.400,27.120,27.480,-0.960,…"
///             0        1            2      3      4      5      6 ← last
/// sh600519 = "贵州茅台,1324.000,1330.000,1316.940,1333.600,1312.660,…"
///             0        1        2        3 ← last
/// ```
///
/// Cross-checked against the change column each row carries: `hk01810`'s
/// `-0.960` is `27.480 - 28.440`, so field 6 is the last price and field 3 is
/// the previous close, not the other way round.
const SINA_LAST_PRICE_FIELD_US: usize = 1;
const SINA_LAST_PRICE_FIELD_HK: usize = 6;
/// Shanghai and Shenzhen share one row layout.
const SINA_LAST_PRICE_FIELD_SH_SZ: usize = 3;

/// How one identity is asked for: which `list=` symbol, which field of the
/// answer is the price, and what currency this plugin has determined that
/// price to be in.
struct SinaLookup {
    /// The single `list=` symbol to ask about.
    symbol: String,
    price_field: usize,
    /// **Not read off the wire.** The source states no unit anywhere in its
    /// response; this is the plugin's own determination from the venue and the
    /// code range, and [`sina_target`] refuses every code it cannot make that
    /// determination for.
    currency: &'static str,
}

/// What this source can do with one identity.
enum SinaTarget {
    /// It can be asked for, and its quote currency is determined.
    Ask(SinaLookup),
    /// This source has no way to spell the identity at all — `HK:TENCENT` is a
    /// legal identity and not a Hong Kong stock code. No request goes out and
    /// the answer is [`Quote::Unknown`]: nothing was asked, so nothing is
    /// known.
    Unspellable,
    /// The identity is spellable, but which currency the exchange quotes that
    /// code in is not determined by the code. Refused out loud rather than
    /// priced under a guessed unit; the string says why.
    UndeterminedCurrency(String),
}

/// Build the request for one identity, and decide what currency its price
/// would be in.
///
/// **The source does not say.** A `hq.sinajs.cn` row is a comma-separated list
/// of numbers with no unit on any of them, so every currency this plugin
/// publishes for a stock is decided right here. Venue alone is not enough to
/// decide it — all three of these are counterexamples, read off the live
/// endpoint on 2026-09-07:
///
/// | code | what it is | quoted in |
/// | --- | --- | --- |
/// | `sh900932` | 陆家Ｂ股, a Shanghai B share | **USD**, not CNY |
/// | `sz200725` | 京东方Ｂ, a Shenzhen B share | **HKD**, not CNY |
/// | `hk89988` | 阿里巴巴－ＷＲ, a renminbi counter | **CNY**, not HKD |
///
/// None of the three is caught by asking a second exchange: each is listed
/// once, answers once, and would be published as a number in the wrong
/// currency — and then summed into a total in that wrong currency, which is
/// precisely the defect [`PortfolioTotal`] exists to prevent, occurring
/// *inside* one venue where no cross-currency check can see it.
///
/// So the ranges below are an ALLOWLIST: a code is priced only where the range
/// itself fixes the currency. Everything else is refused, in one of two ways,
/// and both are visible rather than guessed. A symbol this source could ask
/// about, whose range fixes no currency, is
/// [`SinaTarget::UndeterminedCurrency`]; a symbol that is not a code this
/// source could ask about at all — `HK:TENCENT` — is
/// [`SinaTarget::Unspellable`], and no currency question is reached. What the
/// allowlist excludes is a registered gap: B shares, Hong Kong's renminbi and
/// US-dollar counters, and every mainland range outside the A-share, ChiNext
/// and fund ranges spelled out below.
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
        // Crypto never reaches this source; `quote_asset` routes it to
        // Binance. Spelled out rather than left to a catch-all so that adding
        // a venue is a compile error here.
        Venue::Crypto => SinaTarget::Unspellable,
        // `CN` never reaches this source either; `quote_asset` answers it
        // `Failed` before any routing. Spelled out for the same reason as
        // `Crypto`: adding a venue must be a compile error here.
        Venue::Cn => SinaTarget::Unspellable,
        // Sina's `gb_` list is US-listed securities, and a US listing is
        // quoted in US dollars.
        Venue::Us => SinaTarget::Ask(SinaLookup {
            symbol: format!("gb_{}", asset.symbol.to_ascii_lowercase()),
            price_field: SINA_LAST_PRICE_FIELD_US,
            currency: "USD",
        }),
        // A Hong Kong identity arrives here ALREADY five digits when it is a
        // code at all: [`canonical_symbol`] folded `HK:1810`, `HK:01810` and
        // `HK:001810` onto `01810` at parse time, which is also the spelling
        // this list keys on. No padding happens here — a second copy of that
        // rule is what would let the two layers disagree. What is left to ask
        // is only whether the symbol IS a five-digit code: `HK:TENCENT` and
        // `HK:123456` are legal identities and no Hong Kong stock code, and
        // they get no request rather than a guess.
        //
        // 8xxxx is refused. `hk89988` is Alibaba's renminbi counter — live at
        // 94.45 CNY while `hk09988` trades at 111.00 HKD, both verified on
        // 2026-09-07 — and the row gives no way to tell which currency it is
        // in. What is established is that one range member is not HKD, so the
        // range does not fix a currency.
        //
        // 9xxxx is refused for a different and weaker reason, and the message
        // below says so rather than borrowing 8xxxx's: no currency has been
        // established for it either way. What refusing it costs was sampled,
        // not reasoned about — `hk90988` and `hk96618` both answer with an
        // EMPTY row on 2026-09-07, i.e. this source lists neither, so on those
        // two codes the refusal turns an `Unknown` into a `Failed` and gives
        // up no price. Nothing is known about the rest of the range, and
        // nothing here is a claim about how HKEX assigns codes. Note what is
        // NOT in this range: `HK:9988` folds to `09988`, is below 80000, and
        // is priced normally.
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
                currency: "HKD",
            })
        }
        // Shanghai: `6xxxxx` is the A-share main board and the STAR market,
        // and `5xxxxx` is the exchange-traded fund range; both are renminbi.
        // `9xxxxx` is the B-share board, quoted in US DOLLARS (`sh900932`,
        // 陆家Ｂ股, 0.385 USD), and stays refused. Bond and index ranges are
        // still not priced.
        //
        // What fixes renminbi for the fund range is the exchange's own rule,
        // not a sample: 《上海证券交易所交易规则》3.3.11 states that the tick
        // size for a fund order is denominated in renminbi, so a fund traded
        // on this exchange is quoted in renminbi whatever it holds. That
        // covers the cases a sample would raise: `sh513500` (标普500ETF博时)
        // 2.692, `sh501018` (南方原油LOF) 1.922 and `sh588000` (科创50ETF)
        // 1.705 are QDII, commodity and STAR funds and all quote in renminbi
        // on the live endpoint on 2026-09-07.
        //
        // That the source carries these codes at all was read off the same
        // endpoint that day: `sh510300` (沪深300ETF华泰柏瑞) 4.635,
        // `sh563210` (专精特新ETF富国) 1.949, and `sh511990` (华宝添益)
        // 99.999 — a money-market fund, whose ~100 quote is its unit price and
        // not a stray scale.
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
                currency: "CNY",
            })
        }
        // Shenzhen: `00xxxx` main board, `30xxxx` ChiNext and the `15xxxx` /
        // `16xxxx` fund ranges, all renminbi. `2xxxxx` is the B-share board,
        // quoted in HONG KONG DOLLARS (`sz200725`, 京东方Ｂ, 4.770 HKD,
        // against `sz000725`'s 5.680 CNY), and stays refused.
        //
        // Same basis as Shanghai's fund ranges: 《深圳证券交易所交易规则》
        // 3.3.11 likewise denominates a fund order's tick size in renminbi, so
        // the ranges are renminbi by the exchange's rule rather than by
        // sampling. `sz159915` (创业板ETF) answered 3.338 and `sz160216`
        // (国泰商品) 0.652 on the live endpoint on 2026-09-07, the latter a
        // commodity LOF — the shape a counterexample would have had — quoting
        // in renminbi like the rest. Being in range is not a promise the
        // source has the code:
        // `sz162201` (宏利成长混合, a LOF) answers with an empty row, which
        // comes back `Unknown` — the source does not list it — rather than as
        // a currency refusal, and that is the honest distinction between the
        // two answers.
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
                currency: "CNY",
            })
        }
    }
}

/// The payload of one `var hq_str_<symbol>="…";` row, or `None` when the
/// response carried no row for that symbol at all.
///
/// An empty payload is `Some("")`, and that distinction is the whole point:
/// Sina answers a symbol it does not list with `var hq_str_gb_doge="";`, which
/// is `Unknown`, while a response missing the row entirely is a malformed
/// answer to the question that was asked.
fn sina_payload<'a>(body: &'a str, symbol: &str) -> Option<&'a str> {
    // The `=` and the opening quote are part of the needle, so `hk00001`
    // cannot match inside a row for some longer symbol.
    let head = format!("var hq_str_{symbol}=\"");
    let rest = &body[body.find(&head)? + head.len()..];
    Some(&rest[..rest.find('"')?])
}

/// Sina's quote list, `https://hq.sinajs.cn/list=<symbol>`.
///
/// **One symbol per request.** The endpoint takes any number of
/// comma-separated symbols and answers one row each, and this function uses
/// none of that: an identity names its exchange, so there is exactly one
/// symbol to ask about. Batching across the holdings of a pass would mean
/// draining the per-pass [`PriceCache`] into a two-phase "collect the misses,
/// fetch, fill" pass over every Track, and the cache already collapses the
/// repeat this plugin actually makes (the same asset held by several Tracks).
/// One request per distinct asset per pass is the cost, and it is bounded by
/// how many assets are held, not by how many Tracks hold them.
///
/// **The response is GBK**, and it is read with `from_utf8_lossy` rather than
/// transcoded. Every byte this function looks at is ASCII — the digits of a
/// price, `"` and `,` — and `from_utf8_lossy` preserves the ASCII content and
/// the order of the delimiters, replacing each invalid sequence (the Chinese
/// names, which nothing here reads) with U+FFFD. Byte OFFSETS do move, since
/// the replacement is three bytes wide; nothing here indexes by offset. `"`
/// (0x22) and `,` (0x2C) are also outside GBK's trailing-byte range
/// (0x40–0xFE), so no name can smuggle a delimiter into the split.
fn sina_quote(cfg: &Config, asset: &AssetId) -> Quote {
    let lookup = match sina_target(asset) {
        SinaTarget::Ask(lookup) => lookup,
        SinaTarget::Unspellable => return Quote::Unknown,
        SinaTarget::UndeterminedCurrency(why) => return Quote::Failed(why),
    };
    let url = format!("{}/list={}", cfg.sina_endpoint, lookup.symbol);
    let response = match ureq::get(&url)
        .set("Referer", SINA_REFERER)
        .timeout(Duration::from_secs(10))
        .call()
    {
        Ok(response) => response,
        // Unlike Binance, a non-200 here carries no price shape at all — the
        // 403 the missing `Referer` earns has the literal body `Forbidden` —
        // so the status is the answer and the body is not parsed.
        Err(ureq::Error::Status(code, _)) => {
            return Quote::Failed(format!("GET {url}: HTTP {code}"));
        }
        Err(e) => return Quote::Failed(format!("GET {url}: {e}")),
    };
    let mut bytes = Vec::new();
    if let Err(e) = response
        .into_reader()
        .take(SINA_MAX_BODY_BYTES)
        .read_to_end(&mut bytes)
    {
        return Quote::Failed(format!("reading {url}: {e}"));
    }
    let body = String::from_utf8_lossy(&bytes);

    let symbol = &lookup.symbol;
    let Some(payload) = sina_payload(&body, symbol) else {
        return Quote::Failed(format!("{url}: the response carried no `{symbol}` row"));
    };
    if payload.is_empty() {
        // "We do not list this." Not an error, and not a price.
        return Quote::Unknown;
    }
    let fields: Vec<&str> = payload.split(',').collect();
    let Some(raw) = fields.get(lookup.price_field) else {
        return Quote::Failed(format!(
            "{symbol}: the row has {} fields, so it has no field {}",
            fields.len(),
            lookup.price_field,
        ));
    };
    // A row of zeros is how this source spells a halted or unlisted symbol, so
    // a non-price here is `Unknown`-shaped, not a failure.
    match positive_price(raw) {
        Some(price) => Quote::Price(price, lookup.currency),
        None => Quote::Unknown,
    }
}

/// What a set of priced rows sums to — or why it does not sum to anything.
///
/// The variant that matters is [`PortfolioTotal::AcrossCurrencies`]. Once
/// holdings can be quoted in USD, HKD, CNY and USDT at once, `total += value`
/// over all of them produces a figure in no currency at all: 100 USD plus 100
/// HKD is not 200 of anything. This plugin has no exchange rates yet, so the
/// honest answer is no total — the same answer it already gives when the
/// values do not sum to a finite number — and the per-asset rows, each with
/// its own currency, carry everything a reader can actually use.
#[derive(Clone, Debug, PartialEq)]
enum PortfolioTotal {
    /// Every priced row was in `currency`, and they sum to a finite `amount`.
    Priced { amount: f64, currency: String },
    /// The priced rows are quoted in more than one currency, listed here in
    /// the order they were first seen.
    AcrossCurrencies(Vec<String>),
    /// One currency, but the values do not sum to a finite number.
    NotFinite,
    /// Nothing was priced at all: the total of nothing, in no currency.
    Nothing,
}

impl PortfolioTotal {
    /// The number and unit a caller may state. `None` wherever no number is
    /// honest — which is every variant but one, on purpose.
    fn stated(&self) -> Option<(f64, &str)> {
        match self {
            Self::Priced { amount, currency } => Some((*amount, currency.as_str())),
            _ => None,
        }
    }

    /// Why there is no total, for a caller that got `null`. `None` when there
    /// is one.
    fn no_total_reason(&self) -> Option<String> {
        match self {
            Self::Priced { .. } => None,
            Self::AcrossCurrencies(currencies) => Some(format!(
                "these holdings are quoted in {} and this plugin has no exchange rates yet",
                currencies.join(" and "),
            )),
            Self::NotFinite => Some("the values do not sum to a finite number".into()),
            Self::Nothing => Some("nothing could be priced".into()),
        }
    }

    /// The `value` cell of the `Total` row. `null` wherever a number would be
    /// a claim this plugin cannot make.
    ///
    /// [`Self::Nothing`] is the exception, and it is not a clean one. It
    /// covers an EMPTY portfolio, whose total really is zero, and equally a
    /// non-empty portfolio not one row of which could be priced, whose total
    /// is unknown — the two are one variant because `currencies` is empty
    /// either way. The cell says `0.0` for both, while
    /// [`Self::no_total_reason`] says "nothing could be priced" for both, so a
    /// non-empty unpriced portfolio publishes a `0.0` under a caption denying
    /// there is a total. That predates this slice; what is new is that the
    /// caption is now printed next to the number. Splitting the variant is
    /// left undone rather than papered over.
    fn value_cell(&self) -> Value {
        match self {
            Self::Priced { amount, .. } => json!(round_to(*amount, 2)),
            Self::Nothing => json!(0.0),
            Self::AcrossCurrencies(_) | Self::NotFinite => Value::Null,
        }
    }

    fn currency_cell(&self) -> Value {
        match self {
            Self::Priced { currency, .. } => json!(currency),
            _ => Value::Null,
        }
    }
}

/// Price every holding of one portfolio.
///
/// Returns the table rows, what they total to, and whether every holding
/// priced. An asset that could not be priced keeps its row with `null` price
/// and value — dropping it would understate the portfolio silently, which is
/// exactly what a null says out loud — and is left out of the total.
///
/// Each row carries the CURRENCY its price is in, straight from the source
/// that answered. Nothing here re-labels a number with the configured
/// settlement currency: that is how an HKD price ends up captioned `USDT`.
fn price_holdings(
    cfg: &Config,
    holdings: &[Holding],
    cache: &mut PriceCache,
) -> (Vec<Value>, PortfolioTotal, bool) {
    let mut rows = Vec::with_capacity(holdings.len());
    let mut sum = 0.0;
    // Distinct currencies among the PRICED rows, first-seen order. One entry
    // means the sum is a number in that currency; more than one means it is
    // not a number in any.
    let mut currencies: Vec<&'static str> = Vec::new();
    let mut complete = true;
    for holding in holdings {
        // `price * quantity` can overflow to infinity even when both factors
        // are finite, so the product is checked as well as the input.
        let priced = match quote_cached(cfg, &holding.asset, cache) {
            Quote::Price(price, currency) => {
                let value = price * holding.quantity;
                if value.is_finite() {
                    Ok((price, currency, value))
                } else {
                    Err(format!(
                        "{} × {} is not a finite value",
                        holding.asset.canonical(),
                        holding.quantity
                    ))
                }
            }
            Quote::Unknown => Err(format!(
                "no valid quote came back for {} this pass",
                holding.asset.canonical()
            )),
            Quote::Failed(why) => Err(why),
        };
        match priced {
            Ok((price, currency, value)) => {
                sum += value;
                if !currencies.contains(&currency) {
                    currencies.push(currency);
                }
                rows.push(json!({
                    "asset": holding.asset.symbol,
                    "venue": holding.asset.venue.prefix(),
                    "qty": round_to(holding.quantity, 8),
                    "price": round_to(price, 2),
                    "value": round_to(value, 2),
                    "currency": currency,
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
                    "currency": Value::Null,
                }));
            }
        }
    }
    let total = match currencies.as_slice() {
        [] => PortfolioTotal::Nothing,
        [only] if sum.is_finite() => PortfolioTotal::Priced {
            amount: sum,
            currency: (*only).to_string(),
        },
        [_] => PortfolioTotal::NotFinite,
        many => PortfolioTotal::AcrossCurrencies(many.iter().map(|c| (*c).to_string()).collect()),
    };
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

/// The holdings table.
///
/// It takes no [`Config`]: there is no longer any unit here that comes from
/// the configuration. Every number on this table is in the currency the source
/// that produced it quoted, and that currency is a column and a caption, so a
/// portfolio holding `US:NVDA` and `HK:1810` reads as USD and HKD rather than
/// as two numbers under one wrong heading.
///
/// The table still goes out when there is no total: it is the only place the
/// per-asset rows appear, and withholding it would leave whatever was
/// published last on screen, presented as current.
fn holdings_table(rows: Vec<Value>, total: &PortfolioTotal, complete: bool, at: &str) -> Value {
    let mut rows = rows;
    rows.push(json!({
        "asset": "Total",
        "venue": Value::Null,
        "qty": Value::Null,
        "price": Value::Null,
        "value": total.value_cell(),
        "currency": total.currency_cell(),
    }));
    let caption = match (total.stated(), complete) {
        (None, _) => format!(
            "Priced at {at} — no total is shown: {}",
            total
                .no_total_reason()
                .unwrap_or_else(|| "no reason recorded".into()),
        ),
        (Some((_, currency)), true) => format!("Priced at {at}, totalled in {currency}"),
        (Some((_, currency)), false) => format!(
            "Priced at {at}, totalled in {currency} — some prices unavailable; the total covers \
             the priced rows only"
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
            // No unit in these two headings. The rows are in different
            // currencies, so any single unit written here would be wrong for
            // some of them; the `currency` column carries each row's own.
            { "key": "price", "label": "Price", "align": "right" },
            { "key": "value", "label": "Value", "align": "right" },
            { "key": "currency", "label": "Currency" },
        ],
        "rows": rows,
        "caption": caption,
        "highlight": "Total",
    })
}

/// History as a table, newest first, with the change against the previous
/// point. `points` is `[{ "at": <rfc3339>, "total": <number> }]` in
/// chronological order.
///
/// KNOWN GAP — the column heading is a bare `Total`, with no currency.
/// A stored point is `{at, total}` and has never recorded the currency its
/// number was in, so nothing here can say what unit a point from last week
/// used. This heading used to name the configured settlement currency; that
/// was a claim about points this plugin cannot check, and with holdings now
/// priced in their own currencies it would be wrong for any Track whose total
/// is not in that currency. Recording the currency on new points, and breaking
/// the series where the unit changes, is the next slice's work.
fn history_table(points: &[Value]) -> Value {
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
            { "key": "total", "label": "Total", "align": "right" },
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
            holdings_table(Vec::new(), &PortfolioTotal::Nothing, true, &at),
        ) {
            Refreshed::NothingHeld
        } else {
            Refreshed::Partially("the (now empty) holdings table could not be published".into())
        };
    }

    let (rows, total, complete) = price_holdings(cfg, &holdings, cache);

    // Each row's `price × qty` was checked for finiteness, but the sum of
    // finite values can still overflow — and, since S2, the priced rows may
    // not all be in one currency. Either way there is no total, and the
    // per-asset rows are exactly what a reader needs when there is none.
    if let Some(why) = total.no_total_reason() {
        eprintln!("market: no total for {track_id} — {why}; publishing the rows without one");
    }
    if !push_overlay(
        rpc,
        track_id,
        "portfolio.holdings",
        holdings_table(rows, &total, complete, &at),
    ) {
        return Refreshed::Partially("the holdings table could not be published".into());
    }
    if !complete {
        return Refreshed::Partially(
            "some holdings could not be priced; the history point was skipped".into(),
        );
    }
    // KNOWN GAP, registered rather than worked around. A history point is a
    // number over time, so it needs a total — and a portfolio whose priced
    // holdings span two currencies has none until exchange rates land. Such a
    // Track therefore contributes NO history points in the meantime, exactly
    // as a Track with an unpriceable holding already does, and its series
    // stands still until it is priced in one currency again or FX arrives.
    // Inventing a rate, or summing the currencies as though they were one,
    // would keep the series moving by publishing a number that is not the
    // portfolio's value — which is the failure this slice exists to prevent.
    let Some((total, _currency)) = total.stated() else {
        return Refreshed::Partially(format!(
            "there is no portfolio total — {}; the history point was skipped",
            total
                .no_total_reason()
                .unwrap_or_else(|| "no reason recorded".into()),
        ));
    };

    let mut points = match load_history(rpc, track_id) {
        Ok(points) => points,
        Err(e) => {
            eprintln!("market: reading {track_id}'s history failed, leaving it untouched: {e}");
            return Refreshed::Partially("the history could not be read".into());
        }
    };
    // The currency is dropped here, deliberately and visibly: a stored point
    // is `{at, total}` and records no unit. So the currency a price carries
    // reaches the quote, the rows and the tables, and stops at this line — a
    // Track that switches the currency it totals in writes two series into one
    // document. Recording the unit per point is the next slice's work.
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
    if !push_overlay(rpc, track_id, "portfolio.history", history_table(&points)) {
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
/// The currency comes from the ROW for the same reason it does in the table:
/// this line can carry rows in several currencies at once, so one unit taken
/// from the configuration would be wrong for some of them.
/// Split out from the tool arm so it can be asserted on without a kernel.
fn holdings_line(rows: &[Value]) -> String {
    rows.iter()
        .map(|row| {
            let value = row["value"]
                .as_f64()
                .zip(row["currency"].as_str())
                .map(|(v, currency)| format!("{v} {currency}"))
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
            // `currency`, not `quote`: this is the unit the SOURCE priced in,
            // and it is the only unit this number is true in. The key used to
            // be the configured settlement currency, which is a different
            // fact and, for anything but a crypto holding on a default
            // install, a different string.
            Quote::Price(price, currency) => text_result(
                format!("{canonical} = {price} {currency}"),
                json!({
                    "asset": asset.symbol,
                    "venue": asset.venue.prefix(),
                    "price": price,
                    "currency": currency,
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
            let text = holdings_line(&rows);
            let summary = match (complete, total.stated()) {
                (true, Some((amount, currency))) => {
                    format!("{text}. Total {} {currency}.", round_to(amount, 2))
                }
                (false, _) => format!("{text}. No total — not every holding could be priced."),
                (true, None) => format!(
                    "{text}. No total — {}.",
                    total
                        .no_total_reason()
                        .unwrap_or_else(|| "no reason recorded".into()),
                ),
            };
            text_result(
                summary,
                json!({
                    "holdings": rows,
                    "total": total.value_cell(),
                    // The unit of `total`, which is `null` whenever `total` is
                    // — a caller must never read a number here against a unit
                    // that came from somewhere else.
                    "currency": total.currency_cell(),
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
                    "market: configured — quote={} poll={}s binance={} sina={}",
                    parsed.quote,
                    parsed.poll.as_secs(),
                    parsed.binance_endpoint,
                    parsed.sina_endpoint,
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

    /// **Binance's quote leg is pinned, whatever the install settles in.**
    ///
    /// This replaces S1's I2 equivalence, which was scoped to `quote = "USDT"`
    /// and is false here by design: the leg no longer moves with the
    /// configuration. What is asserted is the property that replaced it — the
    /// request target is `<SYMBOL>USDT` — and it is read off the wire from the
    /// shipping lookup rather than re-derived from `binance_symbol` next to
    /// `binance_symbol`. Four settlement values are exercised, not every
    /// possible one; they are chosen so that a leg still built from `cfg.quote`
    /// would produce a different target for three of them.
    ///
    /// This is registered gap 4 of the design, verified rather than assumed:
    /// an install settling in `CNY` used to build `BTCCNY`, a pair Binance
    /// does not list (`{"code":-1121,"msg":"Invalid symbol."}`), so every
    /// crypto row went unpriced and the Track's history stood still.
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
                Quote::Price(2.5, "USDT"),
                "settling in {settlement} must not change what the source quotes in"
            );
            let target = targets
                .recv_timeout(Duration::from_secs(5))
                .unwrap_or_else(|e| panic!("no request under {settlement}: {e}"));
            assert_eq!(target, "/api/v3/ticker/price?symbol=BTCUSDT");
        }
    }

    /// The leg prices itself, with no request and no configuration value.
    ///
    /// This is where `quote_asset_shortcut`'s `1.0` went. It has to keep
    /// working under a settlement currency that is NOT `USDT`: under the old
    /// shortcut, `quote = "CNY"` no longer matched the `USDT` symbol, so the
    /// holding fell through to `binance_symbol`, which concatenated it into
    /// `USDTCNY` — a pair that does not exist
    /// (`{"code":-1121,"msg":"Invalid symbol."}`). The row went unpriced,
    /// `complete` never held, and one stablecoin position froze the whole
    /// Track's history series.
    #[test]
    fn the_binance_quote_leg_prices_itself_without_a_request() {
        for settlement in ["USDT", "CNY"] {
            // A dead endpoint: anything that DID reach the network here would
            // come back `Failed`, so a green assertion means no request.
            // Both endpoints dead: nothing in this test may reach a network,
            // and anything that tried would come back `Failed`.
            let cfg = Config {
                binance_endpoint: "http://127.0.0.1:1".into(),
                sina_endpoint: "http://127.0.0.1:1".into(),
                quote: settlement.into(),
                ..cfg()
            };
            assert_eq!(
                quote_asset(&cfg, &id("USDT")),
                Quote::Price(1.0, "USDT"),
                "the leg prices itself under settlement {settlement}"
            );
            // Only a CRYPTO identity. `US:USDT` is a different asset that
            // happens to share a symbol, and answering 1.0 for it would be a
            // fabricated price — it goes to the stock source like any other
            // US name, and finds nothing at this dead endpoint.
            assert!(
                matches!(quote_asset(&cfg, &id("US:USDT")), Quote::Failed(_)),
                "US:USDT must not borrow the crypto leg's 1.0"
            );
        }
    }

    /// A loopback stand-in for `hq.sinajs.cn`.
    ///
    /// It reproduces the two things about that endpoint a parser can get
    /// wrong: the response is **GBK**, and it is `403 Forbidden` without the
    /// `Referer` header. The name field of every row below carries real GBK
    /// bytes, including the pair `B0 5C` — a character whose SECOND byte is
    /// the ASCII `\` — so a decode that shifted the ASCII delimiters would
    /// show up here rather than only in production.
    ///
    /// `respond` is handed the request target and returns the body bytes.
    fn sina_server<F>(respond: F) -> (String, mpsc::Receiver<String>)
    where
        F: Fn(&str) -> Vec<u8> + Send + 'static,
    {
        sina_server_with(move |target, has_referer| {
            // The real endpoint's rule, byte for byte: no Referer, no data.
            if has_referer {
                ("200 OK", respond(target))
            } else {
                ("403 Forbidden", b"Forbidden".to_vec())
            }
        })
    }

    /// The transport half of [`sina_server`], with the status line left to the
    /// caller so a refusal can be served unconditionally too.
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

    /// The real endpoint's answer to a request with no `Referer`: `403` with
    /// the body `Forbidden`. Here it is unconditional, so that what a caller
    /// makes of a refusal can be asserted on its own.
    fn sina_forbidden_server() -> (String, mpsc::Receiver<String>) {
        sina_server_with(|_, _| ("403 Forbidden", b"Forbidden".to_vec()))
    }

    // The fixture rows, the GBK name bytes and the response builder live in
    // ONE file, shared with the process suite in
    // `crates/calm-server/tests/cases/market_plugin_process.rs`. Two copies of
    // a wire-format fixture drift apart one edit at a time.
    include!("sina_fixture.rs");

    fn sina_cfg(endpoint: String) -> Config {
        Config {
            sina_endpoint: endpoint,
            // Nothing crypto may reach the network in these tests.
            binance_endpoint: "http://127.0.0.1:1".into(),
            ..cfg()
        }
    }

    /// **Each market's last price is read from that market's own field.**
    ///
    /// The orders are not interchangeable — US is field 1, HK is field 6
    /// (after two name fields), Shanghai and Shenzhen are field 3 — and the
    /// rows above are real, so a parser that used one index everywhere reads a
    /// previous close, a company name, or nothing at all and calls it a price.
    ///
    /// The currency is asserted with the number, because it is what the rest
    /// of the plugin now carries around: a right price under `USDT` is still
    /// a wrong number in a total.
    #[test]
    fn each_market_is_priced_from_its_own_field_in_its_own_currency() {
        let (endpoint, targets) =
            sina_server(|target| sina_fixture_body(target, SINA_FIXTURE_ROWS));
        let cfg = sina_cfg(endpoint);
        for (name, expected, target) in [
            ("US:NVDA", Quote::Price(230.36, "USD"), "/list=gb_nvda"),
            ("HK:1810", Quote::Price(27.48, "HKD"), "/list=hk01810"),
            ("SH:600519", Quote::Price(1316.94, "CNY"), "/list=sh600519"),
            ("SZ:000001", Quote::Price(11.70, "CNY"), "/list=sz000001"),
        ] {
            assert_eq!(quote_asset(&cfg, &id(name)), expected, "{name}");
            assert_eq!(
                targets.recv_timeout(Duration::from_secs(5)).as_deref(),
                Ok(target),
                "{name} must be asked for under its own market prefix"
            );
        }
    }

    /// The `Referer` header is not optional decoration: without it the real
    /// endpoint answers `403` with the body `Forbidden`, and a parser that
    /// went looking for rows in it would find none.
    ///
    /// Both halves are asserted here. The server above 403s any request
    /// missing the header, so the successful lookup proves the shipping code
    /// sends it; and a server that 403s unconditionally proves a 403 is
    /// reported as `Failed` — a broken lookup — rather than as `Unknown`,
    /// which would tell a reader the name does not exist.
    #[test]
    fn a_forbidden_response_is_a_failed_lookup_and_the_referer_is_what_avoids_it() {
        let (endpoint, _targets) =
            sina_server(|target| sina_fixture_body(target, SINA_FIXTURE_ROWS));
        assert_eq!(
            quote_asset(&sina_cfg(endpoint), &id("US:NVDA")),
            Quote::Price(230.36, "USD"),
            "the shipping request must carry the Referer this server demands"
        );

        // And a server that refuses whatever the request carries.
        let (endpoint, _targets) = sina_forbidden_server();
        let refused = quote_asset(&sina_cfg(endpoint), &id("US:NVDA"));
        assert!(
            matches!(&refused, Quote::Failed(why) if why.contains("403")),
            "a refused request is a failure, not an unknown name: {refused:?}"
        );
    }

    /// Sina's three ways of saying "no price", each mapped to what it means.
    ///
    /// * An empty payload is the answer for a name it does not list
    ///   (`gb_doge`, verified live) — `Unknown`.
    /// * A row of `0.0000` is the answer for a halted or delisted shell
    ///   (`gb_ena`) AND for a ticker that does not exist (`gb_zzzzz`), both
    ///   verified live. It is NOT a price: a holding priced at zero would be
    ///   silently dropped from a total that claimed to be complete.
    /// * A response with no row for the symbol at all is a malformed answer to
    ///   the question asked — `Failed`, not `Unknown`.
    ///
    /// The zero case goes through [`positive_price`], the same function
    /// Binance's `> 0.0` guard calls; there is no second copy of the
    /// condition here to drift from it.
    #[test]
    fn an_empty_row_a_zero_row_and_a_missing_row_are_three_different_answers() {
        let (endpoint, _targets) = sina_server(|target| {
            sina_fixture_body(
                target,
                &[
                    // `gb_ena`, as observed: a shell of zeros.
                    ("gb_ena", "ENA,0.0000,0.00,2014-04-19 10:06:28,0.0000"),
                    // `gb_doge` is absent from this table, so it answers "".
                ],
            )
        });
        let cfg = sina_cfg(endpoint);
        assert_eq!(quote_asset(&cfg, &id("US:DOGE")), Quote::Unknown, "empty");
        assert_eq!(quote_asset(&cfg, &id("US:ENA")), Quote::Unknown, "0.0000");

        // A response that answers a different symbol than the one asked for.
        let (endpoint, _targets) =
            sina_server(|_| b"var hq_str_gb_other=\"OTHER,1.0\";\n".to_vec());
        let cfg = sina_cfg(endpoint);
        let answered = quote_asset(&cfg, &id("US:NVDA"));
        assert!(
            matches!(&answered, Quote::Failed(why) if why.contains("gb_nvda")),
            "a response with no row for the symbol asked about is malformed: {answered:?}"
        );
    }

    /// **A code whose quote currency the code does not fix is refused.**
    ///
    /// This is the defect the venue-per-currency shortcut produced, and it is
    /// the one a cross-currency total cannot catch, because it happens INSIDE
    /// one venue: every row still says `CNY`, so `price_holdings` sees one
    /// currency, states a total, and appends it to the history series.
    ///
    /// All three constructions below were read off the live endpoint on
    /// 2026-09-07, and all three are listed on exactly ONE exchange — so
    /// asking a second exchange, which is what the old `CN` venue did, would
    /// not have caught any of them:
    ///
    /// * `sh900932` 陆家Ｂ股 0.385 — a Shanghai B share, quoted in **USD**.
    /// * `sz200725` 京东方Ｂ 4.770 — a Shenzhen B share, quoted in **HKD**
    ///   (against `sz000725` 京东方Ａ at 5.680 CNY, the same company).
    /// * `hk89988` 阿里巴巴－ＷＲ 94.45 — a renminbi counter, quoted in
    ///   **CNY** (against `hk09988` at 111.00 HKD, the same company).
    ///
    /// The refusal is `Failed`, not `Unknown`: nothing about the name is
    /// unknown, and a reader who is told "no source lists this" would go
    /// looking for a spelling mistake. And no request goes out at all — the
    /// currency cannot be determined, so there is nothing to ask.
    #[test]
    fn a_code_whose_currency_the_code_does_not_fix_is_refused_before_any_request() {
        // The fixture WOULD answer all three with a price, so a plugin that
        // asked and labelled the answer would come back `Price`, not `Failed`.
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

    /// **`CN:` parses so that a row written under it cannot vanish.**
    ///
    /// The slice before the Shanghai/Shenzhen split published `CN:` and stored
    /// it, so `CN:600519` exists in KV documents now. Both halves below are
    /// the point, and the first is the one that made this venue stay:
    ///
    /// 1. It still parses, so [`holdings_from_value`] keeps the row. Were it
    ///    to stop parsing, that function would drop it WITHOUT a word and the
    ///    next `market.holdings.set` — which writes the whole array back —
    ///    would erase it from the store permanently, with no message anywhere.
    /// 2. It is never priced. `CN` is not an exchange, so the answer is a
    ///    `Failed` naming the two prefixes and asking which exchange lists the
    ///    code, and no request goes out to guess between them. Exactly one of
    ///    the two prefixes prices any given code; the message does not claim
    ///    both would.
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

        // The fixture would answer `sh600519`, so a `CN` that fell back to
        // asking Shanghai would come back `Price` here rather than `Failed`.
        // The `sz600519` row is in the fixture to show the same code is a
        // different number on the other exchange; nothing reaches it, because
        // `600519` is outside Shenzhen's renminbi ranges and `SZ:600519` is
        // refused before any request — which is also why the failure message
        // asks which exchange lists the code instead of offering the two
        // spellings as equivalent.
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

    /// The mainland fund ranges are priced, in renminbi, alongside the shares.
    ///
    /// These are not B shares and share none of their currency problem: the
    /// four codes below were read off the live endpoint on 2026-09-07 and are
    /// renminbi like the A-share boards they sit on. Refusing them would cost
    /// the most commonly held mainland instruments for nothing.
    ///
    /// The last assertion is the other half: being inside an allowed range is
    /// not a promise the source lists the code. `sz162201` is a LOF the source
    /// answers with an empty row, and the honest answer to that is `Unknown`
    /// after a real request — not the currency refusal, which would say
    /// something false about why.
    #[test]
    fn the_mainland_fund_ranges_price_in_renminbi() {
        let (endpoint, targets) = sina_server(|target| {
            sina_fixture_body(
                target,
                &[
                    // Shanghai `5xxxxx`: a broad-market ETF, a themed ETF, and
                    // a money-market fund whose ~100 unit price is real.
                    ("sh510300", "<NAME>,4.620,4.630,4.635,4.640,4.610"),
                    ("sh563210", "<NAME>,1.940,1.945,1.949,1.955,1.938"),
                    ("sh511990", "<NAME>,99.990,99.995,99.999,100.000,99.980"),
                    // Shenzhen `15xxxx`: ChiNext ETF.
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
                Quote::Price(price, "CNY"),
                "{name} is a renminbi fund and must be priced",
            );
            assert_eq!(
                targets.recv_timeout(Duration::from_secs(5)).as_deref(),
                Ok(target),
            );
        }
        // `16xxxx` is an allowed range too, and this one is simply not listed.
        assert_eq!(quote_asset(&cfg, &id("SZ:162201")), Quote::Unknown);
        assert_eq!(
            targets.recv_timeout(Duration::from_secs(5)).as_deref(),
            Ok("/list=sz162201"),
            "an allowed range is asked about; only the answer is empty",
        );
    }

    /// The mainland A-share ranges each resolve against their OWN exchange,
    /// and the exchange comes from the identity rather than from the digits.
    ///
    /// `600519` on Shenzhen and `000001` on Shanghai are different securities
    /// from the ones asserted above (`sh000001` is the Shanghai Composite
    /// index at ~3933, against Ping An Bank's 11.87 on `sz`, a factor of 330).
    /// The fixture answers neither, so a request that went to the wrong
    /// exchange comes back `Unknown` rather than with a plausible number.
    #[test]
    fn a_mainland_code_is_asked_of_the_exchange_the_identity_names() {
        let (endpoint, targets) = sina_server(|target| {
            sina_fixture_body(
                target,
                &[
                    // Both of these exist live; neither is what the identities
                    // below name.
                    ("sh000001", "<NAME>,3942.5093,3930.1164,3933.2397"),
                    ("sz600519", "<NAME>,1.000,1.000,1.000"),
                ],
            )
        });
        let cfg = sina_cfg(endpoint);
        // `SH:600519` asks Shanghai, which this fixture does not answer for —
        // it must NOT fall through to the `sz600519` row sitting right there.
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

    /// ChiNext (`30xxxx`) is renminbi like the Shenzhen main board, and a
    /// mainland code that is not six digits is not spellable at all.
    #[test]
    fn chinext_prices_in_renminbi_and_a_non_six_digit_code_is_not_requested() {
        let (endpoint, targets) =
            sina_server(|target| sina_fixture_body(target, SINA_FIXTURE_ROWS));
        let cfg = sina_cfg(endpoint);
        assert_eq!(
            quote_asset(&cfg, &id("SZ:300750")),
            Quote::Price(348.20, "CNY")
        );
        assert_eq!(
            targets.recv_timeout(Duration::from_secs(5)).as_deref(),
            Ok("/list=sz300750"),
        );
        // Not six digits: unspellable, and no request.
        assert_eq!(quote_asset(&cfg, &id("SH:60051")), Quote::Unknown);
        assert_eq!(quote_asset(&cfg, &id("SZ:MAOTAI")), Quote::Unknown);
        assert!(
            targets.try_recv().is_err(),
            "a name this source cannot spell must not reach it"
        );
    }

    /// A Hong Kong identity reaches the source as `hk<its five digits>`, and a
    /// symbol that is not a five-digit code is not padded or trimmed into one.
    ///
    /// The padding itself happens in [`canonical_symbol`] at parse time — see
    /// [`hong_kong_spellings_of_one_code_are_one_identity`] — so what this
    /// test pins is the other half: that the symbol reaches the URL unaltered,
    /// and that a non-code gets no request at all. One digit is a real case,
    /// not a degenerate one: `HK:1` is `hk00001`, 长和 / CKH Holdings, which
    /// answers with a price live.
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
            Quote::Price(27.48, "HKD")
        );
        assert_eq!(
            targets.recv_timeout(Duration::from_secs(5)).as_deref(),
            Ok("/list=hk01810"),
        );
        assert_eq!(quote_asset(&cfg, &id("HK:1")), Quote::Price(69.30, "HKD"));
        assert_eq!(
            targets.recv_timeout(Duration::from_secs(5)).as_deref(),
            Ok("/list=hk00001"),
            "`HK:1` is the identity `HK:00001`, and it is asked for, not refused",
        );
        // `TENCENT` is a legal identity — the grammar takes any `[A-Z0-9]+` —
        // and this source has no way to spell it. It answers `Unknown`
        // WITHOUT a request rather than padding a guess.
        assert_eq!(quote_asset(&cfg, &id("HK:TENCENT")), Quote::Unknown);
        // Longer than five digits with nothing to strip: no Hong Kong code,
        // and not folded into one either.
        assert_eq!(quote_asset(&cfg, &id("HK:123456")), Quote::Unknown);
        assert!(
            targets.try_recv().is_err(),
            "a name this source cannot spell must not reach it"
        );
    }

    /// **Every spelling of one Hong Kong code is ONE identity.**
    ///
    /// Leading zeros are optional in the way people write these codes, and
    /// `HK:1810` and `HK:01810` are the same shares of Xiaomi at the same
    /// 27.48 HKD. Left as two `AssetId`s they are two identities everywhere
    /// downstream — `holdings.retain` matches neither against the other and
    /// the price cache keys them apart — so a portfolio recorded one way and
    /// re-recorded the other holds the position twice, in one currency, in a
    /// total with nothing visibly wrong with it. The process-level proof that
    /// the second write REPLACES the first lives in
    /// `market_plugin_process.rs`; this is the identity underneath it.
    ///
    /// The canonical form is the padded one, `HK:01810`, because that is what
    /// HKEX and this source both write.
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

        // The fold reaches past five characters, so a refused range answers
        // the same however it was written. Before it did not: `HK:089988` was
        // over five characters and unspellable — an `Unknown` — while
        // `HK:89988` was a currency refusal, two answers for one security.
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

    /// **The defect this slice exists to prevent: a total across currencies.**
    ///
    /// A Track holding 1 NVDA (USD) and 1 USDT prices both rows fine. Adding
    /// 230.36 to 1 gives 231.36 of nothing — not USD, not USDT — and that
    /// number would go on the table as the portfolio's value, into the history
    /// series, and out of `market.holdings.list` as prose. There are no
    /// exchange rates in this slice, so the honest answer is no total.
    ///
    /// Both of the exits that state a total are checked below — the overlay
    /// table, and the two accessors `market.holdings.list` builds its
    /// structuredContent from. (`market.quote` prices one asset and states no
    /// total, so it has nothing to withhold.)
    #[test]
    fn a_total_is_withheld_when_the_priced_rows_are_in_two_currencies() {
        let (endpoint, _targets) =
            sina_server(|target| sina_fixture_body(target, SINA_FIXTURE_ROWS));
        let cfg = sina_cfg(endpoint);
        // `USDT` prices at 1.0 off the pinned leg with no request, so this
        // portfolio is FULLY priced — the refusal below is about currencies,
        // not about a missing price.
        let (rows, total, complete) = price_holdings(
            &cfg,
            &[holding("USDT", 1.0), holding("US:NVDA", 1.0)],
            &mut PriceCache::new(),
        );
        assert!(complete, "both rows priced: {rows:?}");
        assert_eq!(
            total,
            PortfolioTotal::AcrossCurrencies(vec!["USDT".into(), "USD".into()]),
        );
        assert_eq!(rows[0]["currency"], json!("USDT"));
        assert_eq!(rows[1]["currency"], json!("USD"));

        // Exit 1: the overlay table. The Total row states nothing and the
        // caption says why.
        let table = holdings_table(rows.clone(), &total, complete, "2026-09-07T12:00:00Z");
        let table_rows = table["rows"].as_array().expect("rows");
        assert!(
            table_rows.last().expect("total row")["value"].is_null(),
            "231.36 is not the value of this portfolio: {table_rows:?}"
        );
        let caption = table["caption"].as_str().expect("caption");
        assert!(caption.contains("USDT and USD"), "{caption}");
        assert!(caption.contains("no exchange rates"), "{caption}");
        assert_eq!(validate_payload(KIND_TABLE, &table), Ok(()));

        // Exit 2: the prose and the two total accessors `market.holdings.list`
        // fills its structuredContent from. This calls those directly rather
        // than through the dispatcher, so what it pins is the values that tool
        // publishes, not the wiring that carries them; the wiring is covered by
        // the process suite's cross-currency test.
        assert!(total.stated().is_none());
        assert!(total.value_cell().is_null());
        assert!(total.currency_cell().is_null());
        let line = holdings_line(&rows);
        assert!(line.contains("1 USDT"), "{line}");
        assert!(line.contains("230.36 USD"), "{line}");
        assert!(!line.contains("231.36"), "{line}");
    }

    /// A portfolio that IS in one currency still totals — in that currency,
    /// whatever the install settles in.
    ///
    /// Without this, "withhold the total when the currencies differ" could be
    /// satisfied by never stating one, and the plugin would have stopped doing
    /// the job it exists for.
    #[test]
    fn a_single_currency_portfolio_still_states_its_total() {
        let (endpoint, _targets) =
            sina_server(|target| sina_fixture_body(target, SINA_FIXTURE_ROWS));
        let cfg = Config {
            // Settling in CNY changes nothing about what the sources quote.
            quote: "CNY".into(),
            ..sina_cfg(endpoint)
        };
        let (rows, total, complete) = price_holdings(
            &cfg,
            &[holding("US:NVDA", 2.0), holding("US:NVDA", 2.0)],
            &mut PriceCache::new(),
        );
        assert!(complete);
        assert_eq!(
            total,
            PortfolioTotal::Priced {
                amount: 921.44,
                currency: "USD".into()
            },
            "two USD rows total in USD, not in the settlement currency: {rows:?}"
        );
        let caption = holdings_table(rows, &total, complete, "2026-09-07T12:00:00Z");
        assert!(
            caption["caption"]
                .as_str()
                .expect("caption")
                .contains("totalled in USD"),
            "{caption}"
        );

        // And an all-crypto portfolio, the shape every existing install has.
        let (rows, total, complete) = price_holdings(
            &cfg,
            &[holding("USDT", 3.0), holding("USDT", 4.0)],
            &mut PriceCache::new(),
        );
        assert!(complete && rows.len() == 2);
        assert_eq!(
            total,
            PortfolioTotal::Priced {
                amount: 7.0,
                currency: "USDT".into()
            },
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
        // A prefix that names no venue is refused rather than swallowed as
        // part of a bare name: pricing `SH:600519` as crypto `SH:600519`, or
        // as anything else, would be a number nobody asked for.
        // `CN:600519` is deliberately NOT in this list — see
        // [`a_stored_cn_holding_reads_back_and_is_refused_out_loud`].
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

    /// A stock identity is never handed to the crypto provider, and a crypto
    /// identity is never handed to the stock one.
    ///
    /// Routing `US:BTC` to Binance would price it at bitcoin's price — a
    /// number for an identity nobody quoted. The two endpoints below are
    /// separate servers, so which one a name reaches is observable: the stock
    /// server answers, the Binance one is dead. Two venues are exercised here,
    /// `CRYPTO` and `US`; that the OTHER stock venues reach the stock source
    /// under their own market prefix is what
    /// [`each_market_is_priced_from_its_own_field_in_its_own_currency`] pins.
    #[test]
    fn a_crypto_and_a_us_identity_reach_two_different_sources() {
        let (endpoint, targets) = sina_server(|target| {
            sina_fixture_body(
                target,
                &[
                    // A `gb_btc` row exists here on purpose: if `CRYPTO:BTC`
                    // were routed to this source it would come back priced,
                    // and the assertion below would catch it.
                    ("gb_btc", "<NAME>,1.0,0,2026-09-07 00:00:00"),
                    ("gb_nvda", "<NAME>,230.3600,0.84,2026-09-05 09:46:13"),
                ],
            )
        });
        let cfg = sina_cfg(endpoint);
        // A crypto name goes to the DEAD Binance endpoint, so it fails —
        // it does not quietly pick up the 1.0 sitting in the stock source.
        assert!(
            matches!(quote_asset(&cfg, &id("BTC")), Quote::Failed(_)),
            "a crypto identity still goes to Binance"
        );
        // And `US:BTC` is a different asset that reaches the stock source.
        assert_eq!(quote_asset(&cfg, &id("US:BTC")), Quote::Price(1.0, "USD"));
        assert_eq!(
            quote_asset(&cfg, &id("US:NVDA")),
            Quote::Price(230.36, "USD")
        );
        let asked: Vec<String> = std::iter::from_fn(|| targets.try_recv().ok()).collect();
        assert_eq!(asked, vec!["/list=gb_btc", "/list=gb_nvda"], "{asked:?}");
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
        let (endpoint, _targets) = sina_server(|target| sina_fixture_body(target, &[]));
        let cfg = sina_cfg(endpoint);
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
        let line = holdings_line(&rows);
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
        assert_eq!(
            quoted["structuredContent"]["currency"],
            json!("USDT"),
            "the unit of the number, from the source that produced it"
        );

        let table = holdings_table(rows, &total, complete, "2026-09-06T12:00:00Z");
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
    fn pushed_payloads_are_valid_report_table_blocks() {
        // The load-bearing one: whatever this plugin pushes is read back by a
        // report `table` block, so it must satisfy the KERNEL's validator —
        // not a second opinion written here, which could agree with the
        // plugin and disagree with the renderer.
        let cfg = cfg();
        let (rows, total, complete) =
            price_holdings(&cfg, &[holding("USDT", 3.0)], &mut PriceCache::new());
        assert_eq!(
            total,
            PortfolioTotal::Priced {
                amount: 3.0,
                currency: "USDT".into()
            }
        );
        assert!(complete);
        let holdings = holdings_table(rows, &total, complete, "2026-09-06T12:00:00Z");
        assert_eq!(validate_payload(KIND_TABLE, &holdings), Ok(()));

        let points = vec![
            json!({ "at": "2026-09-06T12:00:00Z", "total": 100.0 }),
            json!({ "at": "2026-09-06T12:00:30Z", "total": 110.0 }),
        ];
        assert_eq!(
            validate_payload(KIND_TABLE, &history_table(&points)),
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
        assert_eq!(
            total,
            PortfolioTotal::Priced {
                amount: 3.0,
                currency: "USDT".into()
            },
            "the total covers the priced rows only"
        );
        assert_eq!(rows.len(), 2, "the unpriceable holding keeps its row");
        assert!(rows[1]["price"].is_null() && rows[1]["value"].is_null());
    }

    #[test]
    fn an_unsummable_portfolio_still_publishes_its_rows() {
        // The rows are the only place the per-asset detail exists. Suppressing
        // the whole table would leave the previous overlay on screen, read as
        // current — the failure mode is silence, not a wrong number.
        let table = holdings_table(
            vec![json!({ "asset": "BTC", "qty": 1.0, "price": 2.0, "value": 2.0 })],
            &PortfolioTotal::NotFinite,
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
        assert_eq!(
            total,
            PortfolioTotal::NotFinite,
            "the sum is what `refresh` must refuse"
        );
        assert!(total.value_cell().is_null());
        assert!(total.stated().is_none());
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

    #[test]
    fn configuration_is_all_optional_and_the_poll_interval_is_floored() {
        let init = |values: Value| json!({ "params": { "_meta": { "dev.neige/config": { "values": values } } } });
        // An unconfigured install is a working install.
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
