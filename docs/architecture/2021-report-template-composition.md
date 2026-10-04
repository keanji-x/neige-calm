# Report template composition: the template places, the plugin publishes data units

Baseline: `origin/main` 54b79918c. Every `file:line` below was read on that tree.
Status: design, revision 9 (review rounds 1-3, owner decisions, S3 corrections, the S4
code slice and the executed 4140 rewrite folded in, §7). The design PR changed no code;
S0-S4 are implemented. S4 (#2078) was merged and deployed directly, and the §3.1 rewrite
ran on 4140 on the S4 build, deletes first (2026-10-04).
Owner decisions (2026-10-04, final): no kernel guard for template views; live `table` and
`chart.series` stay single-block references; the supervised paper profile is deleted
(slice S0); S4 merges and deploys directly. No owner question remains open.

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
pub struct DataUnit { snapshot: Snapshot, cell: Component }
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
{"snapshot": {"id": "<content hash>", "observedAt": 1790798340000, "producedAt": null},
 "cell": {"kind": "metrics", "id": "nav", "title": "", "items": [ ... ]}}
```

- `cell` is validated by the existing `Component::validate` (`native_view.rs:69-255`).
- `cell.id` is **unit-local**: the publisher's identity for its cell, validated like any
  id, never compared across units. The **slot id** is the composition identity: it keys
  React elements and inspection state (`fe/web/src/features/report/native/public.tsx:12,20-27`
  key plots, distributions and records by component id today; for a live slot the key
  becomes the slot id), and it is what uniqueness across a view checks
  (`native_view.rs:274-277`).
- The envelope has no version field. A unit kind never reuses a retired overlay kind name,
  so each overlay key keeps one shape; a future shape change publishes under a new kind
  and the template points at it. That is the one evolution mechanism.
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
  normal state. Rust write validation keeps calling `validate_live_source`
  (`kinds.rs:121-150`). The DTO also carries the rule as a schema `pattern`
  (`^neige://plugin/[A-Za-z0-9._-]+/[A-Za-z0-9._-]+$`, the pattern `view.live` already
  publishes at `track_report_blocks/contracts.rs:455`), which the frontend generator
  compiles (`fe/tools/report-view/generate.mjs:70`). The pattern is a copy of the Rust
  rule; the shared fixture's `slot-bad-source` entry pins that both sides agree. Copies
  today: three literals in `track_report_blocks/contracts.rs` (`:100,191,455`) and
  `LIVE_TABLE_SOURCE_PATTERN` in `fe/core/domain/report.ts:45`, which also serves live
  tables and `chart.series` (`:49,56,68`). S1 subtracts on the Rust side: one
  `LIVE_SOURCE_PATTERN` const beside `validate_live_source`, used by the three
  `contracts.rs` schemas, and a calm-types test that the exported `LiveSlot.source`
  pattern equals it. On the frontend `LIVE_TABLE_SOURCE_PATTERN` stays, because it owns
  live table and `chart.series` decoding, which the native-view generator does not; S1
  adds one assertion that it equals the generated `LiveSlot.source` pattern.
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
`routes/track_recipes.rs:95-108`, `routes/tracks.rs:599,1786,1832`). Overlay writes stay
opaque (`callbacks.rs:210-213`, `calm-truth/src/validation.rs:489-491`).

**Read time (each live slot independently, in row and cell order).**

1. Exact overlay lookup by Track, plugin and kind. None: `pending`. Storage error: the
   whole block is `unavailable`, as today (`track_report_hydrate.rs:75-84,108`).
2. Compact UTF-8 size of the unit at most 4 MiB: today's `MAX_LIVE_VIEW_BYTES`
   (`kinds.rs:31-32`), reused as is through S1-S3 and renamed `MAX_LIVE_UNIT_BYTES` in S4
   when `view.live` goes; the frontend keeps its own
   decoded-resource budget (`fe/core/domain/report-view.ts:8,11-21`), as today. The largest
   SPY unit is bounded well under the cap: `spy.decision_log` holds at most 50 decisions
   (`allocation_views.py:18,154`), each with a rationale of at most 6,000 characters
   (`allocation.py:82-83`), sources bounded to 8,000 (`allocation_views.py:143`) and at most
   20 fill disclosures (`:147`), so roughly 2.4 MB at 3 bytes per character; the
   time-series units hold at most 260 samples (`allocation.py:20`). There is no per-view
   aggregate budget: it would be one more rule duplicated in Rust and TypeScript.
