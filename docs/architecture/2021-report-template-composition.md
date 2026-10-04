# Report template composition: the template places, the plugin publishes data units

Baseline: `origin/main` 54b79918c. Every `file:line` below was read on that tree.
Status: design, revision 4 (review rounds 1-2 and owner decisions folded in, §7). No code
changes in this PR. Owner decisions (2026-10-04, final): no kernel guard for template
views; live `table` and `chart.series` stay single-block references; the supervised
paper profile is deleted (slice S0). No owner question remains open.

## 1. Problem, goals, non-goals

**Owner decision.** Report layout belongs to the report template (a Track recipe or a
builtin template body). Plugins own data and business meaning: calculations, labels,
units, tones, empty states and evidence. The template decides which data sits under
which heading, in which row and whether side by side.

**Today the publisher composes.** `view.live` is only `{source, version}`
(`crates/calm-types/src/report_blocks/kinds.rs:510-524`); its overlay is a whole
`NativeView` (title, description, snapshot, up to six rows with layouts and cells,
`native_view/model.rs:8-43`). The SPY profile publishes `spy.overview` with three rows,
their titles and layouts (`plugins/paper-trading/paper_trading/allocation_views.py:93,125,160-162`);
`spy-recipe.md:29-33` embeds it under one H1. The template cannot split it, reorder it or
put a research section between its rows. The supervised profile does the same with seven
views (`paper_trading/report_views.py:196-219`, `recipe.md:83-117`); S0 deletes that
profile (§3.3), so only the SPY profile is migrated.

Further costs of the current shape:

- One bad cell blanks the whole view: kernel hydration validates the whole overlay and
  returns one `unavailable` (`mcp_server/tools/track_report_hydrate.rs:193-202`); the
  frontend does the same (`fe/web/src/features/report/native/live.tsx:15-18`).
- Two composition mechanisms with one grammar: inline `view` and `view.live`
  (`docs/report-live-views.md`), plus SPY detail tables that restate the overview
  (`spy.portfolio`, `spy.decisions`, `spy.fills`,
  `paper_trading/allocation_report.py:17-33`).

**Goals.**

1. The template owns headings, rows, layouts and slot order. A plugin publishes data
   units, each exactly one cell, with its labels, units, tones and empty text.
2. One **composition** mechanism: the inline `view` block. `view.live` is removed.
   Single-block live references that compose nothing, the live `table`
   (`kinds.rs:494-508`) and `chart.series` (`report_blocks/chart_series.rs:126-139`), stay
   as they are and are not folded into live slots (owner decision).
3. Each live cell resolves, validates and degrades independently, with a visible
   placeholder in its own slot.
4. No plugin identity in kernel or frontend: resolution stays the generic exact
   `(track, plugin, kind)` overlay lookup (`track_report_hydrate.rs:121-149`,
   `fe/core/domain/track.ts:107-128`).

