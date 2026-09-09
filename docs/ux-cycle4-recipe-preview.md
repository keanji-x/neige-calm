# UX cycle 4: render the saved Recipe as a native Report

Status: both independent reviews and candidate GUI acceptance passed on 2026-09-09.

Refs #1595. Outcome: after Save, a Recipe shows native report sections, configured
table headers, chart types and honest empty states. The same saved revision
instantiates the same ordered block payloads in a new Track. Edit remains the
existing source editor. This cycle adds no cash, trade, quote or template-copy
workflow.

The new GET /api/track-recipes/{id}/preview reads the saved row once, optionally
checks if_revision (409 on mismatch), validates its stored fences and calls a
narrow wrapper around tracks::prepare_initial_report_payload. It returns id,
revision and the compiled TrackReportPayload. It creates no Track, card, task,
plugin request or event. Preview block IDs are ephemeral compilation identities;
comparison with a created Track uses ordered kinds/payloads and projected body,
not the new Track's freshly assigned IDs.

The client uses the shared core payload decoder, never a fake Card or another
fence parser. The editor requests the revision returned by Save. Preview state
is keyed by id+revision, aborts/ignores stale requests, and refuses mismatched
response metadata. Conflict refreshes the existing Recipe list so closing and
reopening actually gets the current row. Failed reads are visible and retryable;
the source editor remains available. Drafts and conflict drafts are not compiled
or substituted for saved responses.

ReportDocument gains an explicit preview display mode that suppresses live,
file, navigation and execution context, including supplied callbacks, and stops
app iframe loading at the component boundary. Tasks show their saved declaration
only. Existing native Report behavior remains the default. The existing Chart
primitive owns labelled, height-preserving empty states for both template and
empty Track views; no demonstration observations are invented. Inline saved
chart/table data remains renderable.

Owner approval: root approved only the generated core/api preview contract in
fe/core/api/generated/openapi.json and corresponding legacy generated outputs
web/src/api/openapi.json and web/src/api/generated.ts. No manual generated edits,
new global style contract, transport interface or frozen query key changes.
Required trailer:
OWNERSHIP-CHANGE: fe/core/api/generated/openapi.json — Generate the read-only saved recipe preview API for native report rendering (#1595)

Verification starts with the real missing endpoint and current JSON-pre preview
reproductions, then covers revision races, failed/invalid/unsupported reads,
read-only resource/execution boundaries, native Report regressions, and equality
with the real same-revision Track create path. API generation is run for both
consumers. Independent fixed-source reviews and GUI use follow.

Build isolation: /tmp initially has 188GB free. Copy only the prior private
45GB debug and 6.7GB release caches into a new private target, without hardlinks.
Prior runtime bundles and absolute debug helper paths/hashes remain untouched.

Final acceptance of `30e489bde`:

- Both reviewers accepted the full final diff, including the three generated
  API consumers and the exact ownership trailer. Each separately ran the Rust
  archive: 23/23 passed. Their independent frontend, browser and capability
  checks also passed. The archive SHA256 is
  `85c24fb1d361f1663a9b641b1e3e67a52edf434fd2e4cd6efd8a11e990ba22b6`.
- The author ran 3113 frontend tests (one existing skip) before API generation.
  After both real generators, 99 API/domain/router regressions, consumer
  typechecks, lint/build and quick Rust preflight passed, with no OpenAPI drift.
  The complete frontend suite and its OpenAPI wire-coverage guard were not rerun
  after generation. Cycle 5's complete suite exposed that gap: the server-local
  RecipePreviewResponse lacked its required exact response exception. A separate
  validation correction registers that DTO under the guard's existing rule,
  documents its actual core/domain/recipe-preview.ts decoder and checks both the
  exact name and a rejected Unexpected suffix. The validator, schema and
  generators are unchanged; this was a missed post-generation gate, not a new
  research-link regression.
  The 415 browser tests passed. A lazy-chart assertion now waits up to five
  seconds; its assertion and production behavior were unchanged, and the changed
  case was independently rerun.
- Five mutation checks produced exactly the predicted failures: preview-mode
  capability protection (three), Edit-draft protection (one), response identity
  validation (three), saved revision validation (one), and compiled block output
  (two). Original source was restored and the affected tests returned to green.
- The primary agent saved a starter through the actual Recipes UI. Its saved
  view displayed two native tables, no layout JSON code blocks or iframe, and
  labelled empty charts at their configured heights without invented series.
  The candidate had no Market plugin installed. Preview did not create Tracks
  or request plugin resources/tools.
- Through Edit and Save, the agent changed the template title and moved the
  transaction log before the overview. The new saved revision rendered that
  order. The agent then explicitly created a Track through New track and sent
  only a naming/no-op request. Its ordered block kinds/payloads, body, summary
  and schema matched the preview; the real Track payload survived that turn and
  browser reload unchanged. Compilation-local block identities were not treated
  as persistent Track identities.
- This cycle does not fix research-URL parsing or cash records. Those observed
  workflow gaps remain separate followups.