3. Decode as `DataUnit` (envelope shape), `cell.kind == expects`,
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

- `neige.report.read` shows the template fence in `text` (about 1 KB per view) and, for a
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
  `neige.report.kinds` result) explains live slots as template-owned references;
  `prompts/tools/neige.report.read.md` names view live slots. The Planner tool surface is
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
- **Why the deletes go first:** on a build without `view.live` (S4), a stored `view.live`
  block reads as an unknown kind and a user block DELETE removes it (§7 revision 8), but
  that Track's Replace, full `write_markdown` and fork are refused
  (`validate_body_fences` at `track_report.rs:648`, `:684`, `routes/tracks.rs:1832`).
  Replace before the deletes is refused by the stomp guard (`track_report_guard.rs:52-73`),
  which requires every existing non-prose block byte-identical, so the retired blocks are
  deleted first; the #2078 reviewer verified this against the real 4140 report bytes. The
  frontend shows a leftover block as `unsupported` (`fe/core/domain/report.ts:259-262`).
  Section replaces of other sections still work, because `sections.rs:113` validates only
  the replaced section.

**Scan.** A read-only query over a **copy** of the 4140 database, so hidden area-chat
Tracks and Tracks outside visible areas are covered too:

```sql
SELECT track_id, id FROM cards WHERE kind = 'track-report' AND payload LIKE '%view.live%';
SELECT id, title FROM track_recipes WHERE body LIKE '%view.live%';
```

Result before the rewrite: three Tracks, each with one `view.live` and three live tables
(`spy.portfolio`, `spy.decisions`, `spy.fills`): the live SPY Track
`7c0dd087ed9c4f7895ce4c6bb574c327` and the closed Tracks `2800396680…` and `1144b00f…`;
and one recipe, `d2a8d568…` "SPY 与现金 · 每日例程" (the `spy-recipe.md` body: one
`view.live` and three live tables).

