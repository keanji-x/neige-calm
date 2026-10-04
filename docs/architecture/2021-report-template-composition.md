# Report template composition: the template places, the plugin publishes data units

Baseline: `origin/main` 5971cf5e6. Every `file:line` below was read on that tree.
Status: design for review. No code changes in this PR.

## 1. Problem, goals, non-goals

**Owner decision.** Report layout belongs to the report template (a Track recipe or a
builtin template body). Plugins own data and business meaning: calculations, labels,
units, tones, empty states and evidence. The template decides which data sits under
which heading, in which row and whether side by side.

**Today the publisher composes.** `view.live` is only `{source, version}`
(`crates/calm-types/src/report_blocks/kinds.rs:510-524`); its overlay is a whole
`NativeView` (title, description, snapshot, up to six rows with layouts and cells,
`native_view/model.rs:8-43`). The SPY profile publishes `spy.overview` with three rows
and their titles and layouts (`plugins/paper-trading/paper_trading/allocation_views.py:93,125,160-162`);
`spy-recipe.md:29-33` embeds it under one H1. The template cannot split it, reorder it or put
a research section between its rows. The supervised profile does the same with seven
views (`paper_trading/report_views.py:196-219`, `recipe.md:83-117`).

Further costs of the current shape:

- One bad cell blanks the whole view. Kernel hydration validates the whole overlay and
  returns one `unavailable` (`mcp_server/tools/track_report_hydrate.rs:193-202`); the
  frontend does the same (`fe/web/src/features/report/native/live.tsx:15-18`).
- Two composition mechanisms with one grammar: inline `view` and `view.live`
  (`docs/report-live-views.md`), plus duplicated SPY detail tables
  (`spy.portfolio`, `spy.decisions`, `spy.fills`, `paper_trading/allocation_report.py:179-195`)
  that restate the overview.

**Goals.**

1. The template owns headings, rows, layouts and slot order. A plugin publishes data
   units, each exactly one cell, with labels, units, tones and empty text.
2. One composition mechanism: the inline `view` block. `view.live` is removed.
3. Each live cell resolves, validates and degrades independently, with a visible
   placeholder in its own slot.
4. No plugin identity in kernel or frontend; resolution stays the generic exact
   `(track, plugin, kind)` overlay lookup (`track_report_hydrate.rs:121-149`,
   `fe/core/domain/track.ts:107-128`).

**Non-goals.** No change to inline cell kinds or their limits. No field-level data
binding or query language. No new overlay transport, write-time overlay validation or
manifest declaration of units. No platform staleness thresholds. Live `table` blocks
(`kinds.rs:494-508`, used by `plugins/market` and `plugins/barra`) stay as released;
folding them into live cells is a separate decision (§6 Q3).

## 2. Contract

### 2.1 Options

| Option | Template owns layout | Side by side | Mechanisms | Verdict |
|---|---|---|---|---|
| (a) Inline `view` whose cells may be live references, resolved per cell; delete `view.live` | yes: rows, layouts, slot order, row titles | yes, all five layouts (`model.rs:44-54`) | one | **chosen** |
| (b) Single-cell overlays placed by per-cell `view.live`/`cell.live` blocks | headings only | no: blocks flow vertically; the owner-approved two/three-column layouts (`docs/design-native-dashboard-reading.md:10-14`) are lost | two (`view` plus cell blocks) | rejected |
| (c1) Keep `view.live`, add template row/cell selection (`{source, pick}`) | partially: can drop, not regroup across publishers or interleave | only the publisher's own rows | two | rejected |
| (c2) Template components with field bindings (template writes labels, binds numbers) | yes | yes | one | rejected: labels, units and tones are plugin meaning; needs a binding language |
| (c3) Layout block that arranges other blocks by id | yes | yes | two | rejected: block ids are minted per Track at instantiation, so a template cannot name them (`routes/track_recipes.rs:36-37,57`) |

Option (a) reuses the existing grammar, renderer, inspection state and layouts. The only new
ideas are the slot reference and the per-cell envelope.

### 2.2 Data unit (overlay payload)

A plugin publishes one unit per overlay kind on the Track, through the unchanged
`neige.overlay.set` path (`plugin_host/callbacks.rs:192-242`):

```json
{"version": 1,
 "snapshot": {"id": "<content hash>", "observedAt": 1790798340000, "producedAt": null},
 "cell": { "kind": "metrics", "id": "nav", "title": "", "items": [ ... ] }}
```

