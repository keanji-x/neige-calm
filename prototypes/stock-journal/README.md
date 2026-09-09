# Native Neige portfolio / Market data viewer

Based on main `b7bdad97c`. This viewer reuses the actual Neige router, session/login
gate, Track page, Report renderer, Markdown tables, outline, backlinks, and file
reader. It contains exactly three sections: portfolio charts, holdings, and trade
history. Charts use shadcn/ui + Recharts with transparent backgrounds. No Barra or
factor exposure is included.

## Run

Install the repository frontend dependencies (`cd fe && npm ci`), then:

```sh
cd prototypes/stock-journal
npm ci
npm run dev
```

Open http://127.0.0.1:5194/next/track/portfolio for the example. Choose
**我的 Market data 组合** to use Neige's normal login and select a portfolio Track
in the existing sidebar. A direct live-mode URL is
`/next/track/<actual-track-id>?market=1`.

`connection.json` explicitly selects the Neige backend (default
`http://127.0.0.1:4040`). Development and static preview proxy `/api` to that
backend. No provider credential is copied or requested; this uses Neige's normal
session. Changing that backend requires restarting the preview server.

## Actual market integration

`src/market-adapter.ts` reads the current `dev-neige-market` outputs from the
selected Track's authenticated detail response:

- `portfolio.holdings`: asset, venue, quantity, native price/currency, applied
  rate, converted value, Total row, and conversion disclosures.
- `portfolio.history`: dated totals, with the currency of each individual point.

Producer-reported converted values and the total remain authoritative. Missing
rows/quotes/rates never become complete weights; historical currency boundaries
remain gaps. An overlay older than two minutes is labeled stale; its timestamp is
collection time, not exchange execution time. Current main does not publish daily
changes in these outputs, so that column displays an em dash rather than guessing.

The first selected portfolio (or the Track explicitly named in a live-mode URL)
is remembered for this view. Other research Tracks retain their original Reports,
even if they also carry market overlays. Click the Market data entry again to
choose another portfolio. Authentication changes clear the selection. Saved backend Reports are never overwritten. The only forwarded
writes are native login/logout; registering holdings and editing records stay in
Neige's original workflow. This viewer contains no brokerage order integration.

Active Track details refresh every 30 seconds. Only a validated snapshot for the
active Track is sent to its exact chart iframe. A different frame, different Track,
or impersonated request cannot retrieve cached snapshots. Authentication changes
clear caches and fence in-flight reads. The iframe remains opaque-origin and gets
no cookie, token, API access, or write command.

## Research links, next events and transaction records

Create `.neige-portfolio/metadata.json` in the **portfolio Track's own workspace**
when you want to add research links, events or executed trade records. It is read
through Neige's authenticated workspace-file endpoint and is never bundled into
public frontend assets. For example:

```json
{
  "assets": {
    "SH:600519": {
      "name": "贵州茅台",
      "trackId": "your-research-track-id",
      "nextEvent": { "date": "2026-10-30", "title": "季度复盘" }
    }
  },
  "trades": []
}
```

The initial file is optional. Only the backend's exact missing-file response is
treated as absent; permission failures, deleted Tracks, truncation and malformed
JSON remain errors. Use the normal Neige workspace editing workflow to maintain
this file. An example is provided under `examples/metadata.json`.

A trade record requires `id`, `symbol`, `name`, `date`, nullable `trackId`,
`side` (`buy`/`sell`), `quantity`, `price`, `currency`, `fee`, and `reason`.
These are already-executed trades, not order instructions. Missing metadata stays
empty; the viewer never fabricates transaction history from holding changes.
No separate bank cash balance is added to plugin totals: only registered assets
are represented. Demo data remains separately marked and never fills live errors.

## Verification

```sh
npm run test:model
npm run test:market
npm run build
npm test
node node_modules/@playwright/test/cli.js test --config=playwright-built.config.ts
```

The market contract tests cover producer values, partial totals, currency gaps,
identity, staleness and invalid input. Native browser checks exercise login before
reads, plugin-shaped live responses, the iframe handoff, original research Report
preservation, interactions and narrow layouts. The message-source assertion was
mutation-verified in this isolated worktree: removing its production guard caused
exactly the intended live browser test to fail; restoring it passed.

For static viewing, build first then run:

```sh
node node_modules/vite/bin/vite.js preview --host 127.0.0.1 --port 5193 --strictPort
```

`chart-bundle.ts` emits a local classic IIFE and keeps the sandbox intact. It omits
Vite's generated stylesheet `crossorigin` attribute only in the figure HTML, so
production CSS loads correctly from the opaque-origin frame. The main application
HTML is unchanged. shadcn's source and MIT license are retained under `src/charts/`.