**Executed on 4140 (2026-10-04).** The owner decided to merge S4 (#2078) and deploy it
directly. 4140 was upgraded from 9dd793a9b to 60796fd78 by a preserving restart; the
paper-trading plugin was switched to the S3+ source first, so the kernel restart loaded
the new manifest (the stale-manifest hazard of §3.3). Once the eight §3.2 units were
published, the procedure below ran on the S4 build.

**Procedure (delete-first, on a build with S3 and S4).** To repeat it on another install,
run the scan there and use its ids.

1. Timing: outside the SPY Calendar windows (New York time Mon-Fri 08:45-11:00 and
   16:30-17:30, Sat 10:00-11:00, `spy-recipe.md:6-9`), with the Planner idle and no
   decision about to be submitted: only `submitting` flips on restart
   (`ledger.py:50-53`), and `queued` and `requested` are the states that lead into it
   (`working` is already acknowledged by the broker and survives a restart). The operator
   cannot call `spy.status`: it refuses callers that
   are not a Planner or Worker (`allocation.py:51-54`, pinned by `test_allocation.py:328`).
   Instead, a read-only query on the ledger:

   ```sh
   sqlite3 'file:<plugins-data-dir>/dev-neige-paper-trading/spy-cash/ledger.sqlite3?mode=ro' \
     "SELECT id, state FROM decisions WHERE state IN ('queued','requested','submitting')"
   ```

   (ledger path: `allocation.py:35`, `ledger.py:59`). It must return no row.
2. Deploy: switch the plugin to the S3+ source, then restart the kernel on the S4 build,
   and wait until the live Track has all eight §3.2 unit overlays.
3. Live Track deletes: `GET /api/tracks/{id}/report` and record each block's `rev`; then
   `DELETE /api/tracks/{id}/report/blocks/{block}` with `ifBlockRev` for the `view.live`
   block and each retired live table, back to back. On 4140: `b_ec8d` and the three live
   tables on `7c0dd087…`.
4. Replace: `GET` again and record `summary` and `docRev`; then `POST
   /api/tracks/{id}/report` with `ifDocRev`, that `summary` (required,
   `routes/tracks.rs:3235-3246`) and the §4 `spy-recipe.md` report body (更多明细 becomes
   执行记录). Any non-prose block still on the Track (task blocks, research `table` and
   `chart.series` blocks) goes in byte-identical, in its original relative order, or the
   stomp guard refuses the write.
5. Recipe rows from the scan: `GET /api/track-recipes/{id}` first and record its `title` and
   `revision`; then `PUT /api/track-recipes/{id}` (user actor only,
   `track_recipes.rs:121-127`) with that same `title`, the new `body` and `if_revision` set
   to the recorded `revision` (`UpdateRecipeBody` requires all three; a stale revision is a
   409, so re-GET and retry). For `d2a8d568…` the body is the §4 `spy-recipe.md`, which
   recipe ingress normalizes (`track_recipes.rs:33-77,199-200,230-231`); on 4140 the
   title was kept and `if_revision` was 4, because migrations 0134 and 0135 had bumped it.
6. Retired overlay rows (`spy.overview`, `spy.portfolio`, `spy.decisions`, `spy.fills`)
   deleted with `POST /api/overlays/delete` (`routes/overlays.rs:151-195`). Short of
   uninstalling the plugin (`overlays_clear_by_plugin`, `db/mod.rs:776`, called at
   `plugin_host/lifecycle.rs:218`), nothing else removes them; entity deletes sweep by
   entity (`calm-truth/src/db/sqlite/overlay.rs:55-68`).
7. Closed Tracks: `DELETE` of the `view.live` block only, with `ifBlockRev`, no rewrite.
   Their live tables stay valid `table` blocks and show "nothing pushed" once the retired
   overlays are gone. The REST block write has no closed-Track refusal
   (`routes/track_report_blocks.rs:121-147` → `ReportEditTarget::resolve`,
   `track_report.rs:956-963` → `rest_user_block_op`); on 4140 it removed the block on
   both closed Tracks.
8. Scan again; both queries must return no row, and no retired overlay may reappear.
   Also scan Planner cards for `template_context` copies that still name `view.live`
   (#2098).

Failure handling: a refused delete, Replace or recipe PUT changed nothing; re-read the
revisions and retry that step. A Track whose deletes succeeded but whose Replace has not
yet run is a valid report with the old instructions in block 0.

**Result on 4140.** The second scan found 0 recipes and 0 report blocks containing
`view.live`, and the retired overlays were not republished. The remaining hits were
Planner `template_context` copies (#2098); the SPY Planner card's copy was fixed by an
owner-approved one-off DB edit before a Planner reset. A read-only Planner probe then
confirmed that `spy.status` and `neige.report.read` succeed and all eight slots resolve
`ok`.

**The Planner wake this causes.** User edits wake the Planner
(`PLANNER_WAKE_AUTHORS`, `dispatcher/mod.rs:51-52,128`); steps 3, 4 and 7 deliver
`track.report_edited` observations rendered as "information, not an instruction to
re-read" (`calm-types/src/observation.rs:285-288`). The step-3 wakes still read the old
block 0 ("Keep the live views", `spy-recipe.md:26`), but on the S4 build no write end
accepts a `view.live` fence (§7 revision 8), so a wake cannot re-add it; run step 4 right
after step 3. From step 4 on, block 0 holds the new instructions, which run steps only
for a Calendar wake or an explicit user request (`spy-recipe.md:11`), so the expected
turns are no-ops. The new recipe adds one sentence to make that explicit: "A user edit of
this Report requests no step." The operator checks the Track timeline afterwards: no
report write, no `spy.*` write tool call.

Historical `overlay.set` and `track.report_edited` events are not rewritten.

### 3.2 SPY units (`spy.overview` and detail tables replaced)

| Unit kind | Cell | Content, from |
|---|---|---|
| `spy.nav` | metrics | total equity, previous valuation, day P&L, day change (`allocation_views.py:42-68`) |
| `spy.nav_history` | time-series | equity and return datasets with SPY benchmark (`:69-92`) |
| `spy.weights` | distribution | SPY and cash shares (`:97-107`) |
| `spy.weight_history` | time-series | stacked and line weight history (`:108-117`) |
| `spy.holdings` | table | holdings; the caption opens with the exact 实际 SPY 比例, then the quote time (`:103-105,118-124`) |
| `spy.decision_log` | records | target, state badge, order facts, rationale, sources, fill disclosures (`:128-156`); newest 50 decisions (`:18,154`), at most 20 fill disclosures each (`:147`) |
| `spy.fill_log` | table | every fill in `state['fills']`, the same newest 200 fills `spy.fills` shows today (`allocation.py:230`, `allocation_report.py:31-33`), including fills that match no decision |
| `spy.account` | metrics | reconciliation time or error (negative tone), quote time, max order step, cash reserve, available cash (`allocation_report.py:19-26`, `allocation_views.py:157-159`) |

`spy.portfolio`, `spy.decisions` and `spy.fills` are deleted (`allocation_report.py:17-33`).
Not every fact they showed reappears unchanged in the units (corrected in S3):

- Decisions: `spy.decisions` listed every decision in `spy.status` (the newest 200);
  `spy.decision_log` shows the newest 50. Decisions 51-200 remain only in `spy.status`,
  and their fills only in `spy.fill_log` while they are among the newest 200 fills.
- Fill time: `spy.fills` showed the raw broker ISO timestamp; `spy.fill_log` shows New
  York time to the minute, newest first.
- Account mode: the wording "长桥官方模拟账户 · SPY／现金" (`allocation_report.py:18`) is
  recipe context, not data. It moves to the recipe's 来源与边界 guidance, so units never
  claim a broker and the example marker rule (§3.4) still holds.
- Actual SPY ratio: `spy.portfolio`'s 实际 SPY 比例 (two decimals from `actual_spy_bps`) is
  the first fact of the `spy.holdings` caption; the `spy.weights` slices are whole
  dollars, so its legend is not the exact figure.
- Placement: following §4's order, 调仓决策 sits after 组合表现 (with its new 对账与规则
  row) and 资金投向, so at 1440×1000 the decisions start below the first viewport, where the
  old overview showed them in its third row.

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
supervised ledgers, `allocation.py:33-34`). Result on the current 4140 copy: one hit,
the user recipe `d3c0273e…` "Longbridge paper portfolio" (six `paper.*` live tables),
used by no Track. S0 deletes it with `DELETE /api/track-recipes/{id}` (user actor,
`routes/track_recipes.rs:27-29,254-262`). Any further hit at deploy time is deleted the
same way (recipes; blocks with the block DELETE of §3.1; overlays with
`POST /api/overlays/delete`), so no unplanned recipe stays behind; a supervised ledger
file stops S0 for an owner decision.

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
- `broker.py` is folded into `allocation_broker.py`: `Broker` has one subclass and one
  user, `AllocationBroker` (`allocation_broker.py:11-14`). The runner it needs moves
  there (`BrokerError`, `_object`, the environment allowlist, the output cap
  `MAX_OUTPUT_BYTES`, `_run`/`_json` and the constructor checks), and `AllocationBroker`
  gains a `timeout_seconds` constructor argument, production default 30 (today the
  literal at `allocation_broker.py:13`). The CLI order/cancel/read surface (`order_args`,
  `cancel_args`, `identity` through `execute`, `broker.py:53-69,107-108,179-249`) and
  `_records` (`:76`) are deleted with the file, and so is its module docstring
  (`broker.py:1-23`), which describes that CLI surface; the facts still true for the SDK
  path (Decimal token preservation, explicit environment, no retries) move into the
  `allocation_broker.py` docstring.
- `test_broker.py` is re-driven through `AllocationBroker.request`
  (`allocation_broker.py:16-26`). The executable is the test's own `FIXTURE`
  (`test_broker.py:19-53`) passed as `sdk_python_path`; `tests/allocation_fixture.py`
  cannot sleep, flood or fork. The seam for short timeouts is the new `timeout_seconds`
  argument. Kept and re-driven: environment allowlist (`:234`), timeout and reap
  (`:321-338`), combined output bound (`:340-351`), the exact-limit case with a JSON
  object of exactly `MAX_OUTPUT_BYTES` (1 MiB) bytes (`:354-356`), launch failure
  (`:359-363`), Decimal token preservation (`:96-100`), all five non-leak cases
  (`:303-319`: non-zero exit with secret stdout and stderr, not-JSON, invalid UTF-8, NaN,
  duplicate key; the exit path is `broker.py:149-150`), now also asserting that the argv
  secrets `account_no` and `oauth_client_id` (`allocation_broker.py:21`) appear in no
  error, and invalid configuration (`:366-374`, now on the `AllocationBroker`
  constructor). The helpers they rely on move with the runner: `_argument` (`:47`),
  `_json_object` (`:82`), `_invalid_constant` (`:91`). The CLI-only tests go.
- `ledger.py`: deletes `Ledger.reviews` (`:100-112`). The schema stays byte-for-byte: the
  live 4140 SPY ledger created those tables, and dropping unused `CREATE TABLE IF NOT
  EXISTS` lines buys nothing at a persistence boundary.
- `rpc.py`: only the `Allocation` path (`:116-118`); deletes the supervised branch
  (`:119-125`), its imports (`:10-11`, `:13`; `Runtime` at `:12` stays), the
  non-`Allocation` call branch and the
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
  approval, migration, first cycle, recovery and its verification commands) except a
  short SPY verification paragraph that keeps the pytest command and the `jsonschema`
  test-only dependency (today `:291-299`); keeps the example paragraph (`:334-345`) and
  the SPY section (`:346-`), whose comparison with the supervised profile (`:363`) is
  reworded.
