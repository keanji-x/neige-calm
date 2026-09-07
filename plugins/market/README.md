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
rather than one. Only the crypto venue has a price source today; see
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
| `portfolio.holdings` | one row per asset — quantity, price, value — plus a `Total` row |
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
plugin's business. Today there is **one source, Binance spot, serving `CRYPTO`
only**, with one resolution rule: `<SYMBOL><QUOTE>` is a spot symbol.

> **`US`, `HK` and `CN` names can be recorded, but nothing prices them yet.**
> An identity on one of those venues is reported as unknown — distinctly from a
> lookup that failed — so it keeps a row with a null price, and, because only a
> fully-priced tick contributes a history point (see *Limits* below), a Track
> holding one adds no history points for as long as it holds it. That is a
> stall this slice makes reachable, not one it invents: a crypto name the venue
> does not list has always come back the same way.

Adding a source for those venues later changes nothing outside this plugin: an
identity already qualified with its venue does not have to be renamed. A name
that used to resolve nowhere starts resolving.

The holdings table carries the venue as its own column rather than glued onto
the name, so `US:W` and `CRYPTO:W` read as two distinguishable rows;
`market.holdings.list`'s one-line prose, which has no columns, writes them out
as `US:W` and `CRYPTO:W`.

### Why `data-api.binance.vision`

`api.binance.com` answers `{"code":0,"msg":"Service unavailable from a
restricted location…"}` — HTTP 200 with an error body — from a good many hosts,
this project's deploy host included. `data-api.binance.vision` serves the
identical `/api/v3` paths with no key, no geo gate, and over a direct
connection, which matters because the systemd unit carries no proxy variables.
Point `binance_endpoint` at the main API if your host is eligible.

## Settings

Three keys, all optional, all with working defaults — an unconfigured install
is a working install.

| key | default | meaning |
| --- | --- | --- |
| `quote` | `USDT` | the asset everything is priced in |
| `poll_seconds` | `30` | seconds between refreshes (floored at 5) |
| `binance_endpoint` | `https://data-api.binance.vision` | the Binance market-data base URL |

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
* **A point that could not be stored is not displayed**, because the next tick
  reloads from the store and would silently delete it.
* **At most 500 points per Track**, oldest dropped first.
* **Public market data only.** No API key, no account access, no orders. Your
  quantities live in this plugin's own storage and are never sent anywhere.
