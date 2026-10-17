# Report preparation and rendering

`preparation.ts` prepares immutable report data using the existing core Markdown,
heading, excerpt and reference algorithms. One report-local exact-string reader
shares parse results across metadata, source-slice excerpts and prose. Heading IDs
are still derived per block, and reference context still belongs to the entire
report. Failed/limited input retains its existing fail-soft rendering.

`PreparedProseBlock` renders prepared syntax with current navigation callbacks,
file roots and preview resources. It retains no such capabilities in preparation.
The public standalone `ProseBlock` uses the same entry and memoizes only its
syntax inputs. `ReportDocument` prepares locally unless its owner supplies the
preparation for that exact report identity; a mismatched input is an error.

`createReportPreparationCache` holds pure prepared data under weak report-card
keys. The app creates one instance per signed-in router scope. Query updates must
replace immutable card objects; identity is already the query/React memoization
contract. Changes to another card or Track metadata do not invalidate the report.
The cache adds no strong card inventory, TTL, application IDs or DOM keep-alive.
It cannot cap memory still owned by reachable query data.

`source(cards)` exposes a stable cheap canonical input for an immutable report
card. It does not parse Markdown. `peek(source)` reads completed preparation only;
`prepare(source, execute, signal)` runs an injected `ReportPreparationPort` on a
miss and caches success only while its lease remains active. Missing/invalid reports
reuse canonical empty preparation. The synchronous `read(cards)` entry remains for
existing standalone consumers. There is no in-flight queue or strong source inventory.

`useReportPreparation` memoizes a typed cache/executor adapter for the generic
`systems/content-preparation` lease lifecycle. It returns data candidates with
waiting/preparing/ready/failed state and retries.
A same-selection replacement retains its previous complete prepared candidate until
the new one is ready. Another selection or cache scope immediately loses that value.
Selection, source, enablement, retry and cleanup invalidate the active lease; aborted
results cannot enter the cache or candidate state. Independent instances remain separate.
Cache hits expose prepared data immediately, including while execution is disabled;
consumers still apply their own content admission before mounting.

The candidate model is `candidate.preparation`, including its own `report` identity.
A candidate is data readiness only; app separately retains evidence of the model
actually rendered in an admitted complete commit. A never-rendered cached candidate
cannot be used as a committed fallback during replacement.

A native Worker returns a structured clone, so passing the original `source.report`
with that clone would violate `ReportDocument`'s identity contract. Read/restore
policy and evidence of actual content commit remain in app composition.

`preparation.worker.ts` serves the same canonical algorithm through the per-job
Worker task system. App injects native execution; unit fixtures explicitly inject
canonical inline or controlled execution. Worker construction, transport, clone and
algorithm failures stay explicit errors; there is no missing-Worker sync fallback.

Only the preparation data is reused. Live tables, task execution, images, links,
source previews, compact presentation and destinations remain in their current
owners and update from current inputs. All loaded document DOM stays present for
native Find and selection. The report resource does not own navigation admission,
read receipts or scroll restoration. Their app owner must use the displayed revision and actual complete
content commit, including for empty-success and failed/pending states.

New report kinds continue through the existing typed block renderer; they add no
cache registration or CI list. Preparation regressions are auto-discovered by the
existing unit/browser projects. Performance evidence belongs in the PR; permanent
tests pin reuse, invalidation, scope and rendering/admission invariants.