- CI: `.github/workflows/paper-trading.yml:38` step name drops "process and
  human-confirmation"; the pytest path (`:39`) is unchanged.

**Deploy S0 only through a kernel restart or `POST /api/plugins/{id}/reload`.** A stored
`cli_path` is harmless: the effective configuration drops keys the manifest does not
declare (`plugin_host/config.rs:7,27-38`). The hazard is a stale manifest: the kernel
keeps the manifest it loaded at boot (`state.rs:1012`) until a reload re-reads it
(`plugin_host/lifecycle.rs:243-327`, route `routes/plugins.rs:42`). A child-only respawn
of the new code under the old manifest would receive the old `cli_path` default, which
the new exact parse (`allocation_config.py:30`) refuses. The restart-timing guard of §5
applies. The installed plugin is a symlink in `~/.config/neige-calm/plugins`, which the
4040 instance shares, so the S0 symlink swap reaches 4040 on its next restart; that is
acceptable because compatibility covers the 4140 database only.

Historical design docs (`docs/design-paper-trading-loop.md`,
`docs/design-paper-report-hierarchy.md`, `docs/design-spy-cash-rebalance.md:11`) stay as
history. S0 deleted the supervised strategy-configuration design doc, which described only
the deleted profile.

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
gains live-slot view entries in S1 and, by intent, no unit entries: unit validation
parity is pinned by per-language unit and resolver tests (§5 S1).

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
complete predicted red set, per language.

