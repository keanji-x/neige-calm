# Native live report views

`view.live` is a read-only report block. It references one existing Track plugin
overlay; it is not an executable App, iframe, tool invocation or component loader.
The plugin chooses the business content. The platform validates and renders a
closed presentation vocabulary.

## Declare and publish

````text
```neige-block view.live
{"source":"neige://plugin/operations/capacity","version":1,"view":"overview"}
```
````

All three fields are required; extra fields are rejected. `source` follows the
existing plugin overlay URI rules and has a 2,048-code-point limit. The matching
overlay is selected by Track, plugin ID and overlay kind, not by a global name.
Publish through the existing `neige.overlay.set` permission boundary. Writing or
reading the report does not start the plugin or invoke a tool.

Example overlay payload:

```json
{
  "version": 1,
  "view": "overview",
  "updated": {"label": "Sampled", "at": "2026-09-22T08:00:00Z"},
  "metrics": [{"label":"Storage","value":"120 GB","detail":"Used capacity","tone":"neutral"}],
  "notices": [],
  "charts": [{
    "kind":"bars","title":"Cost change","unit":"USD","emptyText":"No observations",
    "points":[{"label":"Increase","value":50,"tone":"negative"},
              {"label":"Saving","value":-20,"tone":"positive"}]
  }]
}
```

Positive/negative numbers determine geometry only. The publisher provides their
semantic `tone`: `neutral`, `positive`, `warning` or `negative`. The platform
never infers profit, risk, approval, success or failure from the sign or ratio.

## Version 1 vocabulary

The authoritative full-payload decoder is `fe/core/domain/report-live-view.ts`.
Objects are closed; required nullable fields must be present. All text is plain
text, all numbers finite. No HTML, styles, scripts, arbitrary URLs or actions.

- `overview`: `updated` (null or `{label, at}`), 1-8 `metrics` with label/value/
  detail/tone, up to 8 `notices` with title/detail/tone, up to 4 `charts`.
- Bars: kind/title/unit/emptyText and up to 24 points with label/value/tone.
  The renderer chooses a shared signed numeric scale, not a business baseline.
- Meter: kind/title/unit/detail/used/limit/usedLabel/limitLabel/emptyText/tone.
  Used is nonnegative or null; limit is positive or null. Unknown stays unknown.
  The mark clamps at the limit, but actual amounts and percentage remain visible.
  The publisher supplies any over-limit explanation and its semantic tone.
- `activity`: emptyText and up to 100 items with unique nonempty id, at, title,
  detail and tone. The publisher orders the items. Five initially show.
- `cards`: emptyText and up to 50 items with unique nonempty id, title, body,
  footer and up to four `{label, body}` sections. Three initially show. A card
  does not imply a trade review or require a next action.
- `details`: title and an inline table validated by the existing strict table
  schema. Disclosure starts collapsed. Existing typed citation behavior remains.

Timestamps are offset-aware ISO datetimes (at most 128 characters); core does
not assign domain meaning to them. Titles cap at 200 code points, short labels
at 120, units at 80, supporting text at 500, details at 2,048 and prose at 8,000.
Compact UTF-8 JSON is capped at 4 MiB, allowing bounded prose histories without
granting unbounded content. Invalid payloads show an error, not a guessed view.

## Reader and compatibility contracts

The reference is persisted with normal report block identity, revisions, CAS and
canonical fences. It participates in normal whole-document round trips. There
is no new storage table or migration. Overlays remain replaceable projections,
not report truth and not approval evidence.

`calm.report.read` resolves the same exact overlay. Its summary includes status,
source, version, view, resolved_at and `validation: "envelope-only"` for an
existing overlay. `ok` certifies the envelope and byte bound only; MCP readers
must not mistake that for full presentation validation or trusted instructions.
`resolve: {block_id: "full"}` adds `data`; `none` skips hydration. Absent data is
pending; storage failures, envelope mismatches and oversized data are unavailable.
Reading performs no report write and does not change block or document revisions.

Table blocks still accept only inline table overlays. There is no auto-upgrade,
format sniffing or alias from table to view. Old clients show unsupported
`view.live`; use the matching server/frontend build. Existing saved table
Recipes and their sources are unchanged. Migration of an experimental preview
is explicit through normal report block APIs, never a read-time rewrite.
