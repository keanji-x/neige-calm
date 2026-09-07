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
// **The prices are in different currencies**, and converting between them is
// [`fx_path`]'s job, not this layer's. A [`Quote::Price`] carries the currency
// this plugin determined the number is in, and that currency travels with the
// number to every exit; a conversion happens once, at the portfolio layer, and
// states which quotes it multiplied together.
//
// **Neither source states a currency.** Binance's `/ticker/price` answers a
// bare number for a pair whose quote leg this plugin pinned itself (`USDT`),
// and Sina's `/list=` rows carry no unit at all. So the currency of a stock
// price is DECIDED HERE, from the venue and the code range — and where that
// decision cannot be made the quote is refused rather than labelled with a
// guess. See [`sina_target`] for which ranges are priced and why.

/// A currency this plugin can attach to a number.
///
/// An enum rather than a string because every currency reaching this plugin is
/// one of exactly four, and both things done with one — labelling a price, and
/// finding a conversion to another — have to be total over that set. With a
/// free string, "no rate came back for this pair" and "this is not a currency
/// this plugin knows" would be the same runtime miss; the second is a bug and
/// the first is just the market.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum Currency {
    /// Binance's pinned quote leg. It is a currency of its own here rather
    /// than a spelling of [`Currency::Usd`], because a price quoted in it was
    /// quoted in it — but it does not SETTLE: a portfolio stated in USDT is
    /// stated in USD instead, at [`USDT_USD_ASSUMED_PARITY`].
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

    /// The currency a configured `quote` settles a portfolio in, or `None`
    /// when this plugin does not settle in it.
    ///
    /// **Two currencies settle: `USD` and `CNY`.** `HKD` does not, and neither
    /// does anything unrecognised — `quote` is a free string that nothing
    /// validates, so this is where an unusable value becomes a visible absence
    /// rather than a guess.
    ///
    /// `USDT` settles as `USD`. It is this plugin's default and the value
    /// every existing install already has written down, and refusing it would
    /// take the total away from every crypto-only portfolio that has one
    /// today. What it costs is [`USDT_USD_ASSUMED_PARITY`]'s 3.2 basis points;
    /// what it changes for such an install is the LABEL on a total it already
    /// had — `USD` rather than `USDT`, on the same number.
    fn settlement(quote: &str) -> Option<Self> {
        match quote.trim().to_ascii_uppercase().as_str() {
            "USD" | "USDT" => Some(Self::Usd),
            "CNY" => Some(Self::Cny),
            _ => None,
        }
    }
}