**Planning baselines, not authority.** Per-slice cites, red sets and test names in this
document are planning baselines read at 54b79918c. Each slice re-derives them against
its own base and proves them with its own mutation run and two-channel review; the
document is not the authority for line numbers.

S1, S2 and S4 change Rust and frontend text, so
`scripts/gate-prose-ratchet.baseline.tsv` and `scripts/gate-1316-terminology-ratchet.baseline.tsv`
may need `--update-baseline` as generated artifacts of those slices.

**Restart-timing guard (S0, S3 and every kernel deploy).** A plugin restart while a
decision is `submitting` turns it into `unknown` (`ledger.py:50-53`). Every deploy that
restarts the plugin, directly or through the kernel, runs under the §3.1 step-1
conditions: outside the SPY Calendar windows, and the read-only ledger query of §3.1
step 1 returns no `queued`, `requested` or `submitting` decision. Only `submitting` flips
on restart; the other two lead into it. (The operator cannot use `spy.status`, which
refuses non-agent callers.) If a restart still lands mid-submission,
the outcome is fail-closed: the decision becomes `unknown`, is reconciled against broker
records and is never resubmitted (`allocation.py:213-215`, `allocation_reconcile.py:52-60`).

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
  base-runner tests of `test_broker.py` green through `AllocationBroker.request`; the
  residue-scan recipe deleted on 4140.
