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

Only the preparation data is reused. Live tables, task execution, images, links,
source previews, compact presentation and destinations remain in their current
owners and update from current inputs. All loaded document DOM stays present for
native Find and selection. Navigation admission, read receipts and scroll restore
keep their existing lifecycle. No Worker or background-thread claim is made.

New report kinds continue through the existing typed block renderer; they add no
cache registration or CI list. Preparation regressions are auto-discovered by the
existing unit/browser projects. Performance evidence belongs in the PR; permanent
tests pin reuse, invalidation, scope and rendering/admission invariants.