/// What a provider answered, or why it could not.
#[derive(Clone, Debug, PartialEq)]
enum Quote {
    /// A positive, finite price, and the currency the number is in — never the
    /// configured settlement currency. The two are the same only by
    /// coincidence, and labelling an HKD price `USDT` because that is what the
    /// operator configured is the defect this payload exists to prevent.
    ///
    /// Neither source states its currency, so this is this plugin's own
    /// determination — Binance's pinned quote leg for crypto, the venue and
    /// code range for a stock. A code whose currency cannot be determined that
    /// way is [`Quote::Failed`], not a `Price` with a guessed unit.
    Price(f64, Currency),
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

/// The currency [`BINANCE_QUOTE_LEG`] denominates a price in. Written once,
/// next to the symbol it belongs to, so the two cannot come to disagree.
const BINANCE_QUOTE_CURRENCY: Currency = Currency::Usdt;

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
        Some(price) => Quote::Price(price, BINANCE_QUOTE_CURRENCY),
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
    currency: Currency,
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
            currency: Currency::Usd,
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
                currency: Currency::Hkd,
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
                currency: Currency::Cny,
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
                currency: Currency::Cny,
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

/// One row of Sina's quote list, `https://hq.sinajs.cn/list=<symbol>`.
///
/// `Ok(None)` is an EMPTY row, which is how this endpoint says it does not
/// list a symbol — `var hq_str_gb_doge="";` and `var hq_str_fx_susdxxx="";`,
/// both verified on 2026-09-07. A response missing the row altogether is an
/// `Err`: it is a malformed answer to the question that was asked, not an
/// answer about the symbol. Both callers — the stock price path and the
/// exchange-rate path — need that distinction, and neither restates it.
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
fn sina_row(cfg: &Config, symbol: &str) -> Result<Option<String>, String> {
    let url = format!("{}/list={symbol}", cfg.sina_endpoint);
    let response = match ureq::get(&url)
        .set("Referer", SINA_REFERER)
        .timeout(Duration::from_secs(10))
        .call()
    {
        Ok(response) => response,
        // Unlike Binance, a non-200 here carries no price shape at all — the
        // 403 the missing `Referer` earns has the literal body `Forbidden` —
        // so the status is the answer and the body is not parsed.
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
    // "We do not list this." Not an error, and not a price — `fx_susdxxx` and
    // `gb_doge` both come back this way, verified on 2026-09-07.
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
    // A row of zeros is how this source spells a halted or unlisted symbol, so
    // a non-price here is `Unknown`-shaped, not a failure.
    match positive_price(raw) {
        Some(price) => Quote::Price(price, lookup.currency),
        None => Quote::Unknown,
    }
}

// ---------------------------------------------------------------------------
// Exchange rates
// ---------------------------------------------------------------------------

/// Which field of a Sina `fx_s…` row is the current rate.
///
/// **Field 8**, and the row itself is what says so. Every one of these rows
/// carries its own change against the previous close in field 11, and that
/// column is `field 8 − field 3` in all four pairs this plugin asks for, read
/// live on 2026-09-07:
///
/// ```text
/// fx_susdcny  f8 6.7111      − f3 6.7108      =  0.0003     = f11  0.0003
/// fx_scnyusd  f8 0.149007    − f3 0.148994    =  0.000013   = f11  0.000013
/// fx_shkdusd  f8 0.1275526474 − f3 0.1275396329 = 0.0000130 = f11  0.0000 (rounded)
/// fx_shkdcny  f8 0.8560178052 − f3 0.8560104776 = 0.0000073 = f11  0.0000 (rounded)
/// ```
///
/// Field 1 is NOT interchangeable with it: on the pairs Sina computes rather
/// than quotes (marked 此行情由新浪财经计算得出 in field 13) field 1 is the bid
/// and field 2 the ask, and field 8 is their midpoint — `fx_susdcny` answered
/// bid 6.7099, ask 6.7123 and field 8 6.7111, which is exactly the mid.
/// Reading field 1 there would publish the bid as the rate.
const SINA_FX_RATE_FIELD: usize = 8;

/// **`USDT` is settled as `USD` one for one. This is an ASSUMPTION, not a
/// quote, and it is the only number in this plugin that no source stated.**
///
/// It is the owner's decision to approximate rather than to price the pair.
/// What the approximation costs was measured on one day, not guessed:
/// `data-api.binance.vision` answered
/// `{"symbol":"USDTUSD","price":"0.99967000"}` on 2026-09-07 (a second sample
/// minutes later read `0.99966000`). Against **0.99967**, this parity
/// overstates the USDT-quoted part of a portfolio by about **3.3 basis
/// points** — 33 USD per 100,000 USD held in USDT-quoted assets.
///
/// Two things that reading does NOT establish. The effect on a whole total is
/// that 3.3 bp scaled by how much of the portfolio is quoted in USDT, so a
/// mostly-stock portfolio is off by far less. And a snapshot fixes no
/// direction: `USDTUSD` has traded above 1 as well, and on such a day the same
/// parity understates instead. What is fixed is that the parity is not the
/// market's number.
///
/// **This constant is where the parity is decided, and replacing it is not by
/// itself enough to make it a quote.** A fetched leg needs the shape the Sina
/// legs already have — a request, a failure that propagates instead of a value
/// that always exists, and a place in the per-pass cache — and
/// [`FxHop::AssumedParity`] would have to stop being a variant. `USDTUSD` is
/// listed with no key; the other direction, `USDUSDT`, is not
/// (`-1121 Invalid symbol`), so a real `USD → USDT` leg would have to be a
/// reciprocal. Nothing else in this file hard-codes a rate.
///
/// **Where the assumption is visible, exactly.** Every exit that states a
/// converted TOTAL names the hops behind it and names this one as an
/// assumption: the holdings table's caption, `market.holdings.list`'s prose,
/// and that tool's `conversions` array. The per-row `rate` cell is a bare
/// number in every case — a machine consumer reading only `holdings[i].rate`
/// cannot tell an assumed 1 from a quoted one, and has to read `conversions`.
/// See [`FxHop::describe`].
const USDT_USD_ASSUMED_PARITY: f64 = 1.0;

/// One hop of a conversion.
///
/// A hop is either a rate a source answered or the parity this plugin assumes,
/// and the two are never printed alike: a reader shown only a product cannot
/// otherwise tell a quoted rate from an assumed one, and the assumed one is
/// the number that could be wrong without any source being down.
#[derive(Clone, Debug, PartialEq)]
enum FxHop {
    /// A rate read off a source, with the symbol it was read from.
    Quoted { label: &'static str, factor: f64 },
    /// [`USDT_USD_ASSUMED_PARITY`]. No source was asked, and none could fail.
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

/// A rate this plugin knows how to fetch. Also the [`FxCache`] key, so two
/// conversions that need the same pair fetch it once per pass.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
struct FxLeg {
    from: Currency,
    to: Currency,
}

impl FxLeg {
    /// The Sina symbol for this pair. Sina quotes every ordered pair over
    /// `USD`, `HKD` and `CNY` natively, so the symbol asked for is always the
    /// direction wanted; this plugin never divides into the opposite one.
    fn symbol(self) -> String {
        format!(
            "fx_s{}{}",
            self.from.code().to_ascii_lowercase(),
            self.to.code().to_ascii_lowercase()
        )
    }

    /// What the caption calls this hop.
    ///
    /// The four pairs below are every leg a CONFIGURABLE settlement can reach:
    /// [`Currency::settlement`] returns only `Usd` and `Cny`, and the one arm
    /// of [`fx_route`] that builds a `Sina` leg puts the settlement currency in
    /// `to`. `fx_route` itself is a total function over `Currency` pairs and
    /// will happily build a fifth leg — `(Usd, Hkd)` — for a caller that asks
    /// for one; nothing in this plugin asks. The fallback therefore names no
    /// symbol rather than naming a wrong one.
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

/// One pass's exchange rates, keyed by pair. Same lifetime and same reason as
/// [`PriceCache`]: a pass must not ask for one rate several times, and a later
/// pass must not reuse this one's.
type FxCache = HashMap<FxLeg, Result<f64, String>>;

/// Everything one pass fetched from the open world, so a pass over many Tracks
/// asks each source once per distinct thing it needs.
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

/// Fetch one pair, once per pass.
///
/// A failure is cached alongside a success on purpose: within one pass a rate
/// either came back or did not, and re-asking a source that just refused would
/// let two rows of one portfolio be converted at rates fetched under different
/// conditions. The cache is dropped at the end of the pass, so the next pass
/// asks again — **there is no stale-rate fallback anywhere in this plugin**,
/// and a pair that fails leaves its rows unconverted rather than reaching for
/// the value it had last time.
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
    // The same test as a price, and for the same reason: a row of zeros is how
    // this source spells "nothing live here", and a rate of zero or less would
    // value a whole portfolio at nothing.
    positive_price(raw)
        .ok_or_else(|| format!("{symbol}: `{}` is not a positive finite rate", raw.trim()))
}

/// One step of a route, before it has a number: either a pair to fetch or the
/// assumed parity. Kept apart from [`FxHop`] so that WHICH steps a conversion
/// takes is a table with no network in it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum FxStep {
    Parity,
    Fetch(FxLeg),
}

/// The steps that carry `from` to `to`, or `None` when this plugin has no
/// route.
///
/// Settlement is `USD` or `CNY` and nothing else, so this table is the whole
/// of it. **At most one fetched rate in any route**: the `USDT` step is the
/// assumed parity, not a request.
///
/// ```text
/// USDT → USD    assumed parity
/// USDT → CNY    assumed parity, then fx_susdcny
/// USD  → CNY    fx_susdcny
/// CNY  → USD    fx_scnyusd
/// HKD  → USD    fx_shkdusd
/// HKD  → CNY    fx_shkdcny
/// ```
///
/// `from == to` never reaches here; [`fx_path`] answers that with no hops at
/// all rather than with a rate of 1 read off a source, because there is no
/// conversion to make and a portfolio that needs none must not be left
/// unpriceable by an unreachable source.
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

/// A conversion this plugin actually obtained: the hops, in order, and their
/// product.
#[derive(Clone, Debug, PartialEq)]
struct FxPath {
    from: Currency,
    to: Currency,
    hops: Vec<FxHop>,
    factor: f64,
}

impl FxPath {
    /// One line a reader can check: which currencies, at what rate, out of
    /// which quotes — and which step of it, if any, is assumed rather than
    /// quoted.
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

/// What multiplies a number in `from` into a number in `to`, this pass.
///
/// `Err` is the honest answer to a rate that did not come back, and it is what
/// leaves a holding's converted value `null`. There is deliberately no
/// fallback: no cached rate from an earlier pass, no rate assumed between
/// currencies whose names look related, no total assembled out of the rows
/// that did convert. A rate this plugin could not read this pass is a rate it
/// does not have.
///
/// The one number here that is not read from a source is
/// [`USDT_USD_ASSUMED_PARITY`], and every exit that publishes a conversion
/// says so in words.
fn fx_path(
    cfg: &Config,
    from: Currency,
    to: Currency,
    cache: &mut FxCache,
) -> Result<FxPath, String> {
    // Identity. Not a conversion, so no source is asked and no source can make
    // it fail: a portfolio already in the settlement currency prices with the
    // rate endpoint unreachable, exactly as it did before rates existed.
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

/// What a set of priced rows sums to — or why it does not sum to anything.
///
/// Rows reach here already converted into one settlement currency, so summing
/// them is a sum of like things. What is left to refuse is the case where the
/// conversion could not happen at all: [`PortfolioTotal::Unsettleable`] covers
/// a configured settlement currency this plugin does not settle in, which
/// leaves each row in the currency its own source quoted. `total += value`
/// over those produces a figure in no currency at all — 100 USD plus 100 HKD
/// is not 200 of anything — so the honest answer is no total, and the
/// per-asset rows, each with its own currency, carry everything a reader can
/// actually use.
///
/// A row whose price came back but whose RATE did not is not in here at all:
/// it is left out of the sum and marks the pass incomplete, the same as an
/// unpriceable row.
#[derive(Clone, Debug, PartialEq)]
enum PortfolioTotal {
    /// Every counted row was in `currency`, and they sum to a finite `amount`.
    Priced { amount: f64, currency: String },
    /// `configured` is not a currency this plugin settles in, so nothing was
    /// converted and the rows stand in more than one currency — listed in the
    /// order they were first seen.
    Unsettleable {
        configured: String,
        currencies: Vec<String>,
    },
    /// One currency, but the values do not sum to a finite number.
    NotFinite,
    /// The portfolio is EMPTY. Its total really is zero — that is not a
    /// stand-in for an unknown number, it is the value of holding nothing.
    Empty,
    /// The portfolio is NOT empty and not one row of it could be priced and
    /// converted. Its total is unknown, and zero is a wrong answer for it: a
    /// reader shown `0` here would read "this portfolio is worth nothing"
    /// where the truth is "this plugin could not value it".
    ///
    /// This and [`Self::Empty`] were one variant until #1556 S3, because the
    /// count of counted currencies is zero in both cases. That put a `0` on
    /// the table under a caption saying there was no total — a number in front
    /// of a reader and a sentence denying it exists. The two are separate here
    /// because they are separate facts, not because a check was added.
    NonePriced,
}

impl PortfolioTotal {
    /// Whether there is a portfolio value to announce and to plot, and in
    /// what unit.
    ///
    /// This is NOT "is the value cell honest". [`Self::Empty`]'s `0` is a true
    /// number — the value of holding nothing — and this still answers `None`,
    /// for two reasons that are about the caller rather than about the zero:
    ///
    /// - There is no unit to pair it with. Nothing was priced, so no currency
    ///   was counted, and the configured settlement may not be one this plugin
    ///   settles in. A number handed out with no unit is what this whole layer
    ///   exists to prevent.
    /// - The caller that appends history points reads this, and a series of
    ///   zeros for a Track that holds nothing is a series about nothing. That
    ///   is a choice about what the history is FOR, not a claim that the zero
    ///   is wrong.
    ///
    /// That `refresh` also returns early for an empty portfolio does not make
    /// this redundant. If this said `Some`, the history series would be correct
    /// only because of that early return, and a second caller added later would
    /// inherit a defect nothing here warned about.
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

    /// The `value` cell of the `Total` row. A number only where this plugin
    /// can state one.
    ///
    /// [`Self::Empty`] is the only variant outside [`Self::Priced`] with a
    /// number, and it is a real one: the total of an empty portfolio is zero.
    /// Every other variant is `null`, [`Self::NonePriced`] included — a
    /// portfolio nothing could be valued in has an unknown total, and `0` is
    /// not a spelling of unknown.
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
    /// One row per holding, in the order held. Each carries its price in the
    /// currency its own source quoted, the rate that carried it into
    /// [`Self::settlement`], and its value in that settlement currency.
    rows: Vec<Value>,
    total: PortfolioTotal,
    /// Whether every holding both priced AND converted. A history point is
    /// appended only when this holds; see [`refresh`].
    complete: bool,
    /// The currency values are stated in, or `None` when the configured one is
    /// not a currency this plugin settles in — in which case nothing was
    /// converted and each row's value is in its own source's currency.
    settlement: Option<Currency>,
    /// One line per conversion actually used, in first-use order, each naming
    /// its hops. Empty when nothing needed converting.
    conversions: Vec<String>,
}

/// Price every holding of one portfolio and convert each into the settlement
/// currency.
///
/// An asset that could not be priced keeps its row with a `null` price and a
/// `null` value — dropping it would understate the portfolio silently, which is
/// exactly what a null says out loud — and is left out of the total. **A
/// holding whose price came back but whose exchange rate did not is treated
/// the same way**: its own price and currency stay on the row, because they are
/// true, and its settlement value is `null`. There is no fallback to a rate
/// from an earlier pass; see [`fx_path`].
///
/// Each row keeps the CURRENCY its price is in, straight from the source that
/// answered. Nothing here re-labels a price with the settlement currency: that
/// is how an HKD price ends up captioned `USDT`. What carries it across is a
/// rate on the row and a named conversion in [`PricedPortfolio::conversions`].
fn price_holdings(cfg: &Config, holdings: &[Holding], cache: &mut PassCache) -> PricedPortfolio {
    // `None` means the configured value is not one of the two currencies this
    // plugin settles in — see `Currency::settlement`. With no settlement
    // currency nothing is converted, so each row stays in its own source's
    // currency and a portfolio spanning two of them gets no total.
    let settlement = Currency::settlement(&cfg.quote);
    let mut rows = Vec::with_capacity(holdings.len());
    let mut sum = 0.0;
    // Distinct currencies among the COUNTED rows, first-seen order. With a
    // settlement currency there is at most one; without, one entry means the
    // sum is a number in that currency and more than one means it is not a
    // number in any.
    let mut currencies: Vec<&'static str> = Vec::new();
    let mut conversions: Vec<String> = Vec::new();
    let mut complete = true;
    for holding in holdings {
        // `price * quantity` can overflow to infinity even when both factors
        // are finite, so the product is checked as well as the input.
        let converted = match quote_cached(cfg, &holding.asset, &mut cache.prices) {
            Quote::Price(price, currency) => {
                // Each row is carried into the settlement currency, or — with
                // none configured — left where it is, which is what `to`
                // being the row's own currency means.
                let to = settlement.unwrap_or(currency);
                match fx_path(cfg, currency, to, &mut cache.rates) {
                    Ok(path) => {
                        let value = price * holding.quantity * path.factor;
                        if value.is_finite() {
                            Ok((price, currency, path, value))
                        } else {
                            // The conversion SUCCEEDED; the product overflowed.
                            // The rate is a fact this pass obtained, so it stays
                            // on the row — that is what lets every reader below
                            // tell this apart from a rate that never came back.
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
                // The rate cell exists only where a rate does. With no
                // settlement currency nothing was converted, and a column of
                // 1.0s would read as a conversion that happened.
                if settlement.is_some() {
                    row["rate"] = json!(round_to(path.factor, 8));
                }
                rows.push(row);
            }
            Err((priced, why)) => {
                complete = false;
                eprintln!("market: {why}");
                // A price that came back is kept even when the conversion did
                // not finish: it is a true number about this holding, and
                // dropping it would hide that the failure was downstream of the
                // quote. The rate is kept on the same terms — a row with a rate
                // and no value overflowed, a row with neither had no rate.
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
        // Nothing was counted. WHY nothing was counted is the difference
        // between a zero and an unknown, and the holdings themselves are what
        // says which.
        [] if holdings.is_empty() => PortfolioTotal::Empty,
        [] => PortfolioTotal::NonePriced,
        [only] if sum.is_finite() => PortfolioTotal::Priced {
            amount: sum,
            currency: (*only).to_string(),
        },
        [_] => PortfolioTotal::NotFinite,
        // Unreachable with a settlement currency configured, because every
        // counted row was converted into it. Reachable, and the whole point,
        // without one.
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
/// Two units live on this table and they are labelled apart. `Price` is in the
/// currency the source that answered quoted — that is what the `Priced in`
/// column says, per row — and `Value` is in the settlement currency, which the
/// column heading names. `Rate` is what carried one to the other, and the
/// caption spells out which quotes that rate is a product of, so a two-hop
/// conversion never reads as a directly quoted pair.
///
/// With no settlement currency to convert into, the rate column is not shown
/// at all and `Value` carries no unit in its heading: nothing was converted,
/// each row's value is in its own `Priced in` currency, and a heading naming
/// one currency would be wrong for the others.
///
/// The table still goes out when there is no total: it is the only place the
/// per-asset rows appear, and withholding it would leave whatever was
/// published last on screen, presented as current.
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
        // An empty portfolio's Total cell IS a number, so the caption must
        // explain the zero rather than deny it. Every other unstated total
        // shows `null`, and there the caption says why there is none.
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
        // "or rates": a row is left out of the total both when its price did
        // not come back and when its exchange rate did not, and the caption
        // may not name only the first.
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
        // The venue is its own column rather than a prefix glued onto the
        // name: `W` on two venues is two different companies, and a reader has
        // to be able to tell which row is which.
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

/// The settlement currency a stored history point was written in, or `None`
/// when the point does not say.
///
/// **`None` is not a missing rate; it is an unknowable unit.** Points written
/// before this slice are `{at, total}` and record no currency at all, and what
/// unit each of them used was whatever the install's settlement was at that
/// moment — a value nothing kept. So a point without a currency is comparable
/// to no other point, including another point without one: two unknowns are
/// not known to be the same unknown.
fn point_currency(point: &Value) -> Option<&str> {
    point.get("currency").and_then(Value::as_str)
}

/// History as a table, newest first, with the change against the previous
/// point. `points` is `[{ "at": <rfc3339>, "total": <number>,
/// "currency": <code> }]` in chronological order, with `currency` absent on
/// points written before it was recorded.
///
/// **The change column is blank across a currency boundary.** Subtracting a
/// total in HKD from a total in USDT draws a move the portfolio never made,
/// and that is the same defect as plotting a subset total against a whole one.
/// A row's own `Currency` cell says which unit its number is in, so the table
/// no longer needs — and no longer has — a single unit in its heading.
///
/// KNOWN GAP — points written before this slice carry no currency, and their
/// unit cannot be recovered. They keep their number and get an empty currency
/// cell, and no change is computed on either side of them.
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
        // Both points must SAY what unit they are in, and say the same one.
        // `None == None` would be the mistake: it reads two unrecorded units
        // as one.
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
            // No unit in this heading: each row states its own, and points
            // that never recorded one state nothing.
            { "key": "total", "label": "Total", "align": "right" },
            { "key": "currency", "label": "Currency" },
            { "key": "change", "label": "Change", "align": "right" },
        ],
        "rows": rows,
        "caption": caption,
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
fn refresh(rpc: &Rpc, cfg: &Config, track_id: &str, cache: &mut PassCache) -> Refreshed {
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
            holdings_table(price_holdings(cfg, &[], &mut PassCache::new()), &at),
        ) {
            Refreshed::NothingHeld
        } else {
            Refreshed::Partially("the (now empty) holdings table could not be published".into())
        };
    }

    let priced = price_holdings(cfg, &holdings, cache);
    let complete = priced.complete;

    // Each row's `price × qty × rate` was checked for finiteness, but the sum
    // of finite values can still overflow — and a settlement currency this
    // plugin does not settle in leaves the rows in currencies that do not
    // sum. Either way there is no total, and the per-asset rows are exactly
    // what a reader needs when there is none.
    if let Some(why) = priced.total.no_total_reason() {
        eprintln!("market: no total for {track_id} — {why}; publishing the rows without one");
    }
    // Taken before the payload consumes the priced portfolio, so that the
    // number appended to the series below and the number published above are
    // one value rather than two computations of it.
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
    // A history point is a number over time, so it needs a total. A portfolio
    // that has none contributes NO points, exactly as a Track with an
    // unpriceable holding does, and its series stands still until it has one
    // again. Publishing a figure assembled out of the rows that happened to
    // work would keep the series moving with a number that is not the
    // portfolio's value — the failure this whole layer exists to prevent.
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
    // **The unit goes into the document with the number.** A point used to be
    // `{at, total}`, and a Track that changed the currency it totals in wrote
    // two series into one document with nothing to tell them apart; the
    // difference between two such points is a move the portfolio never made.
    // What is stored here is what the number is in, so `history_table` can
    // refuse to subtract across the boundary rather than guess where one is.
    //
    // This fixes new points only. Points already in the store record no
    // currency, and what unit each of them used is not recoverable from
    // anything this plugin kept — see [`point_currency`].
    points.push(json!({ "at": at, "total": round_to(total, 2), "currency": currency }));
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
    let mut cache = PassCache::new();
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
/// priced portfolio.
///
/// The venue is glued to the symbol HERE, unlike in the table, because this
/// exit has no columns to put it in: `1 × W` names Wayfair and Wormhole
/// equally well, and a Planner reading the line has nothing else to go on.
///
/// Every value on this line is in ONE unit — the settlement currency — and
/// the line says which. With no settlement currency configured nothing was
/// converted, so each value falls back to its own row's currency, which is the
/// only unit that value is true in.
///
/// The three failures are told apart in words, and each is read off the row
/// rather than assumed:
///
/// - **no price** — the quote did not come back. Nothing else was attempted.
/// - **no rate** — the quote came back and the conversion did not. The row has
///   a price and no rate. A reader needs this one: it says the holding is fine
///   and this plugin's rate source is not.
/// - **not a finite value** — both came back and `price × quantity × rate`
///   overflowed. The row has a price AND a rate. This case needs no rate at
///   all when the holding is already in the settlement currency, so reporting
///   it as a missing rate names a lookup that never happened.
///
/// Split out from the tool arm so it can be asserted on without a kernel.
fn holdings_line(priced: &PricedPortfolio) -> String {
    priced
        .rows
        .iter()
        .map(|row| {
            let native = row["currency"].as_str();
            let unit = priced.settlement.map_or(native, |s| Some(s.code()));
            let value = match (row["value"].as_f64().zip(unit), native) {
                (Some((value, unit)), _) => format!("{value} {unit}"),
                // A rate on the row means the conversion succeeded, so what
                // failed is the arithmetic. With no settlement currency there
                // is no rate cell and no conversion either — the identity path
                // cannot fail — so an absent value there is an overflow too.
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
            let priced = price_holdings(cfg, &holdings, &mut PassCache::new());
            let text = holdings_line(&priced);
            let PricedPortfolio {
                rows,
                total,
                complete,
                conversions,
                ..
            } = priced;
            // The prose and `structuredContent.total` below are two exits on
            // ONE fact, and they must not disagree. A partial total is a real
            // number over the rows that both priced and converted — the
            // holdings table has always published it, saying what it covers —
            // and `total.value_cell()` hands it out here too. Saying "no
            // total" over the top of it would put a number in the payload and
            // a sentence denying it exists, which is the shape
            // `PortfolioTotal::Empty`/`NonePriced` were split to remove.
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
            // The conversions ride along so a caller reading this reply sees
            // the same hops the table's caption states, rather than a
            // converted total with no way to check what it was converted at.
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
                    // The unit of `total`, which is `null` whenever `total` is
                    // — a caller must never read a number here against a unit
                    // that came from somewhere else.
                    "currency": total.currency_cell(),
                    // The hops behind every rate above, in the same words the
                    // holdings table captions them with. A per-row `rate` is a
                    // bare number and cannot say whether it was quoted or
                    // assumed; this is where a machine consumer reads that.
                    "conversions": conversions,
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
                Quote::Price(2.5, Currency::Usdt),
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
                Quote::Price(1.0, Currency::Usdt),
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
            Quote::Price(230.36, Currency::Usd),
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
                Quote::Price(price, Currency::Cny),
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
            Quote::Price(348.20, Currency::Cny)
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

    /// Stocks and rates off one Sina fixture, with Binance unreachable.
    ///
    /// Nothing crypto may reach a live endpoint in these tests, and nothing
    /// has to: `USDT` prices at 1.0 off Binance's pinned leg with no request,
    /// and settling it costs no request either — the `USDT`/`USD` step is
    /// [`USDT_USD_ASSUMED_PARITY`], not a lookup.
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

    /// **The whole of S3 in one portfolio: four currencies, one total.**
    ///
    /// Four rows, quoted by two sources in USDT, USD, HKD and CNY, settling in
    /// CNY. Before this slice such a Track got per-asset rows, no total and no
    /// history point at all; the number asserted here is the one that was
    /// missing.
    ///
    /// Every rate below is a quote read off the FX rows of the same fixture
    /// the prices come from, except the USDT row's parity step, which the
    /// caption says is assumed.
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

        // Each row keeps the price and currency ITS source quoted…
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
        // …and carries the rate that took it into the settlement currency.
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

        // The reader is told what the numbers were converted at, hop by hop —
        // and which hop is not a quote.
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

        // Which pairs were asked for, in which direction. Sina lists every
        // ordered pair, so a route that divided into the opposite one would
        // show up here as the wrong symbol rather than as a failure — and one
        // `fx_susdcny` serves both the USD row and the USDT row.
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

    /// **`USDT` settles as `USD` at an assumed parity, and every exit says the
    /// parity is assumed.**
    ///
    /// The number is not in dispute — it is 1 — so what this pins is that the
    /// reader is told it was not quoted. A conversion shown as a bare rate
    /// would read as something a source answered, and this one is the only
    /// number in the plugin that no source did.
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

        // And the default `quote` — the value every existing install carries —
        // resolves to the same settlement, so such an install keeps the total
        // it had rather than losing it to an unrecognised setting.
        assert_eq!(Currency::settlement("USDT"), Some(Currency::Usd));
        assert_eq!(
            Currency::settlement(&Config::default().quote),
            Some(Currency::Usd)
        );
    }

    /// **A fiat pair is asked for in the direction it is wanted.**
    ///
    /// Sina quotes `fx_susdcny` at 6.7111 and `fx_scnyusd` at 0.149007, and
    /// those two are not reciprocals of each other — `1/6.7111` is 0.14900538,
    /// which differs from the quoted 0.149007 in the sixth decimal because the
    /// two rows were last updated an hour apart. Dividing into the wrong one
    /// would publish a number no source stated, so each direction has its own
    /// request.
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

        // The opposite direction is a different quote, not 1/6.7111.
        let (cfg, _targets) = converting_cfg("USD");
        let priced = price_holdings(&cfg, &[holding("SH:600519", 1.0)], &mut PassCache::new());
        assert_eq!(
            priced.rows[0]["rate"].as_f64(),
            Some(0.149007),
            "0.14900538 would be the reciprocal of the other row",
        );

        // And Hong Kong has its own pair per settlement currency, rather than
        // being routed through the other one.
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

    /// **A rate that did not come back leaves its row unpriceable — and does
    /// not fall back to anything.**
    ///
    /// The stock price is there; only the rate row is missing. The holding
    /// keeps the price and currency it does have, because those are true, and
    /// gets no value, no rate and no place in the total. The pass is
    /// incomplete, which is what stops a history point being written from a
    /// partial portfolio.
    #[test]
    fn a_holding_whose_rate_did_not_come_back_is_left_out_of_the_total() {
        // Stocks answer; the rate rows do not exist at this endpoint.
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

    /// **No stale rates.** A pass whose rate source is unreachable converts
    /// nothing, even when an earlier pass converted the same pair.
    ///
    /// The two passes share nothing but the `Config`: a plugin that kept the
    /// last rate anywhere outside the per-pass cache would still value the
    /// second portfolio, at the earlier number, with nothing on the table
    /// saying so.
    ///
    /// **The second pass still prices the holding.** Only the rate row goes
    /// missing, which is the whole point: if the price failed too, the row
    /// would be `null` before any rate was looked for, and this test would
    /// pass without the fallback path ever being reached.
    #[test]
    fn a_rate_from_an_earlier_pass_is_never_reused() {
        let (working, _targets) = converting_cfg("CNY");
        let first = price_holdings(&working, &[holding("US:NVDA", 1.0)], &mut PassCache::new());
        assert_eq!(first.rows[0]["rate"].as_f64(), Some(6.7111));
        assert_eq!(first.rows[0]["value"].as_f64(), Some(1545.97));

        // Same asset, same pair, a pass later. The endpoint still answers the
        // stock row and no longer lists any `fx_` row.
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

    /// **An empty portfolio's total is 0, and a portfolio nothing could be
    /// valued in has no total at all.**
    ///
    /// Both used to be one variant, because both count zero currencies, and
    /// both published `0.0` in the Total cell — under a caption that said
    /// there was no total. A number in front of a reader and a sentence
    /// denying it exists is the whole shape of this defect, so each case is
    /// asserted on the CELL and the CAPTION together: checking either alone
    /// passes on the version that had them contradicting each other.
    #[test]
    fn an_empty_portfolio_totals_zero_and_an_unvalued_one_totals_nothing() {
        // Empty: the zero is the real answer, and the caption explains it.
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

        // Non-empty, and not one row could be priced: the total is UNKNOWN,
        // and 0 is a wrong answer for it.
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

    /// **A non-empty portfolio whose every rate is unavailable states no
    /// total** — not a zero.
    ///
    /// Every holding PRICES here; only the rates are missing, which is the
    /// path S3 added to this variant. Before the split this published a `0.0`
    /// for a portfolio worth several thousand.
    #[test]
    fn a_portfolio_whose_every_rate_is_unavailable_states_no_total_not_a_zero() {
        // Stock prices answer; no `fx_` row exists at this endpoint.
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

    /// **An overflow is not a missing rate.**
    ///
    /// Settling in USD with a USD holding: the conversion path is the identity,
    /// it needs no rate and cannot fail. What fails is `price × quantity`,
    /// which overflows at `1e308` shares. Reporting that as
    /// `no USD→USD rate` names a lookup that never happened and sends a reader
    /// at the wrong source; before this was split, it also printed `?` as the
    /// currency whenever nothing settled.
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

        // The same holding with NO settlement currency: still an overflow,
        // still not a rate, and no `?` anywhere.
        let (cfg, _targets) = converting_cfg("HKD");
        let priced = price_holdings(&cfg, &[holding("US:NVDA", 1e308)], &mut PassCache::new());
        assert_eq!(priced.settlement, None);
        assert!(priced.rows[0]["value"].is_null());
        let line = holdings_line(&priced);
        assert!(line.contains("not a finite number"), "{line}");
        assert!(!line.contains('?'), "{line}");
    }

    /// **A settlement currency this plugin does not settle in converts
    /// nothing, and says so.**
    ///
    /// `quote` is a free string that nothing validates, and `HKD` is a real
    /// currency this plugin prices in but does not settle in. Rather than
    /// picking one for the operator, every row stays in the currency its
    /// source quoted — so a single-currency portfolio still totals, and one
    /// spanning two gets no total and a caption naming the configured value.
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

    /// A portfolio already in the settlement currency needs no rate at all.
    ///
    /// Without this, "convert every row" could be satisfied by making every
    /// portfolio depend on the FX endpoint being up, including the ones that
    /// have nothing to convert.
    ///
    /// **Each half asserts against the server it actually uses.** The stock
    /// half runs entirely against `fixture` — a server that would answer an
    /// `fx_` request if one were made — and the assertion is that no such
    /// request appears in ITS log. The crypto half reaches no endpoint at all,
    /// and is the only half the refusing server below constrains.
    #[test]
    fn a_portfolio_already_in_the_settlement_currency_asks_for_no_rate() {
        let (refusing, refused) = sina_forbidden_server();
        let cfg = Config {
            quote: "CNY".into(),
            binance_endpoint: "http://127.0.0.1:1".into(),
            sina_endpoint: refusing,
            ..cfg()
        };
        // This table carries the FX rows too, so a request for one would be
        // answered rather than failing — the absence below is the plugin not
        // asking, not the fixture refusing.
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

        // And the shape every existing install has: all crypto, settling in
        // the default `USDT`. Neither source is reachable and it still totals.
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

        // Exit 2: `market.holdings.list`. It hands these same rows out as its
        // structuredContent, so the venue reaches that half with them — and
        // its HUMAN-READABLE line, which has no columns, must name the venue
        // too. `1 × W` is Wayfair and Wormhole equally.
        let line = holdings_line(&priced);
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
        let priced = price_holdings(&cfg, &[holding("USDT", 3.0)], &mut PassCache::new());
        assert_eq!(
            priced.total,
            PortfolioTotal::Priced {
                amount: 3.0,
                // `USD`, not `USDT`: the default `quote` settles in USD at the
                // assumed parity, so this is the same number under the unit
                // the rates are in.
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
        // `ZZZZ` is not the quote asset, so it goes to the venue — which is
        // unreachable at this endpoint, so it fails. The priced half must
        // still be priced, and the total must cover only it.
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
        // The rows are the only place the per-asset detail exists. Suppressing
        // the whole table would leave the previous overlay on screen, read as
        // current — the failure mode is silence, not a wrong number.
        let table = holdings_table(
            PricedPortfolio {
                rows: vec![json!({ "asset": "BTC", "qty": 1.0, "price": 2.0, "value": 2.0 })],
                total: PortfolioTotal::NotFinite,
                complete: true,
                // `Usd`, not `Usdt`: `Currency::settlement` never returns
                // `Usdt`, so a hand-built portfolio settling in it would be a
                // state production cannot reach.
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
        // Two holdings of the quote asset price without any network (1.0
        // each), and each row's value is finite while their sum is not. The
        // per-row check alone would pass this straight into the history.
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

    /// **The change column is blank wherever two adjacent points are not
    /// known to share a unit.**
    ///
    /// That covers more than a currency change, and the assertions below name
    /// each case: the first point has nothing before it, and a point that
    /// records no currency is comparable to nothing at all.
    ///
    /// Two units in one series is not hypothetical: an operator who changes
    /// `quote` from `USD` to `CNY` writes exactly this document. Subtracting
    /// across the boundary would draw a jump of about six times the
    /// portfolio's value — a move it never made — and the same reasoning
    /// applies to a point that records no unit at all, which is every point
    /// written before this slice.
    #[test]
    fn the_change_column_breaks_wherever_two_points_are_in_different_units() {
        let points = vec![
            // Written before this slice: unit not recorded, not recoverable.
            json!({ "at": "t1", "total": 100.0 }),
            json!({ "at": "t2", "total": 110.0 }),
            // The install starts settling in USD.
            json!({ "at": "t3", "total": 120.0, "currency": "USD" }),
            json!({ "at": "t4", "total": 130.0, "currency": "USD" }),
            // …and is reconfigured to CNY.
            json!({ "at": "t5", "total": 900.0, "currency": "CNY" }),
            json!({ "at": "t6", "total": 910.0, "currency": "CNY" }),
        ];
        let table = history_table(&points);
        let rows = table["rows"].as_array().expect("rows");
        // Newest first, so this is t6 … t1.
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
