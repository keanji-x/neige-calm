# One inert presentation grammar, composed by the template

A `view` block stores a bounded NativeView composition in the report. Its cells
are either inline components or **live slots**: references to one plugin data
unit each, resolved when the report is read. The template (a Track recipe or a
builtin template body) owns the headings, rows, layouts and slot order; the
plugin owns the data, labels, units, tones and empty states of each unit.
Neither grants execution, order, approval, navigation or scheduling authority.

Live `table` and `chart.series` blocks stay single-block live references; they
are not live slots. The earlier whole-view overlay kind was removed in #2021 S4
(see `docs/architecture/2021-report-template-composition.md`).

## Ownership

`calm-types/src/report_blocks/native_view/model.rs` owns the DTOs and structural
constraints: `NativeView`, `RowCell` (`Inline(Component)` or `Live(LiveSlot)`),
`LiveSlot` and `DataUnit`. The real exporter derives the crate-owned JSON Schema
(`native_view.schema.json`, which exports `DataUnit` next to `NativeView`) and
the frontend TypeScript declarations from them. `fe/tools/report-view/generate.mjs`
emits the client structural decoders from that schema. Backend discovery
consumes the crate-owned schema; it never imports frontend source or tooling.

Both sides independently check relations the structural schema cannot express:
unique identities (slot ids included), row width over inline cells and slots,
the snapshot rule, increasing real UTC dates, series/sample width, complete
nonnegative stacks, declared table keys and at most one primary metric. The
shared fixture `test-data/native-view-v1.json` pins that both languages reject
the same invalid views. Client decoding rejects reserved object keys before
normalization.

## Place a live slot (template)

````text
```neige-block view
{"version":1,"title":"","description":"","snapshot":null,"rows":[
 {"id":"capacity","title":"","layout":"two","cells":[
  {"kind":"live","id":"summary","source":"neige://plugin/operations/capacity.summary","expects":"metrics"},
  {"kind":"live","id":"history","source":"neige://plugin/operations/capacity.history","expects":"time-series"}]}]}
```
````

- `source` is the two-segment overlay URI `neige://plugin/<plugin_id>/<overlay_kind>`,
  shape-checked at write time; existence is not checked, because an uninstalled
  plugin is a normal state.
- `expects` is the cell kind the template laid out for. A unit of another kind
  is unavailable in that slot, so a publisher change cannot silently break the
  layout.
- The view `snapshot` describes its inline cells: it is null exactly when every
  cell is a live slot.
- The slot id keys rendering and inspection state and is unique across the view;
  a slot has no title of its own (the unit's cell carries the publisher label).

Views with live slots are template-owned. The Planner reads them but does not
author, move or rewrite their slots.

## Publish a data unit (plugin)

The plugin publishes one overlay per unit on the Track through the unchanged
`neige.overlay.set` path:

```json
{"snapshot": {"id": "capacity-r1", "observedAt": 1790798340000, "producedAt": null},
 "cell": {"kind": "metrics", "id": "capacity", "title": "Capacity", "items": [
  {"id": "free", "label": "Free", "value": {"state": "text", "text": "512 GB"},
   "detail": "", "tone": "neutral", "emphasis": "normal"}]}}
```

`cell` is one `Component`, validated like an inline cell; a unit cannot contain
a live slot. `cell.id` is unit-local. Snapshot timestamps are required nullable
fields: unknown is null, not an invented zero. App publications distinguish
observed source time from actual projection creation time. A unit kind never
reuses a retired overlay kind name; a shape change publishes under a new kind.

Publishers own values, labels, units, tones and business meaning. Generic
records carry subtitle, title, summary, labeled badges, facts, sections and
disclosures; these collections may explicitly be empty. The platform does not
judge staleness: whether data is too old is publisher meaning, expressed as
tone or text.

## Resolution and degradation

Each slot resolves on its own, in row and cell order:

1. Exact overlay lookup by Track, plugin and kind (never by plugin name alone).
   None: pending. Storage error: the whole block is unavailable.
2. Compact UTF-8 size at most `MAX_LIVE_UNIT_BYTES` (4 MiB).
3. Decode as `DataUnit`, `cell.kind == expects`, then the component's own
   validation.

A failure degrades only its slot; the row keeps its layout and the slot shows a
placeholder ("waiting for …", "cannot be displayed: …", or "this view does not
carry live data" where no resolver is injected). Reading never invokes a plugin
tool, changes the report or approves anything.

`neige_report_read` gives a view with live slots
`resolved = {status: ok|partial, validation: "presentation", cells: [{id, source, status, observed_at?, resolved_at?, reason?}]}`.
`ok` certifies bounded structure, not the truth of publisher facts or any
financial/account authority. `resolve: {<block id>: "full"}` adds each ok slot's
unit as `data`; `"none"` skips the overlay query.

## Separate resource policies

Persisted blocks, templates included, keep the released canonical formatter and
the 256 KiB canonical write budget. Units keep the 4 MiB compact read cap per
unit; there is no per-view aggregate budget. The same component validator runs
on both, but the persisted canonical budget is not applied to units. Generic
record bodies support 8,000 Unicode code points and 100 items.

The frontend has its own 4 MiB decoded JSON budget plus bounded shape
(`fe/core/domain/report-view.ts`, `resolveLiveSlot`). It does not estimate Rust
float spellings or pretty JSON size, and cannot decide exact write admission.
Rejected writes remain the kernel's decision.

## Compatibility

The whole-view overlay kind `view.live` was removed. Every write end refuses an
old `view.live` fence as an unknown block kind (400 or `-32602`): block upsert,
Replace, whole-document and section writes, recipe create and update, and fork.
A body already stored outside those write ends fails closed when it is
instantiated: a stored recipe row makes track create fail with a 500, and a
site template file is refused when the template roster loads, so the kernel
does not boot with it.

A report that still stores a `view.live` block reads without failing:
`neige_report_read` lists it without `resolved`, the frontend shows one
"unsupported block kind" line in its place, and a user block DELETE still
removes it. Search for leftovers with `view.live` (for example in the
`track-report` card payloads and `track_recipes` bodies). There is no preset
adapter, shape sniffing, read-time rewrite or silent fallback.
