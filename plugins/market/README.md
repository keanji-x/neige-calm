# Market data plugin

Prices what a Track says it holds, on its own clock, and pushes the result at
that Track.

You tell the current Track’s Assistant or Planner what you hold. It calls this plugin. Nothing goes in
Settings, nothing needs a restart, and no report is rewritten when the price
moves.

## Using it

> **"I have 100 BTC"**

The Assistant or Planner calls `market.holdings.set { asset: "BTC", quantity: 100 }`. The
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
| `portfolio.holdings` | one row per asset — quantity, price, the currency it is priced in, the rate applied and the value in the settlement currency — plus a `Total` row |
| `portfolio.history` | the total over time, newest first, each point with its own currency, and the change against the previous point where the two share one |

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
`SZ` (Shenzhen). (`CN:` parses too, but it names no exchange and is never
priced — see *Known limits* below.) Names are
trimmed and upper-cased, so `crypto:btc` and `CRYPTO:BTC` are one identity, and
a prefix counts as a prefix only when the colon is there — `USNVDA` is a crypto
name, not NVDA.

A bare name is the crypto venue, and that is frozen rather than configurable:
every row a pre-venues `market.holdings.set` wrote is a bare name, so a knob
that reinterpreted them would silently reprice an existing portfolio the moment
an operator flipped it.

A Hong Kong code is folded to five zero-padded digits when it is parsed —
`HK:1810`, `HK:01810` and `HK:001810` are one identity, canonically `HK:01810`
— because leading zeros are optional in every human spelling of the same
security. Two identities for one security is one holding recorded twice, in one
currency, in a total that looks entirely ordinary. Five padded digits is the
form HKEX and this source both use. Mainland codes are six digits at both
exchanges and are asked for verbatim, so they have no second spelling to fold.

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
| `HK` | Sina `hq.sinajs.cn` | `hk<5-digit code>` | HKD |
| `SH` | Sina `hq.sinajs.cn` | `sh<6-digit code>` | CNY |
| `SZ` | Sina `hq.sinajs.cn` | `sz<6-digit code>` | CNY |

An identity is never handed to the other venue's source: routing `US:BTC` to
Binance would answer with bitcoin's price attached to a US listing, which is
the fabricated number this whole identity layer exists to prevent.

Shanghai and Shenzhen are two venues rather than one `CN`, because a six-digit
code does not say which exchange lists it and no rule keyed on the digits
tells you which one does. (Digits do fix the quote CURRENCY once the exchange
is known — that is the allowlist below — but that is a different question.) A single `CN` venue had to ask both and
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
| `HK` | a five-digit code below `80000`, in HKD | `8xxxx` (renminbi counters, verified on `hk89988`) and `9xxxx` (no currency established for it either way; the two codes sampled there, `hk90988` and `hk96618`, are ones this source does not list, so on those two the refusal gives up no price — nothing is known about the rest of the range) |
| `SH` | `6xxxxx` (A shares and STAR) and `5xxxxx` (funds), in CNY | `9xxxxx` B shares, and the bond and index ranges |
| `SZ` | `00xxxx` (main board), `30xxxx` (ChiNext) and `15xxxx` / `16xxxx` (funds), in CNY | `2xxxxx` B shares, and the bond and index ranges |

The fund ranges are renminbi by the exchanges' own rules rather than by
sampling: 《上海证券交易所交易规则》and《深圳证券交易所交易规则》both state at
3.3.11 that a fund order's tick size is denominated in renminbi, so a fund
traded on either exchange quotes in renminbi whatever it holds. The codes a
counterexample would have come from behave accordingly on the live endpoint on
2026-09-07 — `SH:513500` (标普500ETF博时) 2.692, `SH:501018` (南方原油LOF)
1.922, `SZ:160216` (国泰商品) 0.652 and `SH:588000` (科创50ETF) 1.705, all in
renminbi. That the source carries the ranges at all was read off the same
endpoint: `SH:510300` (沪深300ETF华泰柏瑞) 4.635, `SH:563210` (专精特新ETF富国)
1.949, `SH:511990` (华宝添益) 99.999 and `SZ:159915` (创业板ETF) 3.338. Being in an allowed range is not a promise the
source lists the code: `SZ:162201`, a LOF, answers with an empty row and comes
back as *unknown* rather than as a currency refusal.

The holdings table carries the venue as its own column rather than glued onto
the name, so `US:W` and `CRYPTO:W` read as two distinguishable rows;
`market.holdings.list`'s one-line prose, which has no columns, writes them out
as `US:W` and `CRYPTO:W`.

