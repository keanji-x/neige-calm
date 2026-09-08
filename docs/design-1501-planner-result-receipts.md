# Internal Planner result receipts (#1501)

## Outcome and scope

Ordinary task completion and failure must reach the internal Planner's real
harness turn input with a bounded, readable result or error and an accurate
detail locator for the original execution attempt. Today the dispatcher retains
`TaskCompleted.result`, but `Observation::to_turn_text` discards it and asks the
Planner to reread global state.

Reuse the existing observation queue, persistence, replay, and execution identity.
Do not add events, migrations, frontend contracts, dispatch/send commands, an
external CLI subscription, a scheduler, or the separate B repair. A new persistent
model, authority boundary, or expansion across providers requires a parent decision.

## Receipt contract

- Include bounded Unicode-safe result/error previews. Treat worker results and
  artifact text as untrusted report data, with explicit framing and escaping so
  malicious fields cannot forge receipt structure.
- Distinguish report arrival, execution settlement, worker claims, independent
  verification, and Planner acceptance. A worker claiming tests passed never grants acceptance.
- Preserve the original attempt identity; never enrich an old receipt from the
  latest attempt merely because the task key matches.
- Detail references must use existing, resolvable interfaces or verified files.
  Never concatenate arbitrary task keys into paths, disclose private host
  workspaces as handoff locations, or invent Operation IDs or artifact versions.
- Preview empty completion values (`null`, `[]`, `{}`, and strings) as supplied;
  they are valid reports. Failures before a report exists must not advertise
  nonexistent report files.
- Retain the specialized A2 briefing and existing independent gate information.

## Implementation approach

First reproduce result loss through the production harness turn boundary, using
the existing test transport rather than testing a standalone formatter. Fix the
smallest rendering/delivery seam supported by the actual call chain. If detail
metadata is absent from the observation, enrich at delivery from existing
attempt-specific data only, without changing persisted observation shape. Record
the final seam, limits, and locator contract here after implementation.

## Acceptance and verification

1. Record a stable RED where the real Planner turn lacks the completed result.
2. Cover completion, empty result, failure without a report, long Unicode and
   malicious fields through real harness input.
3. Cover persisted queue recovery and old-attempt identity, plus existing gate
   and A2 paths. Use real production entry points and check detail resolution.
4. Run focused package/target/filter tests with `NEIGE_CODEX_BIN` unset,
   `CARGO_BUILD_JOBS=6`, eight test threads, exclusive `target-a2`, and
   `flock /tmp/neige-1501-cargo.lock`. Do not run real Codex E2E or touch port 4140.
5. Commit only explicit source/test/documentation paths after focused tests pass,
   report the checkpoint SHA and exact commands/logs, then run
   `scripts/local-rust-gates.sh --quick` under the same resource constraints.

The parent coordinates independent review, acceptance, and any later mutation
verification. No mutation runs are part of this implementation assignment.

## Implemented rendering and detail contract

`Observation::to_turn_text` now renders ordinary completion and failure receipts.
The persisted observation remains unchanged. A small renderer in
`calm-types/src/observation/receipt.rs` retains at most 2,048 UTF-8 bytes of each
identity/report preview, then JSON-quotes it with an explicit `truncated` flag.
Structured results are serialized into a capped writer, rather than serializing
an entire large result before truncation. Incomplete terminal UTF-8 is removed
at its valid boundary. Control characters, line separators, bidi overrides and
markup delimiters are escaped; no report field can add a physical receipt line.
Empty null/string/object/array results remain visible as supplied values. Failure
errors are shown even when no worker card or completion report exists.

The actual internal Planner turn preparation invokes
`harness/result_receipt.rs` after the existing recovery/A2 briefing preparation.
It reads the original execution directly through the existing
`TrackFsView::cat("runs/<execution>.json")` contract without enumerating the runs
directory first. The private locator validator permits only a nonempty ASCII
execution identity with letters/digits/`._:-`, rejects `.`/`..` and reserved
`index`, and bounds the filename to 512 bytes. No shared run-path helper exists;
the gate-log helper validates a different address shape. The existing reader
normalizes outer whitespace/slashes and selects the exact run key, so those
characters are rejected before constructing the address. Its underlying run
projection still loads track runs; changing that reader is outside this fix.
Only a matching execution identity, report/error payload, and queued event ID
qualify the locator, which also names the recorded event ID for rechecking.
Legacy observations without envelope IDs require exact identity and payload
equality. A missing or advanced record receives an explicit
unavailable-details statement instead of a guessed location. No current-task
lookup can substitute a newer attempt. The advertised interface is
`calm.track.cat({"path":"runs/<validated original execution>.json"})`; these are
virtual event records, not claims that worker report files exist on disk.