- Acceptance: `git grep -n -E 'Portfolio\b|AccountConfig|StrategyConfig|paper\.(strategy|ingest|decide|status|refresh|pause|journal|review)\b|fixture_cli|cli_path|paper_trading\.(engine|strategy|operator|research|reconcile|portfolio|report|broker)\b|from \.broker|supervised' -- plugins/paper-trading .github`
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
  rule, shared `validate_snapshot`, `validate_unit(expects, payload)` capped by the
  existing `MAX_LIVE_VIEW_BYTES`; `generated_schema()` exports `DataUnit`;
  `LIVE_SOURCE_PATTERN` (§2.4). The `view` kind's discovery schema
  (`contracts.rs:463-464`) then also carries the `DataUnit` definitions, which the Planner
  never authors; that is harmless (they are unreferenced from the root).
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
  list. The fixture covers write-time view validation only; it has no unit entries, so
  `expects` mismatches are pinned by the unit and resolver tests below, not by the
  conformance tests. calm-types unit tests cover the slot deserializer's error text and
  units (wrong kind, a `live` cell rejected by the `Component` deserializer). Frontend
  unit tests: "degrades only the failing slot" (one ok, one pending, one malformed slot)
  and, separately, "shows a wrong-kind unit as unavailable" (one ok and one wrong-kind
  slot); empty-title accessible names. One browser test, parametrized over 1440 and 390
  px with `it.each`, renders a two-wide-end row whose degraded slot is pending and
  malformed (never wrong-kind, so it does not overlap the `expects` test). No test for
  live slots inside a `view.live` overlay: that path adds no code (§2.5).
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
  rendering) and both parametrized cases (1440, 390) of the browser "degraded slot keeps
  the row layout" only.
- Mutation, TypeScript: drop the `expects` comparison in the slot resolver. Red: "shows a
  wrong-kind unit as unavailable" only.

**S2: kernel read hydration and Planner guidance** (`track_report_hydrate.rs`, prompts,
golden).

- Per-slot `resolved` summary and full data as in §2.7; `is_overlay_block` extended to
  `view` blocks with live slots (`track_report_hydrate.rs:116-119`).
- Tests (extend `crates/calm-server/tests/cases/mcp_track_report_live_view.rs`): another
  plugin's overlay with the same kind is not used; each slot resolves its own source;
  pending vs storage-unavailable; per-unit cap; one bad slot leaving the others ok;
  `none` performing no query; stale-CAS behavior of template views. The pending and cap
  tests use a single slot. Slots go through the one existing lookup
  (`track_report_hydrate.rs:142-147`), with no new source parser.
- `planner_tool_surface_fits_its_byte_budget` (re-measure; update the cap comment to the
  new number) and the regenerated registry golden.
- Mutation, Rust: drop `overlay.plugin_id == plugin_id` (`track_report_hydrate.rs:145`).
  Red: `live_slot_ignores_another_plugins_overlay_of_the_same_kind` and the existing
  `live_view_hydration_is_scoped_bounded_and_read_only` (its `other` plugin publishes the
  same kind, `mcp_track_report_live_view.rs:211-221`) only. (Mutating the
  entity predicate at `:143-144` is equivalent: `overlays_for("track", track_id)` already
  filters by entity in SQL, `:78-79`.)
- Mutation, Rust: resolve every slot of a view with its first slot's source. Red:
  `live_slots_resolve_their_own_units` and `one_bad_slot_leaves_the_others_ok` only (the
  single-slot pending and cap tests are unaffected).

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
  `test_spy_production_stdio_entrypoint_and_overlays` (`test_allocation.py:312`) waits
  today for the old kinds with no deadline (`:344`): `Host.receive` times out only after
  15 s of silence, and the runtime republishes every kind on each 5 s tick
  (`runtime.py:20-28`, `poll_seconds` 5 at `test_allocation.py:27`). S3 makes it wait for
  all eight unit kinds under a monotonic deadline, the `wait_for` pattern of the same file
  (`:302-309`).
  `test_example_is_the_production_overview_of_the_scripted_run`
  (`tests/test_native_demo.py:20`) is renamed
  `test_example_is_the_production_output_of_the_scripted_run`.
- Mutation, Python: drop `spy.account` from the published units. Red:
  `test_spy_recipe_contract_matches_body_and_published_views`,
  `test_example_is_the_production_output_of_the_scripted_run`,
  `test_spy_production_stdio_entrypoint_and_overlays` (an `AssertionError` at its
  deadline; without the S3 deadline it would hang, not fail), and the two tests whose
  description assertions move to `tables(state)['spy.account']`: the reconciliation-error
  check of `test_overview_is_valid_before_reconciliation_and_after_errors`
  (`test_allocation_views.py:218`) and `test_snapshot_identity_covers_the_description`
  (`:276-278`), each renamed for the unit, only.

**S4: 4140 rewrite and `view.live` removal.**

- §3.1 (scan, live-Track deletes and Replace, recipe, overlays, closed-Track deletes, scan
  again) ran on 4140 on the S4 build right after the S3+S4 deploy (owner decision: merge
  and deploy S4 directly); production restarts follow the machine runbook.