- `cell` is exactly one existing `Component` (`model.rs:261-362`): metrics, time-series,
  distribution, table, bars, meter or records. It is validated by the existing
  `Component::validate` (`native_view.rs:69-255`). A unit's cell may not be a live
  reference, so a unit can never point to another unit.
- `snapshot` reuses the `Snapshot` DTO (`model.rs:21-32`): required id, required nullable
  `observedAt`/`producedAt`. Provenance moves from the view to the unit.
- `version` is the envelope version, checked by readers against the one version they
  support. The template does not repeat it: one overlay key holds one payload, so a
  template version pin cannot select anything.
- A unit kind never reuses a retired overlay kind name, so each overlay key keeps one shape.

New calm-types items: `DataUnit` DTO in `native_view/model.rs`, `validate_unit(expected_kind,
payload)` in `native_view.rs`, and `DataUnit` added to the generated schema and TypeScript
declarations (`native_view.rs:301-381`, `examples/export_report_view_contract.rs`).

### 2.3 Template reference (live cell)

A row cell is either an inline `Component` or a live slot:

```json
{"kind": "live", "id": "nav", "source": "neige://plugin/dev-neige-paper-trading/spy.nav", "cell": "metrics"}
```

- `id`: the slot's identity in the composition, the same id rules and uniqueness as any
  cell (`native_view.rs:21-35,274-277`). The frontend keys inspection state by slot id.
- `source`: the existing two-segment overlay URI, shape-checked by `validate_live_source`
  (`kinds.rs:117-121`); existence is not checked, because an uninstalled plugin is a
  normal state.
- `cell`: the cell kind the template laid out for. A unit with another kind is
  `unavailable` in that slot, so a publisher change cannot silently break the
  template's layout.
- No title: the cell's title comes from the unit (publisher label). The template owns row
  titles, the view description and the surrounding Markdown headings.

`NativeView.snapshot` becomes required nullable. It describes the inline cells and is
non-null exactly when the view has at least one inline cell. Every existing v1 payload
has only inline cells and a non-null snapshot, so it stays valid. `version` stays `1`:
the change is additive for stored data. Old clients are the same build and are not
supported separately. The renderer omits the `<h2>` for an empty view title
(`fe/web/src/features/report/native/public.tsx:71`), so a template view can sit
directly under its H1.

### 2.4 Validation

**Write time (template structure only).** `validate_payload("view")` (`kinds.rs:84-90`)
checks rows, layout widths, slot-id uniqueness, the live slot shape, `cell` within the
closed kind set and the snapshot rule. The existing 256 KiB canonical cap applies to
the template (`kinds.rs:30,96-107`). This runs at every write end that already calls
`validate_payload`: block upsert, Replace, write_markdown, section replace, recipe
create/update, track create and fork (`track_report.rs:648,684`,
`track_report/sections.rs:113`, `routes/track_recipes.rs:95-108`,
`routes/tracks.rs:599,614,1832`). Overlay writes stay opaque (`callbacks.rs:210-213`,
`calm-truth/src/validation.rs:489-491`).

**Read time (each live cell independently).** For each slot in row and cell order:

1. Exact overlay lookup by Track, plugin and kind. None: `pending`. Storage error:
   the whole block is `unavailable`, as today (`track_report_hydrate.rs:75-84,108`).
2. Compact UTF-8 size of at most 4 MiB per unit: today's `MAX_LIVE_VIEW_BYTES`
   (`kinds.rs:31-32`), renamed `MAX_LIVE_UNIT_BYTES`.
3. Running total of resolved units in the view of at most 4 MiB. This keeps the
   resource bound that one `view.live` had. A slot past the budget is `unavailable`;
   earlier slots still render.
4. `validate_unit(slot.cell, payload)`: envelope shape, version, `cell.kind == slot.cell`,
   `Component::validate`.

A failure affects only its slot. The kernel and the frontend apply the same four steps.
The frontend uses the generated decoder plus per-component relation checks, refactored
from the current whole-view `relations` (`fe/core/domain/report-view.ts:25-70`) into a
per-component function that inline cells and units share.

### 2.5 Presentation of degraded and stale cells

- Placeholder in the slot, keeping the row's layout: "waiting for `<source>`" (pending),
  "cannot be displayed: `<reason>`" (`role="status"`, unavailable), or "this view
  carries no live data" when no resolver is injected (same states as
  `features/report/table/public.tsx:14-26`).
