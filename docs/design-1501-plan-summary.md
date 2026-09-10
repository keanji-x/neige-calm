# Current plan summaries

Extend Planner-only `calm.plan.list` with optional `detail` (`summary`/`full`,
full by default) and one exact Track-local `key`. Reject supplied null, unknown
properties, wrong types and blank keys. Use the current-allocation reader directly
for a key; an absent allocation is unavailable, never successful completion.
Preserve the legacy full response and current role/Track boundaries.

Summary is an allowlisted projection of existing recovery, activity and delivery
readers, not another qualification algorithm. Retain current attempt, state,
blockers, publication/verification/review failures and unknown facts. Candidate C2
is distinct from repair input C1 and original repair metadata. Omit goals,
commands, history and findings; limit diagnostic text to 256 Unicode characters,
list truncation/omission paths and provide an exact full-evidence request. Full
reads are fresh current reads, so compare attempt identity before deciding.

Acceptance uses registry/parser/exact-key/large-payload tests, a few preservation
checks reusing existing candidate fixtures, and one production filter mutation.
The 12 KiB ceiling applies to a controlled single-task fixture only. Existing
core tests own eligibility. No storage, lifecycle, provider or UI changes.
Parent owns independent reviews, CI and the combined five-node native trial:
an ordinary status question must elicit compact calls through normal guidance,
followed by a neutral direct interview. Tool flags alone are not acceptance.
