# A2 candidate review qualification

Status: implementation in progress; baseline 37d890e43, 2026-09-08.
Parent approved this bounded contract and persistence design on 2026-09-08.

## Contract

One candidate producer, one explicitly selected candidate_reviewer, one ordinary
candidate consumer. The scope-tagged policy preserves A1 declared-checks-only JSON;
review-required additionally requires the Reviewer task key. Reviewer starts only
after successful declared machine checks, takes the candidate from the verification
Operation's durable frozen input, and receives those exact bytes and machine evidence.
Review of machine-failed candidates is a follow-up, not supported by A2.

Reviewer completion result requires passed and blocking_findings. A pass requires
no blockers; a failure requires at least one specific nonempty blocking reason.
Subject, attempt, Operation and session are derived from kernel input/report authority,
never supplied by the model. Ordinary consumer purpose cannot grant review access.

## Persistence and authority

Reuse immutable candidate input bindings with a distinct review purpose. New migration
0104 adds immutable event-linked acceptance receipts and consumer decision bindings;
0103 and earlier remain byte-frozen. Receipt and actual Planner decision event ID are
written in the same existing verdict transaction. Event result carries exact evidence;
the receipt proves it passed that transaction. Event JSON or broad historical
is_planner_verdict_event classification alone is never acceptance authority.

No independent accepted bit, new Event, TaskKind, Provider or frontend surface.
Accepting the producer requires matching current candidate, frozen policy, machine
verification and the designated Reviewer's authenticated pass. An early acceptance
is refused and cannot become effective when evidence later arrives. Accepting the
Reviewer report is not acceptance of the producer. Rejections retain candidate and
findings and invalidate current delivery eligibility without rewriting Task terminal
state or implementing repair.

Consumer claim atomically binds exact candidate, machine/review evidence and decision
ID. Preparation and preturn revalidate those bindings, current authority and actual
bytes. Technical recovery inherits the original evidence, never latest. Existing
plan/result reads distinguish historical machine/review/decision facts from current
qualification and preserve failed evidence.

## Acceptance

Production authoring, authenticated report, verdict, scheduler and Operations; only
the external Codex provider is faked, machine checks use real harmless commands.
Pin no-review/blocking/malformed/spoofed/stale reports, early or copied verdicts,
report-only acceptance, exact version/policy, withdrawal and preturn integrity,
idempotent replay and recovery. Rejecting or accepting leaves Task terminal state
unchanged. Mutation-check exact-subject and launch fences with predicted complete
red sets. Run focused tests and quick gates using this worktree's exclusive target,
flock /tmp/neige-1501-cargo.lock, jobs 6/tests 8. Two independent complete reviews
remain required. No shared 4140 changes or real Codex E2E; no .codex-local/build output
in the deliverable.

## Authoring shape

Use the existing `neige_execution` selection. Producer policy example:

```json
{"scope":"review-required","reviewer":"review","timeout_secs":60,"steps":[{"name":"tests","cmd":"python3 -m unittest"}]}
```

Reviewer uses `workspace: "file-input"` and this `file_delivery` value:

```json
{"role":"candidate_reviewer","producer":"produce","slot":"project","purpose":"candidate-review-input"}
```

The producer must already be declared when declaring the Reviewer. Set explicit
semantic requirements in the review task goal/acceptance. Ordinary consumer stays
`candidate_consumer` with `verified-candidate-input`. The Planner accepts the
producer's exact attempt via `calm.task.verdict` after inspecting verification and
review facts. Repeating acceptance of the same evidence preserves the original
decision ID (and its original wording); an intervening rejection requires a new
acceptance and never rewrites an existing consumer binding.

## R1: report and Operation settlement are distinct notifications

TaskCompleted can reach the Planner while the Reviewer's parked lease still holds
cleanup. It remains insufficient for acceptance. TaskExecutionSettled now also
records a Done CandidateReviewer's exact succeeded/failed Operation after confirmed
namespace stop. Successful owned-parked completion records this in its terminal
transaction; the existing boot/periodic scheduler repairs the compensation-to-notice
window. Both paths share the event-ID dedupe and retained stop check. Ordinary Done
workers remain quiet; obsolete or withdrawn Reviewers do not imply current readiness.

Review settlement has its own delivery-time briefing, recomputing current authority
and showing report outcome separately from Operation outcome. It binds no Recover
operation, including when the report is Done but the Operation failed. Failed-only
require_stopped_tx and recovery admission remain unchanged. The existing Event shape
is unchanged; this is not a machine-verification event or new acceptance authority.

Fixture reset deletes candidate decision bindings and receipts before their events.
Normal Track teardown and the production verification/stop guards are unchanged.
