# Named verified-candidate consumer dispatch

Extend `calm.task.dispatch` with one bounded workspace variant:

```json
{"name":"Exercise accepted release","goal":"Exercise the accepted files","acceptance":"Report exercised behavior and findings","executor":"codex","workspace":"verified-candidate","input":{"producer":"release-2","slot":"release_bundle"}}
```

An internally tagged, strict argument enum preserves the existing empty wire
object and requires typed producer/slot input only for verified-candidate.
Map those exact values to existing `isolated-codex-v1`, `file-input`,
`candidate_consumer`, `verified-candidate-input`. Validate with existing execution
types. No arbitrary paths, cross-Track input, gates, new provider or protocol.

The existing Planner DecisionSink/report writer owns declaration and immutable
receipt creation atomically. Required authority, User release, budget and
lifecycle checks remain intact. Same name and typed contract replay the original
identity without writes; changed input conflicts. Old saved empty contract JSON
deserializes unchanged, and its declaration payload (including no_gate_reason)
stays identical for current-contract comparison. No migration is needed.

Existing candidate selection, qualification, claim, preparation and preturn checks
own immutable input. Review-required sources need passing machine/reviewer
evidence and explicit producer verdict; declared-checks-only sources retain their
existing policy. Verdict already supports atomic optional lifecycle continuation,
and Reviewing already schedules. No report edit is needed solely to continue or
declare. Recovery retains the original binding; new work uses a new name, and a
repaired candidate is selected explicitly by its returned repair key.

Receipt creation is not worker startup. Only candidate dispatch adds a concise
input/admission diagnostic projected from the existing file-delivery view:
actual contract/source, candidate identity, preparation, qualification/reason and
minimal decision identity. Exclude review findings/history, policy commands and
duplicated goal. Empty replies stay unchanged. Full evidence remains available
through `calm.plan.list`; the compact projection is not a worker result or a new
qualification algorithm.

Verification uses public dispatch tests for exact nondefault mapping, same/changed
input replay, strict schema/parser behavior and historical empty JSON/payload.
One existing real candidate fixture proves waiting before acceptance and exact
prepared files/check/decision binding after verdict, without a second consumer.
Reuse core qualification/recovery negative coverage; add only needed admission
smoke. Mutation-verify one critical mapping invariant, run focused tests and quick
Rust gates, and review the frozen diff independently. Parent owns CI, native
repair trial, PR and merge. No summary-query expansion belongs to this slice.