**Non-goals.** No change to inline cell kinds or their limits. No field-level data
binding or query language. No new overlay transport, write-time overlay validation or
manifest declaration of units. No platform staleness thresholds. No publish-throttling
change (the paper runtime republishes every tick, `paper_trading/runtime.py:20-24`;
reducing that churn is #1995, a separate follow-up).

## 2. Contract

### 2.1 Options

| Option | Template owns layout | Side by side | Mechanisms | Verdict |
|---|---|---|---|---|
| (a) Inline `view` whose cells may be live slots resolved per cell; delete `view.live` | yes: rows, layouts, slot order, row titles | yes, all five layouts (`model.rs:44-54`) | one | **chosen** |
| (b) Single-cell overlays placed by per-cell `view.live`/`cell.live` blocks | headings only | no: blocks flow vertically; the owner-approved two/three-column layouts (`docs/design-native-dashboard-reading.md:10-14`) are lost | two (`view` plus cell blocks) | rejected |
| (c1) Keep `view.live`, add template row/cell selection (`{source, pick}`) | partially: can drop, not regroup or interleave | only the publisher's own rows | two | rejected |
| (c2) Template components with field bindings (template writes labels, binds numbers) | yes | yes | one | rejected: labels, units and tones are plugin meaning; needs a binding language |
| (c3) Layout block that arranges other blocks by id | yes | yes | two | rejected: block ids are minted per Track at instantiation, so a template cannot name them (`routes/track_recipes.rs:37-38,58`) |

(a) reuses the existing grammar, renderer, inspection state and layouts. The only new
ideas are the slot reference and the per-cell envelope.

### 2.2 Types (calm-types, `native_view/model.rs`)

```rust
pub struct DataUnit { version: f64 /* =1 */, snapshot: Snapshot, cell: Component }
pub struct LiveSlot { kind: LiveTag /* "live" */, id: String, source: String, expects: ComponentKind }
pub enum RowCell { Live(LiveSlot), Inline(Component) }   // Row.cells: Vec<RowCell>
pub enum ComponentKind { Metrics, TimeSeries, Distribution, Table, Bars, Meter, Records } // kebab-case
```

- `RowCell` has a hand-written `Deserialize` that reads `kind` once and dispatches:
  `"live"` → `LiveSlot`, anything else → `Component`'s existing internally tagged
  deserializer. Errors therefore name the real variant ("live slot: unknown field
  `title`") instead of serde's untagged "data did not match any variant". The schema
  export emits `RowCell` as `oneOf [LiveSlot, Component]`; ts-rs emits
  `type RowCell = LiveSlot | Component`.
- `DataUnit.cell` is `Component`, not `RowCell`: a unit cannot contain a live slot, by
  type. No unit→unit indirection exists.
- `expects` is the slot's only kind field, one value with one meaning: the cell kind
  the template laid out for. (Revision 1 called it `cell`, which collided with
  `DataUnit.cell`.) `Component::kind() -> ComponentKind` is one exhaustive `match`, so a
  new component variant cannot compile without a kind.
- `Snapshot` (`model.rs:21-32`) is reused unchanged: required id, required nullable
  `observedAt`/`producedAt`. Provenance moves from the view to each unit.
- `NativeView.snapshot` becomes required nullable (`Option<Snapshot>` with
  `required_nullable`, the existing pattern at `model.rs:26-31`). It describes the inline
  cells and must be non-null exactly when the view has at least one inline cell. Every
  stored v1 payload has only inline cells and a non-null snapshot, so it stays valid;
  `version` stays `1`. Old clients are the same build and are not supported separately.
- Snapshot field checks (`native_view.rs:265-271`) move into one `validate_snapshot` that
  views and units share. `generated_schema()` (`native_view.rs:301-311`) pushes the
  `DataUnit` definitions next to `NativeView`'s, so both reach `$defs`.

### 2.3 Data unit (overlay payload)

Published per overlay kind on the Track through the unchanged `neige.overlay.set` path
(`plugin_host/callbacks.rs:192-242`):

```json
{"version": 1,
 "snapshot": {"id": "<content hash>", "observedAt": 1790798340000, "producedAt": null},
 "cell": {"kind": "metrics", "id": "nav", "title": "", "items": [ ... ]}}
```

- `cell` is validated by the existing `Component::validate` (`native_view.rs:69-255`).
- `cell.id` is **unit-local**: the publisher's identity for its cell, validated like any
  id, never compared across units. The **slot id** is the composition identity: it keys
  React elements and inspection state (`fe/web/src/features/report/native/public.tsx:12,20-27`
  key plots, distributions and records by component id today; for a live slot the key
  becomes the slot id), and it is what uniqueness across a view checks
  (`native_view.rs:274-277`).
- `version` is the envelope version, checked by readers against the one version they
  support. The template does not repeat it: one overlay key holds one payload, so a
  template version pin cannot select anything.
- A unit kind never reuses a retired overlay kind name, so each overlay key keeps one shape.
- A table unit wraps the table in a `table` component (`{kind:"table", id, title,
  table: InlineTable}`, `model.rs:303-311`); a live `table` block's overlay is a bare
  `InlineTable` (`track_report_hydrate.rs:153-179`). The two shapes are distinct on
  purpose and live under distinct overlay kinds.

### 2.4 Template reference (live slot)

```json
{"kind": "live", "id": "nav", "source": "neige://plugin/dev-neige-paper-trading/spy.nav", "expects": "metrics"}
```

- `source`: the existing two-segment overlay URI, shape-checked by `validate_live_source`
  (`kinds.rs:117-121`); existence is not checked, because an uninstalled plugin is a
  normal state. The DTO carries the same rule as a schema `pattern`
  (`^neige://plugin/[A-Za-z0-9._-]+/[A-Za-z0-9._-]+$`, the pattern `view.live` already
  publishes at `track_report_blocks/contracts.rs:455`), which the frontend generator
  compiles (`fe/tools/report-view/generate.mjs:70`), so neither side hand-writes it.
- No title: the cell's title is the unit's (publisher label). The template owns row
  titles, the view title and description, and the surrounding Markdown headings.
- A unit whose `cell.kind` differs from `expects` is `unavailable` in that slot, so a
  publisher change cannot silently break the template's layout.

### 2.5 Validation

**Write time (template structure only).** `validate_payload("view")` (`kinds.rs:84-90`)
checks rows, layout widths, slot-id uniqueness, the live slot shape, `expects`, and the
snapshot rule. The existing 256 KiB canonical cap applies to the template
(`kinds.rs:30,96-107`). It runs at every write end that already calls `validate_payload`:
block upsert, Replace, write_markdown, section replace, recipe create/update, track create
and fork (`track_report.rs:648,684`, `track_report/sections.rs:113`,
`routes/track_recipes.rs:95-108`, `routes/tracks.rs:599,614,1832`). Overlay writes stay
opaque (`callbacks.rs:210-213`, `calm-truth/src/validation.rs:489-491`).

**Read time (each live slot independently, in row and cell order).**

1. Exact overlay lookup by Track, plugin and kind. None: `pending`. Storage error: the
   whole block is `unavailable`, as today (`track_report_hydrate.rs:75-84,108`).
2. Compact UTF-8 size of the unit at most 4 MiB: today's `MAX_LIVE_VIEW_BYTES`
   (`kinds.rs:31-32`), renamed `MAX_LIVE_UNIT_BYTES`; the frontend keeps its own
   decoded-resource budget (`fe/core/domain/report-view.ts:8,11-21`), as today. The largest
   SPY unit is bounded well under the cap: `spy.decision_log` holds at most 50 decisions
   (`allocation_views.py:18,154`), each with a rationale of at most 6,000 characters
   (`allocation.py:82-83`), sources bounded to 8,000 (`allocation_views.py:143`) and at most
   20 fill disclosures (`:147`), so roughly 2.4 MB at 3 bytes per character; the
   time-series units hold at most 260 samples (`allocation.py:20`). There is no per-view
   aggregate budget: it would be one more rule duplicated in Rust and TypeScript.
3. Decode as `DataUnit` (envelope shape and version), `cell.kind == expects`,
   `Component::validate`.

A failure affects only its slot. Both sides must reject every invalid entry of the
shared fixture (`native_view_tests.rs:143-168`, `fe/tools/report-view/conformance.test.ts:65`),
so the relations the schema cannot express are written in both languages, as today: the
current whole-view `relations` (`report-view.ts:25-70`) gains the snapshot-nullability
rule, slot-id uniqueness and layout width over `RowCell`, and its per-component part is
split out and shared by inline cells and units. The slot `source` rule is generated
(§2.4), and `expects` is one comparison. New relation rules get invalid fixture entries,
so the conformance tests pin them on both sides.

**No overlay→overlay nesting during the transition (S1 to S4), by construction.**
`hydrate_live_view` (`track_report_hydrate.rs:183-208`) returns the overlay without
resolving any slot, and `live.tsx` renders `NativeReportView` without a `resolveOverlay`,
so a live slot inside a `view.live` overlay renders "this view carries no live data". No
fence code or test is added.

### 2.6 Presentation of degraded and stale cells (all new S1 behavior)

- Placeholder in the slot, keeping the row's layout: "waiting for `<source>`" (pending),
  "cannot be displayed: `<reason>`" (`role="status"`, unavailable), or "this view
  carries no live data" when no resolver is injected. These are the three states the live
  table already renders (`features/report/table/public.tsx:14-26`); S1 extracts one
  placeholder component inside `features/report` that both use.
- The slot resolver (lookup, size cap, unit decode, `expects`) is a pure function in
  `fe/core/domain` next to `trackOverlayPayload` (`fe/core/domain/track.ts:107-128`);
  `features/report/native` only renders its result.
- Empty titles. Today the header always renders `<h2>{payload.title}</h2>` and the expand
  button's accessible name is `展开 ${payload.title}` (`native/public.tsx:51-52`), and every
  row renders `<h3>{row.title}</h3>` with `aria-label={row.title}` (`:34-35`). New: an empty
  view title omits the `<h2>` and the expand button and dialog fall back to the
  accessible name "展开视图"/"视图"; an empty row title omits the `<h3>` and the
  `aria-label` (the row stays an unlabeled `section`); an empty description omits its
  `<p>` (`:33`).
- Staleness is shown, not judged. The snapshot disclosure (`native/public.tsx:40-43`)
  lists the inline snapshot (if any) and, per live slot, the unit's cell title,
  `observedAt` and `producedAt`; null reads "unknown". No time threshold: whether data
  is too old is business meaning the publisher expresses as tone or text (for SPY, the
  reconciliation-error item in `spy.account`, which replaces today's description suffix
  at `allocation_views.py:157-159`).
- No new transport: overlays already arrive with the Track detail and are invalidated per
  `overlay.set` (`fe/core/events/invalidation-plan.ts:196-204`); the injected
  `resolveOverlay` (`fe/web/src/app/router/public.tsx:2154-2157`) reaches only `table`
  and `view.live` today (`features/report/document/public.tsx:239,241`); S1 also passes it
  to `NativeReportView` for `view` blocks (`:242`).

### 2.7 Planner

- `calm.report.read` shows the template fence in `text` (about 1 KB per view) and, for a
  `view` with live slots, `resolved = {status: ok|partial, validation: "presentation",
  cells: [{id, source, status, observed_at?, resolved_at?, reason?}]}`: `ok` when every
  slot is ok, `partial` otherwise, `unavailable` on storage error. `resolve: {id: "full"}`
  adds `data` (the unit envelope) to each ok slot; `"none"` skips the overlay query
  (`track_report_hydrate.rs:66-96`). Reads never call plugins.
- The Planner does not author or rewrite template views. Enforcement stays as today:
  recipe instructions name the template-owned H1s, and routine steps use section-scoped
  replace, which validates and rewrites only its own section (`track_report/sections.rs:100-113`).
  A full `write_markdown` may still rewrite non-prose blocks (`track_report.rs:681-684`).
  There is no kernel guard for template views (owner decision): status-quo parity with
  `view.live` and every other non-prose block.
- Guidance edits: the `view` usage (`prompts/report-kinds/view.md`, returned in the
  `calm.report.blocks.kinds` result) explains live slots as template-owned references;
  `prompts/tools/calm.report.read.md` names view live slots. The Planner tool surface is
  **28,859 of 30,000 bytes** across 26 Planner-visible tools (computed with the test's
  method, description bytes plus compact input-schema bytes per descriptor,
  `mcp_server/tools/mod.rs:176-194`, from the registry golden's schemas and the prompt
  files, each description verified against the golden's `description_sha256`; re-measured
  at 54b79918c; the 29,984 in the comment at `mod.rs:170-171` predates #2017). The ~1.1 KB headroom
  absorbs the guidance edits; S4 lowers the measured number by removing `view.live` from
  the kind enum (`track_report_blocks/contracts.rs:426-436`) and the descriptions.
  S2 and S4 regenerate the registry golden (`crates/calm-server/tests/goldens/mcp_tool_registry.json`).

### 2.8 Publisher-owned meaning is preserved

A unit carries the whole `Component`, so record subtitles, badges with tones, facts,
sections and disclosures (`model.rs:148-167`), table columns and captions
(`model.rs:373-386`), metric units, signs, precision and detail captions, and empty texts
stay exactly the plugin's. The template contributes slot position, row title, layout and
a neutral view title/description; data caveats ("盈亏未扣除费用与出入金") live in the
units' details and captions, as they already do (`allocation_views.py:63-66,91`).

## 3. Migration

### 3.1 `view.live`: remove, with an explicit rewrite of 4140 (no migration code)

- **Why not a SQL migration:** report bodies are Automerge documents
  (`crates/calm-server/src/track_report_doc.rs:1-30`); the card payload is only a JSON
  projection cache of them (`track_report/write.rs:15-16`).
- **Why not a boot migrator:** permanent kernel code for a few Tracks, and the rewrite is
  a layout decision (new H1s, new slots), not a kind rename.
- **Why in place:** the SPY Track cannot be recreated: the ledger stores an immutable
  account/Track binding (`paper_trading/ledger.py:43-46`) and every SPY call and execution
  is refused for any other Track (`allocation.py:47`).
- **Why before S4:** a leftover `view.live` block makes that Track's Replace fail
  (`validate_body_fences` at `track_report.rs:648`, after which the stomp guard at `:649`
  could not drop it either), its full `write_markdown` fail (`:684`), and a fork fail
  (`routes/tracks.rs:1832`); the frontend shows it as `unsupported`
  (`fe/core/domain/report.ts:259-262`). Section replaces of other sections still work,
  because `sections.rs:113` validates only the replaced section.

**Precondition scan.** A read-only query over a **copy** of the 4140 database, so hidden
area-chat Tracks and Tracks outside visible areas are covered too:

```sql
SELECT track_id, id FROM cards WHERE kind = 'track-report' AND payload LIKE '%view.live%';
SELECT id, title FROM track_recipes WHERE body LIKE '%view.live%';
```

Run it once to size the rewrite and again immediately before deploying S4; S4 deploys
only on two empty results.

**Rewrite runbook (per affected Track, after S3 is deployed).** Script and transcript go
in the S4 PR. On the SPY Track the H1 更多明细 (`spy-recipe.md:57`) becomes 执行记录.

1. Timing: outside the SPY Calendar windows (New York time Mon-Fri 08:45-11:00 and
   16:30-17:30, Sat 10:00-11:00, `spy-recipe.md:6-9`), with the Planner idle and no
   `spy-exec-*` task queued or running (check `spy.status` and the Track's task list).
2. `GET /api/tracks/{id}/report`; save the response JSON as the backup and record
   `summary`, `docRev` and each block's `rev`.
3. `DELETE /api/tracks/{id}/report/blocks/{block}` with `ifBlockRev` for each `view.live`
   and retired live-table block.
4. Immediately after, `POST /api/tracks/{id}/report` (Replace) with `ifDocRev` = the
   post-delete revision and the step-2 `summary` (required, `routes/tracks.rs:3235-3246`).
   Build the body from the step-2 GET: new contract header and Planner comment in block 0,
   new H1s and template views, research prose unchanged, and **every remaining non-prose
   block byte-identical** in its fence text: the task blocks under 执行记录 and the
   research sections' `table` and `chart.series` blocks (关键数据). The stomp guard refuses
   any change to them (`track_report_guard.rs:61-73`) and accepts the new view fences.
5. Any non-2xx response after the first step-3 delete aborts the run. Restore from the
   step-2 backup by re-creating each deleted block with `POST
   /api/tracks/{id}/report/blocks` at its old position (`view.live` and live tables are
   still valid before S4), then re-read and restart from step 2. No retry with a stale
   revision.
6. Recipe rows from the scan: `PUT /api/track-recipes/{id}` with the new body (user actor
   only, `track_recipes.rs:121-127`).
7. Retired overlay rows (`spy.overview`, `spy.portfolio`, `spy.decisions`, `spy.fills`)
   deleted with `POST /api/overlays/delete`
   (`routes/overlays.rs:151-195`); rows are never collected otherwise
   (`calm-truth/src/db/sqlite/overlay.rs:55-68`).

**The Planner wake this causes.** User edits wake the Planner
(`PLANNER_WAKE_AUTHORS`, `dispatcher/mod.rs:51-52,128`); steps 3-4 deliver
`track.report_edited` observations rendered as "information, not an instruction"
(`calm-types/src/observation.rs:285-288`). The Planner reads the new block-0
instructions, which run steps only for a Calendar wake or an explicit user request
(`spy-recipe.md:11`), so the expected turn is a no-op. The new recipe adds one sentence
to make that explicit: "A user edit of this Report requests no step." The operator
checks the Track timeline after the turn: no report write, no `spy.*` write tool call.

Historical `overlay.set` and `track.report_edited` events are not rewritten.

### 3.2 SPY units (`spy.overview` and detail tables replaced)

| Unit kind | Cell | Content, from |
|---|---|---|
| `spy.nav` | metrics | total equity, previous valuation, day P&L, day change (`allocation_views.py:42-68`) |
| `spy.nav_history` | time-series | equity and return datasets with SPY benchmark (`:69-92`) |
| `spy.weights` | distribution | SPY and cash shares (`:97-107`) |
| `spy.weight_history` | time-series | stacked and line weight history (`:108-117`) |
| `spy.holdings` | table | holdings with quote-time caption (`:103-105,118-124`) |
| `spy.decision_log` | records | target, state badge, order facts, rationale, sources, fill disclosures (`:128-156`); newest 50 decisions (`:18,154`), at most 20 fill disclosures each (`:147`) |
| `spy.fill_log` | table | every fill in `state['fills']`, the same newest 200 fills `spy.fills` shows today (`allocation.py:230`, `allocation_report.py:31-33`), including fills that match no decision |
| `spy.account` | metrics | reconciliation time or error (negative tone), quote time, max order step, cash reserve, available cash (`allocation_report.py:19-26`, `allocation_views.py:157-159`) |

`spy.portfolio`, `spy.decisions` and `spy.fills` are deleted (`allocation_report.py:17-33`);
every fact they showed is in `spy.account`, `spy.holdings`, `spy.weights`,
`spy.decision_log` or `spy.fill_log`. The account-mode wording ("长桥官方模拟账户 · SPY／现金",
`allocation_report.py:18`) is recipe context, not data: it stays in the recipe's 来源与边界
prose, so units never claim a broker and the example marker rule (§3.4) still holds.

### 3.3 Supervised profile: deleted (owner decision, slice S0)

The supervised profile is not used on 4140 (the plugin instance runs `spy_cash`; no 4140
Track embeds `paper.*` views), so S0 deletes it outright instead of converting it. S0 is
independent of the contract work and touches only `plugins/paper-trading` and its CI step.

**Residue scan (on the 4140 DB copy, before S0).**

```sql
SELECT id, title FROM track_recipes WHERE body LIKE '%dev-neige-paper-trading/paper.%';
SELECT track_id, id FROM cards WHERE payload LIKE '%dev-neige-paper-trading/paper.%';
SELECT entity_kind, entity_id, kind FROM overlays
 WHERE plugin_id = 'dev-neige-paper-trading' AND kind LIKE 'paper.%';
```

plus a listing of the plugin data directory for `ledger.sqlite3`/`strategy.sqlite3` (the
supervised ledgers, `allocation.py:33-34`). Expected result: empty. Any hit is deleted in
S0 through the user APIs (`DELETE /api/track-recipes/{id}`, the block DELETE of §3.1,
`POST /api/overlays/delete`), so the S4 gate cannot block on an unplanned recipe; a
supervised ledger file stops S0 for an owner decision.

**Delete** (supervised-only modules and files):

- `paper_trading/strategy.py` (`Portfolio`, the eight `paper.*` tools), `engine.py`,
  `research.py`, `reconcile.py`, `operator.py`, `recipe.md`.
- `paper_trading/portfolio.py`, after moving `BROKER_ACTIVE`/`BROKER_TERMINAL`
  (`portfolio.py:9-11`) into `allocation_reconcile.py`, their only SPY user (`:7`).
- `paper_trading/report.py`, after moving `display_cell`/`table` (`report.py:8-19`), used
  by `allocation_report.py:3`, into `report_views.py`.
- `allocation.py:33-34`, the guard against reusing supervised data, which no longer
  exists (the residue scan above checks the 4140 data directory instead).
- Tests: `test_strategy.py`, `test_strategy_process.py`, `test_engine.py`,
  `test_report.py`, `test_report_views.py`, `test_report_view_regressions.py`,
  `test_process.py` (after moving its `Host` class, which `test_allocation.py:318` uses,
  into `tests/host.py`), `fixture_cli.py`, `smoke_host.py`, `browser_smoke.cjs`.
  `conftest.py` shrinks to `ROOT` and the `sys.path` insert (`conftest.py:8-9`) every test
  module's `paper_trading` import depends on; its supervised `rig` fixture
  (`:18-98`) goes.

**Keep and trim** (what `spy_cash` still uses):

- `report_views.py`: keeps the generic helpers `native_view`, `row`, `scalar`, `unknown`,
  `metric`, `record`, `records` (`:12-54`, used by `allocation_views.py:8`) plus the moved
  `display_cell`/`table`, so it is the one presentation-helper module; deletes `overview`,
  `activity`, `reviews`, `details`, `table_view`, `view_payloads` (`:57-219`) and the
  imports only they used (`:3,5,8-9`: `Decimal`, `Fraction`, portfolio and report_text).
- `report_text.py`: keeps `money_text`, `bounded` (`allocation_views.py:7`); deletes
  `state_text`, `event_text` (`:16-78`).
- `config.py`: keeps `money`, `integer`, `broker_money`, `timestamp`, `identifier`, `exact`
  (used by `allocation*.py` and `sdk_bridge.py:14`); deletes `calendar_date`, `symbol`,
  `AccountConfig`, `StrategyConfig`, `Config` (`:46-54,73-78,85-190`) and the imports
  that become unused (`asdict`, `dataclass`, `date`, `Path`, `json`). `AllocationConfig`
  inlines the account checks it borrows from `AccountConfig.parse`
  (`allocation_config.py:34-35`).
- `broker.py`: keeps `BrokerError`, `_object`, the environment allowlist, output cap and
  `Broker.__init__`/`_run`/`_json`, which `AllocationBroker` subclasses
  (`allocation_broker.py:4-26`); deletes the CLI order/cancel/read surface (`order_args`,
  `cancel_args`, `identity` through `execute`, `broker.py:53-69,107-108,179-249`) and
  `_records` (`:76`), whose only callers are those reads.
- `test_broker.py`: the base-runner contracts (environment allowlist, timeout and reap,
  combined output bound, launch failure; `test_broker.py:234-362`) are driven today
  through `assets`/`preview`/`execute`, which S0 deletes. S0 re-drives them through
  `AllocationBroker.request` (`allocation_broker.py:16-26`) against the SDK fixture; the
  CLI-only tests go.
- `ledger.py`: deletes `Ledger.reviews` (`:100-112`). The schema stays byte-for-byte: the
  live 4140 SPY ledger created those tables, and dropping unused `CREATE TABLE IF NOT
  EXISTS` lines buys nothing at a persistence boundary.
- `rpc.py`: only the `Allocation` path (`:116-118`); deletes the supervised branch
  (`:119-125`), its imports (`:10-13`), the non-`Allocation` call branch and the
  `paper.status`/`paper.journal` entries of the read-only list (`:76-82`), and the
  per-profile tool filter (`:144`). The profile check has one owner,
  `AllocationConfig.parse` (`allocation_config.py:26-32`): `profile` is in its required
  set and must equal `spy_cash`; `rpc.py` does not repeat it.
- `runtime.py`: publishes only `allocation_report.tables` (`:4-5,21`).
- `paper_trading/__init__.py:1` docstring and `allocation_broker.py:1` drop "supervised".
- `manifest.json`: deletes the eight `paper.*` tools; `profile` becomes required with the
  single value `spy_cash` and no default (it stays the explicit opt-in to automatic
  execution, and the 4140 config already sets it); deletes `cli_path`, unused by SPY, from
  the manifest, `AllocationConfig` and `test_allocation.py:318`.
- README: rewrites the intro (`README.md:1-15`), deletes `:17-333` (supervised setup,
  approval, migration, first cycle, recovery and its verification commands), keeps the
  example paragraph (`:334-345`) and the SPY section (`:346-`), whose comparison with the
  supervised profile (`:363`) is reworded.
- CI: `.github/workflows/paper-trading.yml:37` step name drops "process and
  human-confirmation"; the pytest path (`:39`) is unchanged.

**Deploy S0 only through a kernel restart or `POST /api/plugins/{id}/reload`.** A stored
`cli_path` is harmless: the effective configuration drops keys the manifest does not
declare (`plugin_host/config.rs:7,27-38`). The hazard is a stale manifest: the kernel
keeps the manifest it loaded at boot (`state.rs:1012`) until a reload re-reads it
(`plugin_host/lifecycle.rs:243-327`, route `routes/plugins.rs:42`). A child-only respawn
of the new code under the old manifest would receive the old `cli_path` default, which
the new exact parse (`allocation_config.py:30`) refuses. The restart-timing guard of §5
applies.

Historical design docs (`docs/design-paper-trading-loop.md`,
`docs/design-paper-strategy-configuration.md`, `docs/design-paper-report-hierarchy.md`,
`docs/design-spy-cash-rebalance.md:11`) stay as history.

### 3.4 Example and frontend fixtures

The example must remain the real production output of the scripted simulated account
(`examples/build_native_demo.py:1-9`). It becomes template plus units:
`examples/native-demo.json = {"views": [...], "overlays": {"<kind>": <unit>}}`.
`views` are the `view` fences parsed from `spy-recipe.md` with only their (neutral)
descriptions replaced by the example marker, as `create_view` does today
(`build_native_demo.py:170-174`); `overlays` is `allocation_report.tables(state)` of the
scripted run. `native-demo.md`, the inline paste copy (`build_native_demo.py:193-198`), is
deleted: a template with live slots has no inline equivalent, and materializing one would
duplicate the resolver in Python.

Consumers to update in S3: `tests/test_native_demo.py:20-49`;
`fe/tools/report-view/conformance.test.ts:37-39`;
`fe/web/src/features/report/native/native.browser.test.tsx:6`;
`fe/web/src/app/router/report-shell-preservation.browser.test.tsx:8`. The frontend tests
render `views` through `NativeReportView` with a resolver that calls the production
`trackOverlayPayload` over overlay wires built from `overlays`, not a test copy of the
lookup. The shared fixture `test-data/native-view-v1.json` (`native_view_tests.rs:134-139`)
gains live-slot views plus valid and invalid units in S1.

## 4. Proposed `spy-recipe.md` report body

Contract header:

```
<!-- neige:contract {"version":1,"sections":[{"h1":"组合表现"},{"h1":"资金投向"},{"h1":"调仓决策"},{"h1":"结论"},{"h1":"待你定","omit_if_empty":true},{"h1":"核心逻辑"},{"h1":"关键数据"},{"h1":"风险与证伪"},{"h1":"催化剂与跟踪"},{"h1":"复盘"},{"h1":"来源与边界"},{"h1":"执行记录"}]} -->
```

执行记录 is **not** `omit_if_empty`: new task blocks are appended at the end of the
document (`track_report.rs:535-565`), so the H1 must exist before the first one, or the
first execution task would land under 来源与边界.

Planner comment edits (everything else in `spy-recipe.md:2-27` unchanged):

- Routing (`:11`): add "A user edit of this Report requests no step."
- Execution step (`:13`): the task block "is appended at the end of the Report, in 执行记录".
- Post-close (`:14`) and Report (`:17`): "refer to 组合表现, 资金投向 and 调仓决策" instead of
  组合概览; "No step rewrites 组合表现, 资金投向, 调仓决策 or 执行记录: the first three are
  template views of live App data; 执行记录 holds only the appended execution task
  blocks."
- 来源与边界 (`:22`): its data-limits line names the account: 长桥官方模拟账户 · SPY／现金.
- Rules (`:26`): "Keep the live views and the ledger."

Body (`P` = `neige://plugin/dev-neige-paper-trading`; written compact, recipe
normalization re-renders fences canonically, `track_recipes.rs:33-77`):

````markdown
# 组合表现

```neige-block view
{"version":1,"title":"","description":"","snapshot":null,"rows":[
 {"id":"performance","title":"","layout":"two-wide-end","cells":[
  {"kind":"live","id":"nav","source":"P/spy.nav","expects":"metrics"},
  {"kind":"live","id":"nav-history","source":"P/spy.nav_history","expects":"time-series"}]},
 {"id":"account","title":"对账与规则","layout":"one","cells":[
  {"kind":"live","id":"account","source":"P/spy.account","expects":"metrics"}]}]}
```

# 资金投向

```neige-block view
{"version":1,"title":"","description":"","snapshot":null,"rows":[
 {"id":"allocation","title":"","layout":"three","cells":[
  {"kind":"live","id":"weights","source":"P/spy.weights","expects":"distribution"},
  {"kind":"live","id":"weight-history","source":"P/spy.weight_history","expects":"time-series"},
  {"kind":"live","id":"holdings","source":"P/spy.holdings","expects":"table"}]}]}
```

# 调仓决策

```neige-block view
{"version":1,"title":"","description":"","snapshot":null,"rows":[
 {"id":"decisions","title":"","layout":"one","cells":[
  {"kind":"live","id":"decisions","source":"P/spy.decision_log","expects":"records"}]},
 {"id":"fills","title":"成交明细","layout":"one","cells":[
  {"kind":"live","id":"fills","source":"P/spy.fill_log","expects":"table"}]}]}
```

# 结论

# 待你定

# 核心逻辑

# 关键数据

# 风险与证伪

# 催化剂与跟踪

# 复盘

## 最近交易日

## 最近一周

# 来源与边界

仅作研究，不构成交易建议。

# 执行记录
````

`guard_task_declarations` refuses rewrites that drop task blocks
(`track_report_edit_guard.rs:135`); no routine step names 执行记录.

## 5. Slices

Each slice is independently reviewable and green and is deployed in order. Every slice
runs `scripts/local-ratchet-gates.sh`; Rust runs use the targeted `cargo nextest -p <pkg>`
form from AGENTS.md. Mutation plans name one single-factor production mutation and the
complete predicted red set, per language. S1, S2 and S4 change Rust and frontend text, so
`scripts/gate-prose-ratchet.baseline.tsv` and `scripts/gate-1316-terminology-ratchet.baseline.tsv`
may need `--update-baseline` as generated artifacts of those slices.

**Restart-timing guard (S0, S3 and every kernel deploy).** A plugin restart while a
decision is `submitting` turns it into `unknown` (`ledger.py:50-53`). Every deploy that
restarts the plugin, directly or through the kernel, runs under the §3.1 step-1
conditions: outside the SPY Calendar windows, and `spy.status` shows no decision in
`queued`, `requested` or `submitting`.

**S0: delete the supervised paper profile** (`plugins/paper-trading`, its CI step;
independent of S1-S4, can merge and deploy first).

- Everything listed in §3.3, including the residue scan; deploy through a kernel restart
  or plugin reload under the restart-timing guard.
- Tests: the remaining suite (`python -m pytest plugins/paper-trading/tests -q`, now
  `test_allocation*.py`, `test_native_demo.py`, `test_sdk_bridge.py`, trimmed
  `test_broker.py`) green; `build_native_demo.py --check` unchanged;
  `cargo nextest -p calm-server real_spy_app_admits_planner_plan_and_worker_execution_request`
  (the kernel test that boots the real App) green; new `AllocationConfig.parse` tests that
  a missing `profile` and a `profile` other than `spy_cash` are both refused; the moved
  base-runner tests of `test_broker.py` green through `AllocationBroker.request`.
- Acceptance: `git grep -n -E 'Portfolio\b|AccountConfig|StrategyConfig|paper\.(strategy|ingest|decide|status|refresh|pause|journal|review)\b|fixture_cli|cli_path|paper_trading\.(engine|strategy|operator|research|reconcile|portfolio|report)\b|supervised' -- plugins/paper-trading .github`
  matches nothing (this includes `test_allocation.py:318`, `__init__.py:1` and
  `allocation_broker.py:1`); `pyflakes` reports no unused import in `paper_trading/`.
- Coverage note: the supervised isolated-host smoke (`smoke_host.py`) goes with the
  profile; SPY process-level coverage stays in `test_allocation.py` (real `run`
  subprocess through `Host`) and the kernel test above.
- Mutation, Python: delete the `profile != 'spy_cash'` check in `AllocationConfig.parse`
  (`allocation_config.py:31-32`), the single owner of the profile rule. Red: the
  other-profile refusal test only (a missing `profile` is still refused by `exact`'s
  required set, `:26-30`).

**S1: contract, write validation, frontend rendering** (calm-types, generated artifacts,
`fe/core/domain`, `features/report`). One slice because the generated TypeScript union changes with the DTO and the
renderer's exhaustive switch (`native/public.tsx:14-29`) must handle it in the same change.

- Rust: `DataUnit`, `LiveSlot`, `RowCell` with its dispatching deserializer,
  `ComponentKind` with the exhaustive `Component::kind`, nullable view snapshot with its
  rule, shared `validate_snapshot`, `validate_unit(expects, payload)`,
  `MAX_LIVE_UNIT_BYTES`; `generated_schema()` exports `DataUnit`.
- Generated artifacts via the real generator: `native_view.schema.json`,
  `fe/core/domain/report-view.generated.ts` and `report-view.types.generated.ts`.
  `fe/tools/report-view/generate.mjs` exports only the root `NativeView` today
  (`:106`); it also exports `DataUnit` from `$defs` with its own decoder
  (`dataUnitShapeSchema`).
- Frontend: unit decoder and slot resolver in `fe/core/domain` (§2.5 steps), the new
  relation rules in `report-view.ts`, the shared placeholder, snapshot disclosure,
  empty-title behavior and `resolveOverlay` for `view` blocks (§2.6).
- Tests: the view relations are pinned only through new invalid entries of the shared
  fixture `test-data/native-view-v1.json` (`snapshot-null-with-inline-cell`,
  `snapshot-set-without-inline-cell`, `duplicate-slot-id`, `slot-layout-width`,
  `slot-bad-source`, `slot-unknown-expects`), so `native_view_shared_conformance`
  (`native_view_tests.rs:143`) and `conformance.test.ts:65` check both languages from one
  list; calm-types unit tests for the slot deserializer's error text and for units (wrong
  kind, bad version, a `live` cell rejected by the `Component` deserializer); frontend
  unit tests: "degrades only the failing slot" (one ok, one pending, one malformed slot)
  and, separately, "shows a wrong-kind unit as unavailable" (one ok and one wrong-kind
  slot); empty-title accessible names; a browser test of a two-wide-end row with one
  degraded slot at 1440 and 390 px. No test for live slots inside a `view.live` overlay:
  that path adds no code (§2.5).
- Mutation, Rust: drop the `cell.kind == expects` comparison in `validate_unit`. Red:
  `native_view_tests::unit_of_another_kind_is_rejected` only.
- Mutation, Rust: drop the snapshot-nullability relation. Red:
  `native_view_tests::native_view_shared_conformance` only (the snapshot cases live in the
  fixture; the TypeScript side has its own copy of the rule and stays green).
- Mutation, TypeScript: drop the snapshot-nullability relation in `report-view.ts`. Red:
  `conformance.test.ts` "rejects snapshot-null-with-inline-cell" and "rejects
  snapshot-set-without-inline-cell" only.
- Mutation, TypeScript: make any slot failure fail the whole composition. Red: "degrades
  only the failing slot", "shows a wrong-kind unit as unavailable" (its ok sibling stops
  rendering) and the browser "degraded slot keeps the row layout" only.
- Mutation, TypeScript: drop the `expects` comparison in the slot resolver. Red: "shows a
  wrong-kind unit as unavailable" only.

**S2: kernel read hydration and Planner guidance** (`track_report_hydrate.rs`, prompts,
golden).

- Per-slot `resolved` summary and full data as in §2.7; `is_overlay_block` extended to
  `view` blocks with live slots (`track_report_hydrate.rs:116-119`).
- Tests (extend `crates/calm-server/tests/cases/mcp_track_report_live_view.rs`): another
  plugin's overlay with the same kind is not used; each slot resolves its own source;
  pending vs storage-unavailable; per-unit cap; one bad slot leaving the others ok;
  `none` performing no query; stale-CAS behavior of template views.
- `planner_tool_surface_fits_its_byte_budget` (re-measure; update the cap comment to the
  new number) and the regenerated registry golden.
- Mutation, Rust: drop `overlay.plugin_id == plugin_id` (`track_report_hydrate.rs:145`).
  Red: `live_slot_ignores_another_plugins_overlay_of_the_same_kind` and the existing
  `live_view_hydration_is_scoped_bounded_and_read_only` (its `other` plugin publishes the
  same kind, `mcp_track_report_live_view.rs:211-221`) only. (Mutating the
  entity predicate at `:143-144` is equivalent: `overlays_for("track", track_id)` already
  filters by entity in SQL, `:78-79`.)
- Mutation, Rust: resolve every slot of a view with its first slot's source. Red:
  `live_slots_resolve_their_own_units` and `one_bad_slot_leaves_the_others_ok` only.

**S3: plugin data units, recipes, example** (`plugins/paper-trading`).

- `allocation_views.py` emits the §3.2 units and deletes the SPY detail tables; units no
  longer compose rows, so `report_views.row` is deleted and `native_view` becomes a
  `unit(state, cell)` builder that keeps the content-hash snapshot identity;
  `spy-recipe.md` per §4; regenerate the example; update README; frontend example tests
  (§3.4); the kernel test that boots the real App
  (`crates/calm-server/tests/cases/mcp_plugin_tools/caller_identity.rs:160-177`, today
  waiting for `spy.overview` and validating it as a view) waits for `spy.decision_log` and
  validates it with `validate_unit("records", …)`.
- Tests: units validate against the generated `DataUnit` schema; the existing projection
  tests move to units; `test_spy_recipe_contract_matches_body_and_published_views`
  (`tests/test_allocation_views.py:224-240`) becomes set equality between recipe slot
  sources and published unit kinds in both directions, with each slot's `expects` equal
  to the published unit's kind; H1 order equals the contract and the last H1 is 执行记录;
  `build_native_demo.py --check`; `cargo nextest -p calm-server
  real_spy_app_admits_planner_plan_and_worker_execution_request`.
- Mutation, Python: drop `spy.account` from the published units. Red:
  `test_spy_recipe_contract_matches_body_and_published_views`,
  `test_example_is_the_production_output_of_the_scripted_run`, and the two tests whose
  description assertions move to `tables(state)['spy.account']`: the reconciliation-error
  check of `test_overview_is_valid_before_reconciliation_and_after_errors`
  (`test_allocation_views.py:218`) and `test_snapshot_identity_covers_the_description`
  (`:276-278`), each renamed for the unit, only.

**S4: 4140 rewrite and `view.live` removal.**

- Run §3.1 (scan, runbook, scan again) on 4140 after S3 is deployed; production restarts
  follow the machine runbook.
- Remove: `KIND_LIVE_VIEW`, its `DATA_KINDS` entry, dispatch and `validate_live_view`
  (`kinds.rs:13,32,42,83,510-524`) and its re-exports (`report_blocks/mod.rs:23-25`);
  `kinds_tests.rs` cases (`:58-76,345`); `live_view_kind` (`contracts.rs:447-461`) and
  `prompts/report-kinds/view.live.md`; the `view.live` wording in
  `prompts/tools/calm.report.blocks.kinds.md` and `calm.report.read.md`;
  `hydrate_live_view` (`track_report_hydrate.rs:150-151,183-208`);
  `view.live` cases in `tests/cases/mcp_track_report_live_view.rs` (renamed to
  `mcp_track_report_live_slots.rs`, `tests/mcp_integration_suite.rs:42-43` updated) and
  `tests/cases/mcp_track_report_blocks.rs:200`; the registry golden; frontend
  `native/live.tsx`, `live.test.tsx`, `live.browser.test.tsx`,
  `fe/core/domain/report-live-view.test.ts`, the `view.live` case of
  `native/native.browser.test.tsx:52`, `LiveViewBlockPayload` and its kind branches
  (`fe/core/domain/report.ts:55-59,228,250`, `document/public.tsx:200-201,217,240-241`,
  `document/public.test.tsx`); `docs/report-live-views.md` rewritten for live slots.
- Acceptance (authoritative list): `git grep -n -P '(?<![A-Za-z])view\.live|KIND_LIVE_VIEW|MAX_LIVE_VIEW_BYTES|LiveViewBlock|liveViewBlock|hydrate_live_view'`
  (the lookbehind keeps `resolution.preview.live`, `preview/public.tsx:78`, out)
  matches only the historical design docs `docs/design-native-report-composition.md`,
  `docs/design-paper-report-hierarchy.md`, `docs/design-report-presentation-boundaries.md`
  and this document; an old `view.live` fence is rejected at each write-end family (block
  upsert, Replace, recipe, fork); surface budget re-measured.

## 6. Risks and owner decisions

**Risks.**

- Between the S3 deploy and the 4140 rewrite the old `view.live` keeps showing the last
  `spy.overview` row (stale, with its own `observedAt`). Keep it to minutes by running the
  runbook right after the plugin restarts, inside the allowed windows.
- A full `write_markdown` by the Planner can still delete template views
  (`track_report.rs:681-684`); recoverable from the recipe, not automatically.
- `resolve: full` on one view returns at most 18 units (6 rows × 3) of ≤4 MiB each; the
  largest SPY unit is bounded near 2.4 MB (§2.5). No aggregate cap by design.
- SPY grows from four to eight overlay kinds per poll tick; the churn is #1995's.

**Owner decisions (2026-10-04, final; no open questions).**

1. No kernel guard for template views: recipe guidance plus section-scoped writes,
   status-quo parity with `view.live` (§2.7).
2. The supervised paper profile is deleted (§3.3, S0).
3. Live `table` and `chart.series` stay single-block live references; they are not folded
   into live slots (§1 goal 2).

## 7. Revision log

Revision 2 (review round 1, nothing blocking):

- Facts: Planner surface re-derived (28,859/30,000, 26 tools); `allocation_report.py` and
  `native/public.tsx` line references corrected; leftover `view.live` breaks Replace,
  `write_markdown` and fork, not other sections' replaces; ledger binding cited from the
  ledger and call sites.
- Types: `LiveSlot` + `RowCell` with a dispatching deserializer; `DataUnit.cell: Component`
  forbids nesting by type; slot field `cell` renamed `expects`; unit-local cell ids.
- Dropped the per-view aggregate budget and publish-on-change (#1995 follow-up).
- Added the transition fence, the `spy.fill_log` unit (fills are no longer lost; former
  Q2 removed), empty-title accessibility as new S1 behavior, generator export of
  `DataUnit`, the DB-copy precondition scan and the rewrite runbook with the Planner
  wake, per-language mutation plans with red sets, the complete S4 removal list.
- New pending owner question on the supervised profile; 执行记录 kept non-optional.

Revision 3 (owner decisions):

- Q1 and Q3 recorded as decided (no guard; keep live `table`/`chart.series`); the open
  question list is empty.
- Supervised profile deleted as its own first slice S0 (§3.3): delete list, keep-and-trim
  list for helpers `spy_cash` still uses, manifest/config and deploy-order note, test and
  CI sweep, acceptance grep, mutation plan. S3 now covers only `spy_cash` units and adds
  the kernel real-App test (`caller_identity.rs`) to its sweep.

Revision 4 (review round 2, rebased on 54b79918c; nothing blocking):

- S0: deploy only through a kernel restart or plugin reload (stale-manifest hazard; a
  stored `cli_path` is dropped by the effective configuration); one owner of the profile
  rule (`AllocationConfig.parse`) and a mutation that is not masked; residue scan of the
  4140 copy for `paper.*` recipes, blocks, overlays and supervised ledger files; sweep
  gaps closed (`allocation.py:33-34`, `__init__.py`, `rpc.py:76-82`, `cli_path` in
  `test_allocation.py`, base-runner tests re-driven through `AllocationBroker.request`,
  dead `_records`, unused imports, `conftest.py` kept for the `sys.path` insert); README
  range fixed.
- Restart-timing guard for S0, S3 and every kernel deploy (`submitting` → `unknown` on
  restart).
- §3.1 runbook: Replace carries the step-2 `summary`; any non-2xx after a delete restores
  from the saved GET; 更多明细 becomes 执行记录; research `table`/`chart.series` blocks stay
  byte-identical; `engine.py` cite dropped.
- Validation: the relation rules are written in both languages and pinned by new shared
  fixture entries; slot `source` is a generated schema pattern; the transition fence and
  its tests are dropped (no nesting by construction); `Component::kind` is exhaustive;
  shared `validate_snapshot`; `generated_schema()` exports `DataUnit`; the slot resolver
  sits in `fe/core/domain`; one shared placeholder.
- Mutation red sets corrected (S1 split tests and per-language snapshot rule, S2 adds the
  existing scoping test, S3 adds the moved description tests); S4 grep uses a lookbehind
  and lists `native.browser.test.tsx:52`; unit size stated as a bound; ratchet baselines
  listed as possible generated artifacts; cites re-checked after the rebase
  (`mcp_integration_suite.rs:42-43`, `mod.rs:170-194`, `track_recipes.rs:37-38,58`).
