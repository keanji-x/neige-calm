# One inert presentation grammar, two delivery mechanisms

Inline `view` stores a bounded NativeView composition in the report. `view.live`
stores only `{source, version}` and reads the same composition from a Track plugin
overlay. Both use the same renderer, component vocabulary and structural contract.
Neither grants execution, order, approval, navigation or scheduling authority.

## Ownership

`calm-types/src/report_blocks/native_view/model.rs` owns the DTOs and structural
constraints. The real exporter derives the crate-owned JSON Schema and frontend
TypeScript declarations from them. `fe/tools/report-view/generate.mjs` emits the
client structural decoder from that schema. Backend discovery consumes the
crate-owned schema; it never imports frontend source or tooling.

Both sides independently check relations the structural schema cannot express:
unique identities, row width, increasing real UTC dates, series/sample width,
complete nonnegative stacks, declared table keys and at most one primary metric.
Client decoding rejects reserved object keys before normalization.

## Publish a live composition

````text
```neige-block view.live
{"source":"neige://plugin/operations/capacity","version":1}
```
````

The existing overlay scope selects exact Track, plugin and kind. Its value is a
NativeView root: version, title, description, snapshot and rows. Components are
metrics, time-series, distribution, table, records, bars and meter. Sources name
content, not application-specific preset renderers. Reading never invokes a
plugin tool, changes the report or approves anything.

Publishers own values, labels, units, tones, composition and business meaning.
Generic records carry subtitle, title, summary, labeled badges, facts, sections
and disclosures. These collections may explicitly be empty. The platform does
not require a finding/handling workflow or determine whether evidence refutes a
thesis. Disclosures have publisher labels; dates can be included in those labels
or facts when appropriate, without imposing evidence-date semantics on records.

Snapshot timestamps are required nullable fields: unknown is null, not an
invented zero. App publications distinguish observed source time from actual
projection creation time; if the latter is unavailable it remains unknown.

## Separate resource policies

Persisted inline blocks retain the released kernel formatter and exact256 KiB
canonical write budget. Live overlays retain a4 MiB compact UTF-8 transport cap.
The same structural validator runs on both, but the persisted canonical budget
is not applied to overlays. Generic record bodies support8,000 Unicode code points
and100 items, preserving existing activity and review histories.

The frontend has its own4 MiB decoded JSON budget plus bounded shape. It does not
estimate Rust float spellings or pretty JSON size, and cannot decide exact write
admission. Rejected writes remain the kernel's decision.

`neige.report.read` emits source, version, resolved_at and
`validation: "presentation"`. `ok` certifies bounded structure/version, not truth
of publisher facts or financial/account authority. Full adds `data`; none skips
hydration. Missing overlays are pending; malformed data, storage errors and
oversized transport are unavailable.

## Compatibility

This consolidation changes only new view contracts on unmerged PR #1770. Replace
experimental preview content and regenerate its Recipe/demo explicitly before
release. There is no preset adapter, shape sniffing, read-time rewrite or silent
fallback. Released table, chart, app and preview blocks and database migrations
are unchanged. Legacy nullable/large table overlays keep their read contract.
