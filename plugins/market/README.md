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
`HK:1810`, `SH:600519`, `SZ:000001` — which is what makes `W` on two venues two
holdings rather than one. Every venue has a price source, and each source
quotes in its own currency. Not every symbol on a venue is priced: a code whose
quote currency the code itself does not fix is refused rather than guessed at.
See [Assets and sources](#assets-and-sources).

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
`<VENUE>:<SYMBOL>`, over five venues: `CRYPTO`, `US`, `HK`, `SH` (Shanghai) and
`SZ` (Shenzhen). Names are
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
| `SH` | Sina `hq.sinajs.cn` | `sh<6-digit code>` | CNY |
| `SZ` | Sina `hq.sinajs.cn` | `sz<6-digit code>` | CNY |

An identity is never handed to the other venue's source: routing `US:BTC` to
Binance would answer with bitcoin's price attached to a US listing, which is
the fabricated number this whole identity layer exists to prevent.

Shanghai and Shenzhen are two venues rather than one `CN`, because a six-digit
code does not say which exchange lists it and no rule keyed on the digits
survives contact with real tickers. A single `CN` venue had to ask both and
take whichever answered, and that is unsound the moment only one of them
answers: `000001` is the Shanghai Composite index at 3933 on `sh` and Ping An
Bank at 11.87 on `sz`, so a day when either is halted — this source answers a
halted security with a row of zeros, which is filtered out — leaves exactly one
answer and the wrong security accepted in silence. The caller names the
exchange instead.

#### Which currency a stock price is in

**Neither source states one.** A Sina row is a comma-separated list of numbers
with no unit anywhere on it, and Binance's `/ticker/price` answers a bare
number for a pair whose quote leg this plugin pinned itself. So the currency
attached to every published price is this plugin's own determination.

Venue alone is not enough to make it, and getting it wrong is invisible to the
cross-currency check below, because it happens *inside* one venue — every row
still says `CNY`, so a total is stated and written to history. Three real
counterexamples, read off the live endpoint on 2026-09-07, each listed on
exactly one exchange:

| code | what it is | quoted in |
| --- | --- | --- |
| `SH:900932` | 陆家Ｂ股 0.385, a Shanghai B share | **USD**, not CNY |
| `SZ:200725` | 京东方Ｂ 4.770, a Shenzhen B share | **HKD**, not CNY |
| `HK:89988` | 阿里巴巴－ＷＲ 94.45, a renminbi counter | **CNY**, not HKD |

So the code ranges below are an allowlist — a code is priced only where the
range itself fixes the currency — and everything else is refused out loud:

| venue | priced | refused, with the reason said out loud |
| --- | --- | --- |
| `US` | any `gb_` symbol, in USD | — |
| `HK` | one to five digits, padded to five, below `80000`, in HKD | `8xxxx` (renminbi counters, verified on `hk89988`) and `9xxxx` (refused as the conservative side of the same boundary) |
| `SH` | `6xxxxx` (A shares and STAR), in CNY | `9xxxxx` B shares, and the fund, bond and index ranges |
| `SZ` | `00xxxx` (main board) and `30xxxx` (ChiNext), in CNY | `2xxxxx` B shares, and the fund, bond and index ranges |

The holdings table carries the venue as its own column rather than glued onto
the name, so `US:W` and `CRYPTO:W` read as two distinguishable rows;
`market.holdings.list`'s one-line prose, which has no columns, writes them out
as `US:W` and `CRYPTO:W`.

### Currencies, and why a total sometimes goes missing

**Every price carries the currency this plugin determined it is in**, and that
currency travels with the number to `market.quote`, to the holdings table and
to `market.holdings.list`. It stops there: a stored history point is
`{at, total}` and records no unit, so the history table's total column carries
none. Nothing is converted either: this plugin holds no exchange rates.

So a portfolio whose priced holdings are all in one currency gets a total in
that currency, as it always did — but one holding `BTC` (USDT) alongside
`US:NVDA` (USD) gets **no total at all**. Adding 1 to 230.36 there would
produce a figure in no currency, published as the portfolio's value. The
per-asset rows still go out, each with its own currency, which is everything a
reader can actually use.

The consequence worth knowing: such a portfolio also contributes **no history
points** for as long as it spans currencies, because a history point is a total.
Its series stands still, exactly as a portfolio with an unpriceable holding
does. It resumes as soon as the priced holdings are back in one currency — by
selling or by removing the odd holding — and would resume for a mixed portfolio
too once currency conversion lands.

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
| `sina_endpoint` | `https://hq.sinajs.cn` | the US/HK/SH/SZ quote-list base URL |

`quote` is **not** a pricing input. Each market is quoted in its own currency
and nothing converts between them yet, so today this key changes none of the
published numbers; it is what a later slice will convert totals into. It used
to do three jobs at once — display unit, Binance's quote leg, and "this asset
is the unit, worth 1" — and the last two now live inside the Binance source
where they belong. Setting `quote` to `CNY` no longer sends `BTCCNY` to
Binance: that pair does not exist (`{"code":-1121,"msg":"Invalid symbol."}`),
so a `BTC` holding on a CNY-settling install used to go unpriced — and with it
the whole Track's history series.

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
* **A code whose quote currency the code does not fix is refused, not priced.**
  Shanghai and Shenzhen B shares, and Hong Kong's renminbi and US-dollar
  counters, cannot be held here yet; nor can the mainland fund, bond and index
  ranges. The refusal names the reason. Reading the counter currency off the
  source is not possible — it does not publish one — so closing this means a
  second source or a checked-in table, which is not done.
* **`CN:` is no longer a venue.** A holding recorded as `CN:600519` before the
  Shanghai/Shenzhen split no longer parses, and the read path drops rows it
  cannot parse and rewrites the document without them on the next `set`. Such a
  holding has to be recorded again as `SH:600519` or `SZ:000001`.
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