- Staleness is shown, not judged. The view's snapshot disclosure
  (`native/public.tsx:60-63`) lists the inline snapshot (if any) and, for each live slot,
  the cell title, `observedAt` and `producedAt`. A null value reads "unknown". There is no
  time threshold: whether data is too old is business meaning that the publisher
  expresses as a tone or text. For example, SPY's reconciliation-error item replaces
  today's description suffix (`allocation_views.py:157-159`).
- The frontend needs no new transport: overlays already arrive with the Track detail and
  are invalidated per `overlay.set` (`fe/core/events/invalidation-plan.ts:196-204`). The
  injected `resolveOverlay` (`fe/web/src/app/router/public.tsx:2154-2157`) is passed to
  `NativeReportView` for `view` blocks (`features/report/document/public.tsx:242`).

### 2.6 Planner

- `calm.report.read` shows the template fence in `text` (about 1 KB per view) and, for a
  `view` with live slots, `resolved = {status: ok|partial, validation: "presentation",
  cells: [{id, source, status, observed_at?, resolved_at?, reason?}]}`. Block status is
  `ok` when every slot is ok, `partial` otherwise, and `unavailable` on storage error.
  `resolve: {id: "full"}` adds `data` (the unit envelope) to each ok slot;
  `"none"` skips the overlay query (`track_report_hydrate.rs:66-96`). Reads never call
  plugins.
- The Planner does not author or rewrite template views. Enforcement stays as today:
  recipe instructions name the template-owned H1s, and routine steps use section-scoped
  replace, which cannot touch other sections (`track_report/sections.rs:100-113`). A
  full `write_markdown` may still rewrite non-prose blocks
  (`track_report.rs:681-684`); a kernel guard is §6 Q1.
- Guidance edits: the `view` usage text (`prompts/report-kinds/view.md`, returned in the
  `calm.report.blocks.kinds` result, not the always-loaded surface) explains live slots as
  template-owned references. `calm.report.read.md` swaps "view.live" for "view live
  cells". The Planner tool surface is 29,984 of its 30,000 bytes
  (`mcp_server/tools/mod.rs:171-174`): S2 must stay net-neutral, and S4 frees bytes by
  removing `view.live` from the kind enum (`track_report_blocks/contracts.rs:426-436`)
  and the two descriptions. The registry golden
  (`crates/calm-server/tests/goldens/mcp_tool_registry.json`) is regenerated in both.

### 2.7 Publisher-owned meaning is preserved

A unit carries the whole `Component`, so record subtitles, badges with tones, facts,
sections and disclosures (`model.rs:148-167`), table columns and captions
(`model.rs:373-386`), metric units, signs and precision, and empty texts stay exactly the
plugin's. The template contributes only slot position, row title, layout and view
description.

## 3. Migration

### 3.1 `view.live`: remove, with an explicit rewrite of 4140 (no migration code)

- **Why not a SQL migration:** report bodies are Automerge documents
  (`crates/calm-server/src/track_report_doc.rs:1-30`), not rewritable by SQL.
- **Why not a boot migrator:** it would be permanent kernel code for a few Tracks, and
  the rewrite is a layout decision (new H1s, new slots), not a kind rename.
- **Why in place:** the SPY Track cannot be recreated, because the plugin binds its ledger
  to `owner_track_id` (`plugins/paper-trading/manifest.json:17`).
- **Why the rewrite must precede S4:** a leftover `view.live` block makes every
  whole-body write on that Track fail (`validate_body_fences` at
  `track_report.rs:648,684` and `sections.rs:113`), makes a fork fail
  (`routes/tracks.rs:1832`), and the frontend shows it as `unsupported`
  (`fe/core/domain/report.ts:259-262`).

**What the 4140 rewrite covers** (run once, after S3 is deployed, through ordinary user
APIs, script and transcript in the S4 PR):

1. Every Track report with a `view.live` block. Known: SPY Track
   `7c0dd087ed9c4f7895ce4c6bb574c327`, which has one `view.live` (`spy.overview`), three
   live tables (`spy.portfolio`, `spy.decisions`, `spy.fills`) and the appended execution
   task blocks under 更多明细. Unknown and must be scanned: supervised paper Tracks (seven
   `view.live`), Planner-authored `view.live` in any Track, and forks of these.
   For each Track: `DELETE /api/tracks/{id}/report/blocks/{block}` for the `view.live`
   and retired live-table blocks. Then `POST /api/tracks/{id}/report` (Replace) with the
   new body: the new contract header and Planner comment in block 0, the new H1s and
   template views, research prose unchanged, and the task fences byte-identical under
   执行记录. Replace refuses to drop or alter existing non-prose blocks
   (`track_report_guard.rs:50-75`), which protects the task blocks; it accepts the new
   view fences.