- Rename `MAX_LIVE_VIEW_BYTES` to `MAX_LIVE_UNIT_BYTES` (`kinds.rs:31-32`) and its users.
- Remove: `KIND_LIVE_VIEW`, its `DATA_KINDS` entry, dispatch and `validate_live_view`
  (`kinds.rs:13,42,83,510-524`) and its re-exports (`report_blocks/mod.rs:23-25`);
  `kinds_tests.rs` cases (`:58-76,345`); `live_view_kind` (`contracts.rs:447-461`) and
  `prompts/report-kinds/view.live.md`; the `view.live` wording in
  `prompts/tools/neige.report.kinds.md` and `neige.report.read.md`;
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
  and this document; the regression tests that name the retired kind,
  `crates/calm-server/tests/cases/mcp_track_report_retired_kind.rs`,
  `crates/calm-server/tests/cases/report_retired_kind.rs`, `fe/core/domain/report.test.ts`
  and `fe/web/src/features/report/document/public.test.tsx`; and the Compatibility section of
  `docs/report-live-views.md`, which names it so operators can grep for it. An old `view.live`
  fence is rejected at each write-end family (block
  upsert, Replace, recipe, fork); surface budget re-measured.

## 6. Risks and owner decisions

**Risks.**

- Between the S4 deploy and the §3.1 rewrite a stored `view.live` shows as one
  "unsupported block kind" line. Keep it to minutes by running §3.1 right after the
  restart, in a permitted deploy slot: outside the SPY Calendar windows, under the §3.1
  step-1 conditions.
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
4. S4 merges and deploys directly; the 4140 rewrite runs on the S4 build, deletes first
   (§3.1).

## 7. Revision log

Revision 2 (review round 1, nothing blocking):

- Facts: Planner surface re-derived (28,859/30,000, 26 tools); `allocation_report.py` and
  `native/public.tsx` line references corrected; leftover `view.live` breaks Replace,
  `write_markdown` and fork, not other sections' replaces; ledger binding cited from the
  ledger and call sites.
- Types: `LiveSlot` + `RowCell` with a dispatching deserializer; `DataUnit.cell: Component`
  forbids nesting by type; slot field `cell` renamed `expects`; unit-local cell ids.