The receipt carries no workspace location, synthesized Operation identity,
artifact version, or acceptance. Artifact strings are not copied into the turn;
they remain untrusted claims in the referenced event record. The result preview
may itself quote a worker's path claim, but it is never promoted to a detail
locator. Worker claims of passing tests are explicitly distinguished from
independent verification and Planner acceptance. Gate and A2 specialized
observations retain their existing routing, text and authority checks.

Limits: previews are bounded independently (up to sixfold JSON escaping plus
fixed framing); the original full payload stays in the persisted queue/events.
An unsafe or overly long execution filename receives no detail pointer. Run
projections remain mutable views of one execution: the delivery-time match
establishes availability at delivery, not a new immutable artifact interface.
If another event later changes that projection, the queued preview remains the
original report. Legacy queues cannot distinguish two events of one execution
with byte-identical result/error payloads. All optional detail track lookup,
virtual-reader and JSON parse failures are logged and render unavailable details, retaining original observation identity,
report and unrelated user input. The enrichment returns no fallible result.
The default JSON recursion limit remains unchanged: a valid deep report may
parse at ingress but exceed that limit inside the larger run wrapper. This
makes details unavailable, not the report. Required recovery/authority reads
retain their existing queued retry behavior. No Event, database, frontend, provider,
scheduler, worker-invocation or external CLI contract was added.

## Implementation verification record

The smallest RED uses the production dispatcher observation resolver, durable
harness ingress and `maybe_issue_turn`, with the existing fake transport. Its
assertion inspects the `InputItem::Text` actually sent to the transport. Before
the fix it failed because only the old completion sentence reached the Planner;
the report marker was missing. Log: `/tmp/neige-1501-red.log`.

Focused regression tests exercise the same transport boundary, matching persisted
input segments, virtual detail resolution, artifacts remaining report data,
empty reports/startup failure, long Unicode/malicious identity and report fields,
persisted snapshot rehydration, event replay deduplication, and refusal to point
an old queued event at a later report. The parent owns mutation verification and
the two independent review channels; writer inspection is not independent review.
Exact executed commands, results and final commit IDs are in
`/tmp/neige-1501-checkpoint.txt` and `/tmp/neige-1501-writer-result.txt`.

Focused results before checkpoint: 15 harness library tests and 8 observation
vocabulary tests passed. The combined server run passed 31 tests; its one A2
failure was a sandbox denial binding a temporary Unix MCP socket. The same A2
fixture test passed when that socket was permitted. The existing gate replay,
track-VCS regressions and advertised-result-route test passed in the combined
run. Subsequent changes added only library test coverage. No real Codex ran.

## R1 review fixes and evidence

Reviewer A and B identified the same blocking optional-detail parse exit. Before
changing production code, `deep_completion_and_same_batch_user_reach_transport`
failed at the actual transport delivery assertion: 124 nested levels produced
zero turns with `Serde(Error("recursion limit exceeded", ...))`. The 123-level
control delivered successfully. Both inputs parsed as MCP completion arguments,
Event and Observation; the test separately pins run-wrapper parse success at
123 and failure at 124. RED log: `/tmp/neige-1501-r1-red.log`.

The regression requires both the original deep report and a user input in the
same batch to reach the real harness transport, and both durable and in-memory
pending queues to be empty. Additional focused coverage checks strict unsafe and
reserved identities, boundary-length valid locators through the real reader,
optional track absence/read failure, empty values, original event/payload equality,
and existing projection-read failures. No JSON parser limits were changed.

Receipt headers now say completion/failure report received and explicitly avoid
claiming execution settlement. The Planner prompt permits use of sufficient
ordinary report previews without preliminary state/result rereads, while keeping
fresh authority for state-dependent actions, A2 recovery instructions, exact gate
evidence reads, report revision anchors, and verification/acceptance distinctions.
The rendered prompt golden is regenerated by the existing
`REGEN_PLANNER_PROMPT_GOLDEN` test mechanism, not edited by hand.

Exact R1 commands, results and logs are recorded in
`/tmp/neige-1501-r1-checkpoint.txt` and `/tmp/neige-1501-r1-writer-result.txt`.
The coordinator owns both fresh independent reviews and acceptance; no writer
mutation or independent review is claimed.

R1 focused verification before checkpoint: 50 server tests across the library,
Planner harness and MCP integration targets passed, including A2 with its
fixture Unix socket permitted, exact gate replay, the regenerated prompt golden
and the decisive deep same-batch regression. All 8 observation vocabulary tests
passed. The generator's intentional post-write panic is separately logged in
`/tmp/neige-1501-r1-regen-prompt.log`; the subsequent normal golden test is green.