2. Every `track_recipes` row containing `view.live`, updated with the new recipe body
   (`PUT /api/track-recipes/{id}`; user actor only, `track_recipes.rs:121-127`).
3. Retired overlay rows (`spy.overview`, `spy.portfolio`, `spy.decisions`, `spy.fills`,
   `paper.overview`, `paper.activity`, `paper.review_cards`, `paper.*_details`) removed with
   `POST /api/overlays/delete` (`routes/overlays.rs:151-195`). Rows are never collected
   otherwise (`calm-truth/src/db/sqlite/overlay.rs:55-68`).
4. Precondition for deploying S4: a read-only scan through the same APIs reports zero
   `view.live` blocks in all Track reports and zero in all recipes.

Historical `overlay.set` and `track.report_edited` events are not rewritten.

### 3.2 SPY units (`spy.overview` and detail tables replaced)

| Unit kind | Cell | Content, from |
|---|---|---|
| `spy.nav` | metrics | total equity, previous valuation, day P&L, day change (`allocation_views.py:42-68`) |
| `spy.nav_history` | time-series | equity and return datasets with SPY benchmark (`:69-92`) |
| `spy.weights` | distribution | SPY and cash shares (`:97-107`) |
| `spy.weight_history` | time-series | stacked and line weight history (`:108-117`) |
| `spy.holdings` | table | holdings with quote time caption (`:103-105,118-124`) |
| `spy.decision_log` | records | target, state badge, order facts, rationale, sources, fill disclosures (`:128-156`) |
| `spy.account` | metrics | reconciliation time or error (negative tone), quote time, max order step, cash reserve, available cash (`allocation_report.py:181-188`, `allocation_views.py:157-159`) |

`spy.portfolio`, `spy.decisions` and `spy.fills` are deleted (`allocation_report.py:179-195`).
Their facts are now in `spy.account`, `spy.holdings`, `spy.weights` and `spy.decision_log`.
One consequence: fills that do not match a decision's broker id (for example external
activity) are no longer listed in the report. The ledger and `spy.status` keep them.
The static account wording ("长桥官方模拟账户 · USD …") moves to the template's view
description.

The runtime publishes every projection on every poll tick
(`paper_trading/runtime.py:20-24`), and each publish is an event carrying the full
payload (`callbacks.rs:233-236`) plus a Track refetch in the frontend. Going from four to
seven SPY kinds raises that churn, so S3 also skips a publish when a kind's payload hash
is unchanged since the last successful publish.

### 3.3 Supervised `paper.*` views

| View today | Units |
|---|---|
| `paper.overview` (`report_views.py:57-141`) | `paper.account` (metrics), `paper.realized` (bars), `paper.budget` (meter), `paper.notices` (records, publisher empty text instead of a conditional row), `paper.reconciliation` (records) |
| `paper.activity` (`:144-154`) | `paper.activity_log` (records) |
| `paper.review_cards` (`:157-167`) | `paper.review_log` (records) |
| `paper.{alert,strategy,order,trade}_details` (`:170-193,196-219`) | `paper.alert_table`, `paper.strategy_table`, `paper.order_table`, `paper.trade_table` (table) |

`recipe.md:83-117` is rewritten as template views. The legacy `paper.*` tables
(`paper_trading/report.py:26-75`) are not views and are out of scope. If the 4140 scan
finds no reference to them, a follow-up deletes them.

### 3.4 Example and frontend fixtures

The example must remain the real production output of the scripted simulated account
(`examples/build_native_demo.py:1-9`). It becomes template plus units:
`examples/native-demo.json = {"views": [...], "overlays": {"<kind>": <unit>}}`.
`views` are the `view` fences parsed from `spy-recipe.md` with only their descriptions
replaced by the example marker, as `create_view` does today (`build_native_demo.py:170-174`).
`overlays` is `allocation_report.tables(state)` of the scripted run. `native-demo.md` (an
inline paste copy, `build_native_demo.py:193-198`) is deleted: a template with live slots
has no inline equivalent, and materializing one would duplicate the resolver in Python.