### Currencies, conversion, and why a total sometimes goes missing

**Every price carries the currency this plugin determined it is in**, and that
currency travels with the number to `market.quote`, to the holdings table and
to `market.holdings.list`. `market.quote` never converts: it answers one
asset's price in one market's currency.

A **total** is a different thing, and it is converted. Each row is carried into
the settlement currency the install configures, so a Track holding `BTC`
(USDT), `US:NVDA` (USD), `HK:1810` (HKD) and `SH:600519` (CNY) gets one total
in one currency and a history point to go with it. Before conversion landed
such a Track got rows and nothing else, because adding 1 to 230.36 produces a
figure in no currency at all.

**Two currencies settle: `USD` and `CNY`.** `USDT` is accepted and settles as
USD — see below. Anything else, `HKD` included, settles nothing: each row keeps
its own market's currency, and a portfolio spanning two of them gets no total,
with the caption naming the value that was configured.

The rates come from the same source the stock prices do, `hq.sinajs.cn`, one
request per pair:

| pair | symbol | on 2026-09-07 |
| --- | --- | --- |
| USD → CNY | `fx_susdcny` | 6.7111 |
| CNY → USD | `fx_scnyusd` | 0.149007 |
| HKD → USD | `fx_shkdusd` | 0.1275526 |
| HKD → CNY | `fx_shkdcny` | 0.8560178 |

Sina quotes every ordered pair natively, so **each direction is its own
request** and no rate here is the reciprocal of another. The two directions are
not reciprocals in the data either — `1/6.7111` is 0.14900538 against the
quoted 0.149007 — because the two rows were last updated an hour apart, and
dividing into the wrong one would publish a number no source stated.

A row already in the settlement currency is not converted and asks for nothing:
an all-crypto portfolio settling in the default still prices with the rate
source unreachable.

**Every conversion is stated where its result is.** The holdings table carries
the rate on each row and names the pair behind it in the caption; the same
lines come back from `market.holdings.list`. A reader is never shown a
converted number without being told what it was converted at.

#### `USDT` is taken as 1 USD, and that is an assumption

It is the one number in this plugin that no source stated. What it costs was
measured on one day rather than guessed: `data-api.binance.vision` answered
`USDTUSD` at **0.99967** on 2026-09-07 (a second sample minutes later read
0.99966), so against that reading the parity overstates the **USDT-quoted part**
of a portfolio by about **3.3 basis points** — 33 USD per 100,000 USD held in
USDT-quoted assets.

Two things that reading does not establish, and this plugin does not claim.
The effect on a whole total is those 3.3 bp scaled by how much of the portfolio
is quoted in USDT, so a mostly-stock portfolio is off by far less. And one
day's sample fixes no direction: `USDTUSD` has traded above 1 as well, and on
such a day the same parity understates instead. What is fixed is that the
parity is not the market's number.

The prose exits say so in words — *"USDT taken as 1 USD — assumed, not
quoted"* — rather than printing it as a rate alongside the ones that were
fetched: the holdings table's caption, `market.holdings.list`'s text, and that
tool's `conversions` array. The per-row `rate` cell is a bare number in every
case, so a machine consumer reading only `holdings[i].rate` cannot tell an
assumed 1 from a quoted one and has to read `conversions`.

`USDT_USD_ASSUMED_PARITY` in `main.rs` is where the parity is decided, but
replacing it is not by itself enough to make it a quote: a fetched leg needs a
request, a failure that propagates instead of a value that always exists, and a
place in the per-pass cache, and the `AssumedParity` hop would have to stop
being a variant. `USDTUSD` is listed with no key; `USDUSDT` is not listed at
all, so the reverse direction would have to be a reciprocal.

**What this changes for an existing install:** the default `quote` is `USDT`
and stays `USDT`; it now settles in USD. An all-crypto portfolio's total is
therefore the **same number** it was before, labelled `USD` instead of `USDT` —
USD being the unit the rates are in. No configuration needs changing.

**When a rate does not come back**, the holdings quoted in that currency keep
their own price and currency — those are true — and get no converted value, no
place in the total, and no history point for that tick. Nothing falls back to a
rate from an earlier pass: a rate this plugin could not read this pass is a
rate it does not have. Its series stands still, exactly as a portfolio with an
unpriceable holding does, and resumes on the next pass that reads the rate.

