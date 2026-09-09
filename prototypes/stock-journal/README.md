# Native Recipe preview

The portfolio is the canonical Recipe at
[`fe/web/src/features/report/recipe/examples/portfolio.md`](../../fe/web/src/features/report/recipe/examples/portfolio.md).
Its headings, layout, charts, table columns, data sources and presentation are
saved configuration. The renderer lives in `fe/` and ships in the normal app.

In Neige, open **Recipes → 投资组合模板 → Save**, then select your saved Recipe
when creating a Track. Market data resolves against that new Track. Use normal
chat/Report editing to change layout, annotations and trade records. Nothing
selects a portfolio using browser storage, replaces saved Reports, or blocks
normal authoring requests.

The template starts empty and defaults to CNY. If the plugin settles in another
currency, update the chart `unit.equals` settings; unavailable or inconsistent
units remain visible errors. Missing daily change stays a dash. Annotations can
add research Track links and next events without overriding producer fields.
Only user-supplied executed transactions belong in the trade log.

## Preview

Install `fe` dependencies first, then in this directory:

```sh
npm ci
npm run build
node node_modules/vite/bin/vite.js preview --host 127.0.0.1 --port 5193 --strictPort
```

`/next/track/portfolio?market=0` uses visibly fictitious records and the same
native layout renderer. Only this demo transport is read-only.
`/next/?market=1` uses ordinary authenticated Neige API transport, with the
backend configured in `connection.json`. It does not synthesize a portfolio
Report: create a real Track from your saved Recipe in that mode.

## Checks

```sh
npm test
node node_modules/@playwright/test/cli.js test --config=playwright-built.config.ts
# Build fe first. This tests the actual fe production entry, not this preview:
node node_modules/@playwright/test/cli.js test --config=playwright-native.config.ts
```

These browser flows use fixtures at the API boundary and never launch an AI or
place orders. Backend integration tests separately exercise real Recipe storage,
Track instantiation, Report block writes and revision conflicts. See
[the layout contract](../../docs/report-layout-contract.md) and
[the template guide](../../docs/portfolio-template.md).