Consumers to update: `tests/test_native_demo.py:20-49`;
`fe/tools/report-view/conformance.test.ts:37-39`;
`fe/web/src/features/report/native/native.browser.test.tsx:6`;
`fe/web/src/app/router/report-shell-preservation.browser.test.tsx:8`. The frontend
tests render `views` through `NativeReportView` with a resolver that calls the
production `trackOverlayPayload` over overlay wires built from `overlays`, not a test
copy of the lookup. The shared fixture `test-data/native-view-v1.json`
(`native_view_tests.rs:134-139`) gains a live-slot view plus valid and invalid units.

## 4. Proposed `spy-recipe.md` report body

The contract header becomes:

```
<!-- neige:contract {"version":1,"sections":[{"h1":"组合表现"},{"h1":"资金投向"},{"h1":"调仓决策"},{"h1":"结论"},{"h1":"待你定","omit_if_empty":true},{"h1":"核心逻辑"},{"h1":"关键数据"},{"h1":"风险与证伪"},{"h1":"催化剂与跟踪"},{"h1":"复盘"},{"h1":"来源与边界"},{"h1":"执行记录"}]} -->
```

Planner comment edits (everything else in `spy-recipe.md:2-27` unchanged):

- Execution step (`:13`): the task block "is appended at the end of the Report, in 执行记录".
- Post-close (`:14`) and Report (`:17`): "refer to 组合表现, 资金投向 and 调仓决策" instead of
  组合概览. "No step rewrites 组合表现, 资金投向, 调仓决策 or 执行记录: the first three are
  template views of live App data; 执行记录 holds only the appended execution task
  blocks."
- Rules (`:26`): "Keep the live views and the ledger."

Body (`P` = `neige://plugin/dev-neige-paper-trading`; written compact, recipe
normalization re-renders fences canonically, `track_recipes.rs:33-77`):

````markdown
# 组合表现

```neige-block view
{"version":1,"title":"","description":"长桥官方模拟账户 · USD · 数值为对账估值，盈亏未扣除费用与出入金。","snapshot":null,"rows":[
 {"id":"performance","title":"","layout":"two-wide-end","cells":[
  {"kind":"live","id":"nav","source":"P/spy.nav","cell":"metrics"},
  {"kind":"live","id":"nav-history","source":"P/spy.nav_history","cell":"time-series"}]},
 {"id":"account","title":"对账与规则","layout":"one","cells":[
  {"kind":"live","id":"account","source":"P/spy.account","cell":"metrics"}]}]}
```

# 资金投向

```neige-block view
{"version":1,"title":"","description":"按市值计算，包含现金。","snapshot":null,"rows":[
 {"id":"allocation","title":"","layout":"three","cells":[
  {"kind":"live","id":"weights","source":"P/spy.weights","cell":"distribution"},
  {"kind":"live","id":"weight-history","source":"P/spy.weight_history","cell":"time-series"},
  {"kind":"live","id":"holdings","source":"P/spy.holdings","cell":"table"}]}]}
```

# 调仓决策