- Dropped the per-view aggregate budget and publish-on-change (#1995 follow-up).
- Added the transition fence (superseded in revision 4: no nesting by construction, no
  fence), the `spy.fill_log` unit (fills are no longer lost; former
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
  from the saved GET (superseded in revision 5: Replace first, then deletes; a failed step
  changes nothing or leaves only deletes to retry); 更多明细 becomes 执行记录; research `table`/`chart.series` blocks stay
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

Revision 5 (review round 3; nothing blocking):

- The operator's in-flight check is a read-only query on the SPY ledger (`spy.status`
  refuses non-agent callers), with the fail-closed `unknown` fallback stated.
- Scans sized with real 4140 results: three Tracks with `view.live` (live SPY plus two
  closed Tracks, whose `view.live` blocks S4 deletes without a rewrite, after confirming
  closed Tracks accept a user block DELETE); one supervised recipe that S0 deletes.
- S0 folds `Broker` into `AllocationBroker` with a `timeout_seconds` argument; the
  base-runner tests run against `test_broker.py`'s `FIXTURE` as `sdk_python_path`, keeping
  Decimal, non-leak and configuration cases; the CLI module docstring goes with
  `broker.py`.
- Runbook: Replace first (old blocks byte-identical), then deletes; failure handling is
  "nothing changed" or "retry the remaining deletes" (superseded in revision 9: deletes
  first, on the S4 build).
- Subtracted `DataUnit.version` (new kinds are the one evolution mechanism) and the S1
  rename of the size cap (renamed in S4).
- S3 adds the stdio overlay test to its sweep and red set and states the example test
  rename; S1 and S2 test shapes made explicit (fixture has no unit entries, browser test
  parametrized and never wrong-kind, single-slot pending and cap tests, one lookup).
- Cites: `rpc.py` imports, CI step `:38`, README verification paragraph kept, the exact
  observation text, the source-rule copy, the 4040 symlink note.

Revision 6 (review round 3 follow-up; one blocking item):

- Blocking: the S3 stdio overlay wait gets a monotonic deadline, so the drop-`spy.account`
  mutation fails with an `AssertionError` instead of hanging (`test_allocation.py:344`).
- Planning-baseline statement added to §5: line cites, red sets and test names are
  re-derived per slice.
- Fixture contradiction removed (no unit entries, by intent); recipe `d2a8d568…` recorded
  and its PUT body named; all five non-leak cases plus argv-secret assertions and the
  moving helpers named; Host cite no longer points at a deleted file; source-pattern
  copies: one Rust const plus one equality assertion in each language.
- Nits: `MAX_LIVE_VIEW_BYTES` renamed, not removed; fork prose-fence cite `:1786`;
  overlay collection statement narrowed; restart guard wording; harmless `DataUnit` defs
  in the `view` discovery schema.
- Considered and rejected: deleting the closed Tracks' `view.live` blocks after the S4
  deploy. After S4 a stored `view.live` is an unknown kind, so reading or deleting it could
  fail, and the S4 gate must stay "zero hits anywhere". The rev-5 plan (delete before S4)
  stands. (Superseded in revision 9: S4 was deployed first and the deletes ran on the S4
  build.)

Revision 7 (S3 implementation, #2057):

- §3.2: the claim that every fact of the retired SPY tables reappears in the units is
  corrected (decision cap, fill time format, account mode, exact SPY ratio, placement).
- #2028 wording: §6 deploy timing now says outside the SPY Calendar windows, matching
  §3.1; revision 2's transition fence and revision 4's restore-from-GET are marked
  superseded.

Revision 8 (S4 code slice; the PR merges only after the §3.1 rewrite of 4140 and an empty
pre-merge scan; that merge gate is superseded in revision 9):

- Removed per §5 S4; `MAX_LIVE_VIEW_BYTES` is now `MAX_LIVE_UNIT_BYTES`;
  `mcp_track_report_live_view.rs` is `mcp_track_report_live_slots.rs` (its live-table tests
  stay, renamed `live_table_*`); the records-disclosure checks of the deleted frontend tests
  moved to the inline `view` tests; `docs/report-live-views.md` describes live slots.
- Write ends: an old `view.live` fence is an unknown block kind at block upsert (MCP commit
  and REST create/update), Replace (REST, and the Planner's whole-document write), recipe
  ingress (create and update) and fork. Tests through those entry points pin the exact error
  text (`mcp_track_report_retired_kind.rs`, `report_retired_kind.rs`).
- Read of a stored `view.live` (a block the rewrite missed): it reads as an unknown kind.
  `neige.report.read` lists it without `resolved` while the rest of the report hydrates, the
  frontend shows one "unsupported block kind" line, other block writes beside it succeed and
  a user block DELETE removes it. This corrects revision 6, which expected reading or
  deleting it could fail; neither does. The revision-5 plan (delete before S4) stands,
  because that Track's Replace, whole-document write and fork would still be refused.
  (Superseded in revision 9: the deletes ran on the S4 build, before the Replace.)
- Planner tool surface: 29,909 of 30,000 bytes across 30 tools (the real test, on
  origin/main 5263e7159), down 70 bytes from that base's 29,979 (same method over its golden
  and prompt files); the cap comment records the new number.
- #2069: `spy_recipe_slots.rs` decodes each fence as `NativeView` and keeps
  `RowCell::Live`; the Python `UNIT_KINDS` list is derived from the recipe
  (`tests/recipe.py`, which also replaces the two test copies of the view-fence regex; the
  example builder keeps its own, compared against it by the example test); the status line,
  the §3.2 holdings row and §3.1 step 6 (GET first, keep `title`, pass `if_revision`;
  step 5 since revision 9).

Revision 9 (executed 4140 rewrite, #2099):

- The owner decided on 2026-10-04 to merge S4 (#2078) and deploy it directly, so the
  merge gate of revision 8 and the "delete before S4" plan of revisions 5, 6 and 8 no
  longer hold.
- §3.1: the Replace-first runbook is replaced by the delete-first procedure that ran on
  4140 on the S4 build (60796fd78): live-Track block deletes, Replace, recipe PUT,
  overlay delete, closed-Track deletes. Replace before the deletes is refused by the stomp
  guard; the #2078 reviewer verified this against the real 4140 report bytes.
- §3.1 records the result: no recipe or report block contains `view.live`, the retired
  overlays were not republished, the Planner `template_context` copies are #2098, and a
  Planner probe resolved all eight slots `ok`.
- Status line, §3.3, §5 S4, §6 risk and owner decision 4 updated to match.
