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

| tool | what it does |
| --- | --- |
| `market.holdings.set` | record a quantity for one asset and re-price now (`0` removes it) |
| `market.holdings.list` | what this Track holds, with current price and value |
| `market.quote` | the price of one asset, without touching any holding |
| `market.refresh` | re-price this Track now instead of waiting for the next tick |

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

Callers name an **asset** (`BTC`). Which venue answers, at which URL, in which
response shape, is this plugin's business — that indirection is the whole
point. Today there is one source, Binance spot, and one resolution rule:
`<ASSET><QUOTE>` is a spot symbol. An asset no source knows is reported as
unknown, distinctly from a lookup that failed.

Adding a source later (Google Finance for US equities, say) changes nothing
outside this plugin: no agent, no report, and no stored holding. A name that
used to resolve nowhere starts resolving.

There is deliberately **no namespace syntax** (`crypto:BTC`), no per-source
priority list, and no routing configuration. Nothing exists yet for such a
scheme to disambiguate, and a guess made now is a guess we would have to keep.

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
restarted. Holdings are not configuration and are never affected by this.

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