**An empty `Total` cell means "unknown"; a `0` never does.** A portfolio that
holds something, none of which could be priced or converted, shows an empty
cell, because its value is unknown rather than nil and a `0` there would read
as *this portfolio is worth nothing*.

A `0` in that cell is arithmetic, and more than one thing produces it: a Track
that holds nothing — the only case whose caption says the total is 0 — and
equally a real total too small to survive rounding to two decimals, such as
0.001 USDT, or a nanogram-sized crypto position settled into USD.

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
| `quote` | `USDT` | the currency a total is stated in — `USD` or `CNY`, with `USDT` settling as `USD` |
| `poll_seconds` | `30` | seconds between refreshes (floored at 5) |
| `binance_endpoint` | `https://data-api.binance.vision` | the Binance market-data base URL |
| `sina_endpoint` | `https://hq.sinajs.cn` | the US/HK/SH/SZ quote-list base URL |

`quote` is **not** a pricing input: each market is quoted in its own currency
and that is what every price cell says. It is the unit **totals** are stated
in, and the only values that settle are `USD` and `CNY` (`USDT` settles as
`USD`). It used to do three jobs at once — display unit, Binance's quote leg,
and "this asset is the unit, worth 1" — and the last two now live inside the
Binance source where they belong. Setting `quote` to `CNY` does not send
`BTCCNY` to Binance: that pair does not exist
(`{"code":-1121,"msg":"Invalid symbol."}`), so a `BTC` holding on a CNY-settling
install used to go unpriced — and with it the whole Track's history series. It
is now priced in USDT and converted.

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
* **Only a tick where every holding priced and converted contributes a history
  point.** The published total is more forgiving than that: when some holdings
  could not be valued the tables and `market.holdings.list` state a *partial*
  total over the ones that could, and say what it covers. A partial total is
  never written to history. See *Currencies* above.
* **`USDT` is taken as 1 USD.** An assumption, not a quote: the pair answered
  0.99967 on 2026-09-07, so on that day's reading the USDT-quoted part of a
  portfolio is about 3.3 basis points high, and a whole total is off by that
  scaled to its USDT weight. One day's sample fixes no direction. Every
  published conversion says in prose that the step is assumed; the per-row
  `rate` cell does not.
* **Exchange rates come from Sina only.** The stock prices have no second
  source today either, but if one is added the fallback will be **partial**: a
  pass could price every stock through the fallback and still convert nothing,
  because `hq.sinajs.cn` is the only source of rates here. Such a pass has rows
  and no total, not a total assembled at a guessed rate.
* **Only `USD` and `CNY` settle.** `HKD` is priced but not settled in, and any
  unrecognised `quote` settles nothing: rows keep their own currencies and a
  portfolio spanning two gets no total. The caption names the configured value.
* **A code whose quote currency the code does not fix is refused, not priced.**
  Shanghai and Shenzhen B shares, and Hong Kong's renminbi and US-dollar
  counters, cannot be held here yet; nor can the mainland bond and index
  ranges. The refusal names the reason. Reading the counter currency off the
  source is not possible — it does not publish one — so closing this means a
  second source or a checked-in table, which is not done.
* **`CN:` is a migration path, not a venue.** A holding recorded as `CN:600519`
  before the Shanghai/Shenzhen split still parses, still reads back and is
  never priced: it comes back as a failure naming `SH:600519` and `SZ:600519`
  and asking which exchange lists the code. Exactly one of the two prices it —
  `600519` is a Shanghai code, so `SZ:600519` is refused as well — and the
  holding has to be recorded again under that one. The prefix is kept
  for what deleting it would do instead — the row would stop parsing, the read
  path drops a row it cannot parse without saying so, and the next `set`
  rewrites the whole document without it. A holding that visibly cannot be
  priced is better than one that silently disappears.
* **History points written before this slice do not record their currency.**
  A stored point used to be `{at, total}`, and the unit it used — whatever the
  install settled in at that moment — was never written down anywhere, so it
  cannot be recovered. Those points keep their number, show an empty currency
  cell, and have **no change computed on either side of them**: subtracting
  across an unknown unit would draw a move the portfolio never made. New points
  each record their own currency, and the change column is likewise blank
  wherever two adjacent points are in different currencies.
* **A point that could not be stored is not displayed**, because the next tick
  reloads from the store and would silently delete it.
* **At most 500 points per Track**, oldest dropped first.
* **Public market data only.** No API key, no account access, no orders. Your
  quantities live in this plugin's own storage and are never sent anywhere.