```neige-block view
{"version":1,"title":"","description":"Planner 保存的目标比例与券商执行结果。","snapshot":null,"rows":[
 {"id":"decisions","title":"","layout":"one","cells":[
  {"kind":"live","id":"decisions","source":"P/spy.decision_log","cell":"records"}]}]}
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

执行记录 is last because new task blocks are appended at the end of the document
(`track_report.rs:535-565`) and `guard_task_declarations` refuses rewrites that drop
them (`track_report_edit_guard.rs:135`). No routine step names it.

## 5. Slices

Each slice is independently reviewable and green, and is deployed in order. Every slice
runs `scripts/local-ratchet-gates.sh`. Rust runs use the targeted `cargo nextest -p <pkg>`
form from AGENTS.md.

**S1: contract, write validation, frontend rendering** (calm-types, generated artifacts,
`fe/core/domain`, `features/report/native`). This is one slice because the generated
TypeScript union (`fe/core/domain/report-view.types.generated.ts`, drift-checked by
`fe/tools/report-view/generate.mjs --check`) changes with the DTO, and the renderer's
exhaustive cell switch (`native/public.tsx:34-49`) must handle the new variant in the
same change.

- Rust: `DataUnit`, live slot variant, nullable view snapshot with its rule,
  `validate_unit`, schema and TypeScript export; `MAX_LIVE_UNIT_BYTES`.
- Frontend: generated decoder, per-component relations, unit decoder, slot resolution
  with the four read steps, placeholders, snapshot disclosure, empty-title header.
- Tests: calm-types valid and invalid live slots (unknown `cell`, bad source, duplicate
  slot id, snapshot null with inline cell, non-null with none) and units (wrong kind,
  nested live, bad version); shared fixture conformance in Rust and the frontend; a
  frontend unit test with one ok, one pending, one malformed and one wrong-kind slot in
  one view; aggregate budget; a browser test of a two-wide-end row with one degraded
  slot at 1440 and 390 px.
- Mutation verification: (i) per-slot isolation: make any slot failure fail the whole
  composition; predicted red is the mixed-slot unit test and the browser degraded-slot
  test. (ii) Expected kind: drop the `cell.kind == slot.cell` comparison; predicted
  red is the wrong-kind tests in calm-types and the frontend.

**S2: kernel read hydration and Planner guidance** (`track_report_hydrate.rs`, prompts,
golden).

- Per-slot `resolved` summary and full data as in §2.6; `is_overlay_block` extended to
  `view` blocks with live slots (`track_report_hydrate.rs:116-119`).
- Tests: extend `crates/calm-server/tests/cases/mcp_track_report_live_view.rs` for exact
  scoping (another Track's or plugin's overlay with the same kind is not used), pending vs
  storage-unavailable, per-unit and aggregate budgets, one bad slot leaving the others ok,
  `none` performing no query, and the stale-CAS behavior of template views.
- `planner_tool_surface_fits_its_byte_budget` and the registry golden.
- Mutation verification: replace the exact `(entity, track, plugin, kind)` match with
  `(plugin, kind)`; predicted red is the cross-Track scoping test only.

**S3: plugin data units, recipes, example** (`plugins/paper-trading`).

- `allocation_views.py` and `report_views.py` emit units (§3.2, §3.3), delete the SPY
  detail tables, skip unchanged publishes; rewrite `spy-recipe.md` and `recipe.md`;
  regenerate the example; update README; frontend example tests.
- Tests: units validate against the generated schema; the existing projection tests move
  to units; `test_spy_recipe_contract_matches_body_and_published_views`
  (`tests/test_allocation_views.py:224-240`) becomes set equality between recipe slot
  sources and published unit kinds in both directions, with slot `cell` equal to the
  published unit's kind; H1 order equals the contract and the last H1 is 执行记录;
  `build_native_demo.py --check`; `smoke_host.py` (`:78-94`) for template views instead of
  `view.live`.
- Mutation verification: drop one unit (for example `spy.account`) from the published
  set; predicted red is the recipe/published set-equality test and the example equality
  test.

**S4: 4140 rewrite and `view.live` removal.**

- Run §3.1 steps 1-4 on 4140 after S3 is deployed. Commit the script and the scan
  transcript in the PR description. Production restarts follow the machine runbook.
- Remove `KIND_LIVE_VIEW`, `validate_live_view`, the `view.live` discovery entry and
  prompt (`contracts.rs:447-461`, `prompts/report-kinds/view.live.md`),
  `hydrate_live_view` (`track_report_hydrate.rs:183-208`), the frontend `live.tsx`,
  `LiveViewBlockPayload` and its kind branches (`fe/core/domain/report.ts:55-59,228,250`,
  `document/public.tsx:200-241`), and their tests. Update
  `docs/report-live-views.md`.
- Acceptance: `git grep -n "view\.live"` limited to historical design docs; an old
  `view.live` fence is rejected at every write end (one test per entry family: block
  upsert, Replace, recipe, fork); surface budget lowered to the new measurement.

## 6. Risks and open questions

**Risks.**

- Window between the S3 deploy and the 4140 rewrite: the old `view.live` keeps showing
  the last `spy.overview` row (stale, with its own `observedAt`). Keep it to minutes by
  running the rewrite right after the plugin restarts.
- A full `write_markdown` by the Planner can still delete template views
  (`track_report.rs:681-684`). They are recoverable from the recipe, but not
  automatically.
- Up to 18 slots per view (6 rows × 3) means more overlay rows, events and frontend
  refetches. This is bounded by the 4 MiB aggregate and reduced by publish-on-change.
- Planner surface headroom is 16 bytes until S4.

**Open questions for the owner.**

1. Should the kernel refuse non-user edits to `view` blocks containing live slots
   (template-owned blocks), or stay with recipe guidance plus section-scoped writes as
   proposed? A guard is a new authority rule in the report funnel.
2. Is it acceptable that the SPY report no longer lists fills unmatched to a decision
   (§3.2), or should `spy.decision_log` gain a separate "other fills" record set?
3. Should live `table` blocks later fold into live slots (one live-data mechanism, needs a
   rewrite of `plugins/market` and `plugins/barra` Tracks on 4140), or stay as a released
   second form?
