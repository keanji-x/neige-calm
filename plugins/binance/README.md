# Binance portfolio plugin

Prices the holdings you configure and pushes the result at a Track, on its own
clock. A report block names the result; the plugin keeps moving it.

## What it writes

Two overlays on the configured Track, both shaped as report `table` block
payloads:

| overlay kind | contents |
| --- | --- |
| `portfolio.holdings` | one row per asset — quantity, price, value — plus a `Total` row |
| `portfolio.history` | the total over time, newest first, with the change against the previous point |

## Putting them in a report

````markdown
## Portfolio

```neige-block table
{ "source": "neige://plugin/dev-neige-binance/portfolio.holdings" }
```

## Since I started watching

```neige-block table
{ "source": "neige://plugin/dev-neige-binance/portfolio.history" }
```
````

The block holds the reference, not the numbers. Every push re-renders it
without touching the document, so the report keeps one revision while the value
moves. A block written before the plugin is installed renders as
"waiting for …" rather than an error — install order is yours to choose.

## Configuring it

Settings → the plugin → Configure:

| key | default | meaning |
| --- | --- | --- |
| `track_id` | *(required)* | the Track the overlays are pushed at |
| `holdings` | `BTC:100` | comma-separated `ASSET:QUANTITY` pairs, e.g. `BTC:100,ETH:2.5` |
| `quote` | `USDT` | the asset everything is priced in |
| `poll_seconds` | `30` | seconds between refreshes (floored at 5) |
| `endpoint` | `https://data-api.binance.vision` | market-data base URL |

Configuration is read at handshake, so a change takes effect when the plugin is
restarted.

### Why not `api.binance.com`

It answers `{"code":0,"msg":"Service unavailable from a restricted location…"}`
— HTTP 200 with an error body — from a good many hosts, this project's deploy
host included. `data-api.binance.vision` serves the identical `/api/v3` paths
with no key, no geo gate, and over a direct connection, which matters because
the systemd unit carries no proxy variables. Point `endpoint` at
`https://api.binance.com` if your host is eligible; nothing else changes.

## Limits worth knowing

* **History is forward-only.** It starts empty and grows one point per
  successful refresh. Nothing is back-filled, so the series says "since this
  plugin started watching", not "since you bought".
* **Only a fully-priced tick contributes a history point.** History is a claim
  about the *portfolio's* value over time, so a total covering a subset —
  plotted against totals covering the whole — would draw a crash that never
  happened, and state it as a number in the `Change` column. The holdings
  table still goes out either way: it names the missing prices row by row,
  which is the honest form of the same information.
* **A point that could not be stored is not displayed.** Publishing it would
  put a value on screen that the next tick, which reloads from the store,
  silently deletes.
* **`binance.portfolio.refresh` reports a partial refresh as an error**, not as
  success with a caveat — the caller's next act is to read a number off the
  table.
* **At most 500 points are kept**, oldest dropped first, well inside the KV
  quota the manifest asks for.
* **Public market data only.** No API key, no account access, no order
  placement — the plugin reads prices and nothing else. Your quantities live in
  its configuration; they are never sent anywhere.
