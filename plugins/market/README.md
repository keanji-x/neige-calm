# Market data plugin

Prices what a Track says it holds, on its own clock, and pushes the result at
that Track.

You tell the Planner what you hold. It calls this plugin. Nothing goes in
Settings, nothing needs a restart, and no report is rewritten when the price
moves.

## Using it

> **"I have 100 BTC"**

The Planner calls `market.holdings.set { asset: "BTC", quantity: 100 }`. The
holding is stored against **that Track**, priced immediately, and published.
Say "I sold 40" and it calls the same tool with `60`; say "I sold it all" and
it calls it with `0`, which removes the holding.

An asset may also be named with its venue — `CRYPTO:BTC`, `US:NVDA`,
`HK:1810`, `CN:600519` — which is what makes `W` on two venues two holdings
rather than one. All four venues are priced, each in its own currency; see
[Assets and sources](#assets-and-sources).

| tool | what it does |
| --- | --- |
| `market.holdings.set` | record a quantity for one asset (`0` removes it) and wake a refresh |
| `market.holdings.list` | what this Track holds, with current price and value |
| `market.quote` | the price of one asset, without touching any holding |

### Why `set` does not price

Because a tool that prices touches the network, and a tool that touches the
network must declare `openWorldHint: true` — which is exactly what makes codex
demand approval for it. The kernel spawns every agent with
`approval_policy: "never"`, so such a tool is not "gated", it is **unusable**.
We found this by running it: the Planner produced a perfectly-formed
`market.holdings.set { asset: "BTC", quantity: 100 }` and got back *"MCP tool
call requires approval, but approval policy is never"*.

The fix is not to relabel a tool that does touch the open world. `set` records
state and **wakes** the poll thread, which does the pricing a moment later. The
annotation stays honest, and the reader still sees a fresh table within a
second or two rather than at the next interval. A test in the plugin refuses
any future write tool that declares otherwise.

**Which Track a call acts on comes from the kernel**, in
`params._meta["dev.neige/track"]`, filled from the identity it resolved for the
caller — nothing in the request body reaches it, so an agent cannot aim a
holding at another Track by writing one into its arguments. (The value follows
the *identity*: a caller trusted with the daemon token picks its identity by
naming a session, and so can pick the Track. That is what daemon trust means
everywhere in this transport, not something this plugin changes.)

A call that carries no Track is refused rather than defaulted — acting on some
other Track's portfolio is exactly the failure worth preventing. Two Tracks
therefore keep two independent portfolios.

## What it writes

Two overlays per Track, both shaped as report `table` block payloads:

| overlay kind | contents |
| --- | --- |
| `portfolio.holdings` | one row per asset — quantity, price, value, currency — plus a `Total` row |
| `portfolio.history` | the total over time, newest first, with the change against the previous point |

Name them from a report and the numbers keep moving under a document that does
not change:

````markdown
```neige-block table
{ "source": "neige://plugin/dev-neige-market/portfolio.holdings" }
```

```neige-block table
{ "source": "neige://plugin/dev-neige-market/portfolio.history" }
```
````

The same two blocks work in any Track — the overlay is per Track, the `source`
is not. A block written before the plugin is installed renders as "waiting
for …" rather than an error.

## Assets and sources

An asset is named either bare (`BTC`) or **venue-qualified**,
`<VENUE>:<SYMBOL>`, over four venues: `CRYPTO`, `US`, `HK`, `CN`. Names are
trimmed and upper-cased, so `crypto:btc` and `CRYPTO:BTC` are one identity, and
a prefix counts as a prefix only when the colon is there — `USNVDA` is a crypto
name, not NVDA.

A bare name is the crypto venue, and that is frozen rather than configurable:
every row a pre-venues `market.holdings.set` wrote is a bare name, so a knob
that reinterpreted them would silently reprice an existing portfolio the moment
an operator flipped it.

The venue is written down rather than guessed, because a name alone does not
identify a security — `W` is Wayfair on the NYSE and Wormhole in crypto — and
every rule proposed for inferring one from a name's shape ("six digits means
Shanghai") has counterexamples among real tickers. A wrong guess here is a
silently wrong number in a total, not a visible failure.

Which source answers a venue, at which URL, in which response shape, is this
plugin's business. There are two:

| venue | source | asked for | quoted in |
| --- | --- | --- | --- |
| `CRYPTO` | Binance spot | `<SYMBOL>USDT` | USDT |
| `US` | Sina `hq.sinajs.cn` | `gb_<symbol>` | USD |
| `HK` | Sina `hq.sinajs.cn` | `hk<code padded to 5 digits>` | HKD |
| `CN` | Sina `hq.sinajs.cn` | `sh<code>` **and** `sz<code>` | CNY |

An identity is never handed to the other venue's source: routing `US:BTC` to
Binance would answer with bitcoin's price attached to a US listing, which is
the fabricated number this whole identity layer exists to prevent.

A `CN` code does not say which exchange lists it, and no rule keyed on the
digits survives contact with real tickers, so both candidates go out in one
request and the one that answers is the one taken. A code **both** exchanges
answer is refused rather than picked between — `CN:000001` is the Shanghai
Composite index on `sh` and Ping An Bank on `sz`, a factor-of-300 apart. Such
a code cannot be priced until the grammar grows separate `SH` and `SZ` venues.

The holdings table carries the venue as its own column rather than glued onto
the name, so `US:W` and `CRYPTO:W` read as two distinguishable rows;
`market.holdings.list`'s one-line prose, which has no columns, writes them out
as `US:W` and `CRYPTO:W`.

### Currencies, and why a total sometimes goes missing

**Every price carries the currency its own market quotes in**, and that
currency travels with the number to the table, to the history and to
`market.holdings.list`. Nothing is converted: this plugin holds no exchange
rates.

So a portfolio whose priced holdings are all in one currency gets a total in
that currency, as it always did — but one holding `BTC` (USDT) alongside
`US:NVDA` (USD) gets **no total at all**. Adding 1 to 230.36 there would
produce a figure in no currency, published as the portfolio's value. The
per-asset rows still go out, each with its own currency, which is everything a
reader can actually use.

The consequence worth knowing: such a portfolio also contributes **no history
points** for as long as it spans currencies, because a history point is a total.
Its series stands still, exactly as a portfolio with an unpriceable holding
does, until it is back in one currency or currency conversion lands.

### Why the `Referer` header

`hq.sinajs.cn` answers `HTTP 403` with the body `Forbidden` to any request that
omits `Referer: https://finance.sina.com.cn`. Its responses are also **GBK**,
not UTF-8; only the company-name fields are affected and this plugin reads none
of them.

### Why `data-api.binance.vision`

`api.binance.com` answers `{"code":0,"msg":"Service unavailable from a
restricted location…"}` — HTTP 200 with an error body — from a good many hosts,
this project's deploy host included. `data-api.binance.vision` serves the
identical `/api/v3` paths with no key, no geo gate, and over a direct
connection, which matters because the systemd unit carries no proxy variables.
Point `binance_endpoint` at the main API if your host is eligible.

## Settings

Four keys, all optional, all with working defaults — an unconfigured install
is a working install.

| key | default | meaning |
| --- | --- | --- |
| `quote` | `USDT` | the settlement currency a total is meant to be stated in |
| `poll_seconds` | `30` | seconds between refreshes (floored at 5) |
| `binance_endpoint` | `https://data-api.binance.vision` | the Binance market-data base URL |
| `sina_endpoint` | `https://hq.sinajs.cn` | the US/HK/CN quote-list base URL |

`quote` is **not** a pricing input. Each market is quoted in its own currency
and nothing converts between them yet, so today this key changes none of the
published numbers; it is what a later slice will convert totals into. It used
to do three jobs at once — display unit, Binance's quote leg, and "this asset
is the unit, worth 1" — and the last two now live inside the Binance source
where they belong. Setting `quote` to `CNY` no longer sends `BTCCNY` to
Binance, a pair that does not exist and that used to leave every crypto row
unpriced.

Configuration is read at handshake, so a change takes effect when the plugin is
restarted; a re-handshake replaces it for the running poll thread too, rather
than starting a second one. Holdings are not configuration and are never
affected by this.

## Limits worth knowing

* **History is forward-only.** It starts empty and grows one point per
  successful refresh; nothing is back-filled. The series says "since this
  plugin started watching", not "since you bought".
* **Only a fully-priced tick contributes a history point.** History is a claim
  about the *portfolio's* value over time, so a total covering a subset —
  plotted against totals covering the whole — would draw a crash that never
  happened. The holdings table still goes out either way: it names the missing
  prices row by row, which is the honest form of the same information.
* **A total is only stated when the priced holdings share one currency**, and
  a tick without a total contributes no history point. See *Currencies* above.
* **Stored history points do not record their currency**, so the history
  table's total column carries no unit. A series written before and after a
  portfolio changed the currency it totals in is two series plotted as one;
  recording the currency per point, and breaking the series where it changes,
  is not done yet.
* **A point that could not be stored is not displayed**, because the next tick
  reloads from the store and would silently delete it.
* **At most 500 points per Track**, oldest dropped first.
* **Public market data only.** No API key, no account access, no orders. Your
  quantities live in this plugin's own storage and are never sent anywhere.
