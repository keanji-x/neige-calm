# Worker outcome reports and completion (#2053)

## Decision

A Worker reports the outcome of one execution attempt. It does not accept its
own work, close the Track, or wait for the delivery or gate. The public tools are
`neige.task.report_success` and `neige.task.report_failure`; the CLI commands are
`neige task-report-success` and `neige task-report-failure`. Both entry points use
the same kernel handlers and return `{"status":"report_received"}` after the
report transaction. This response includes a same-outcome idempotent retry; it
is not a statement that a new event was emitted or that downstream work finished.

The old MCP and CLI names are removed, without hidden aliases. Stale MCP calls
receive the existing unknown-tool error with the calling role's current tool
list. A service upgrade must finish or stop existing Worker turns before their
old instructions become unusable. Newly rendered Worker instructions use the
new names. A new migration updates saved Track recipes and their revision
anchors. Historical transcript calls and released migrations retain their
original names; those records describe calls made against an older interface,
not executable instructions. Existing history rendering treats tool names as
recorded data and does not require these two tools to remain registered.

The three-segment MCP grammar allows compound actions separated by underscores;
the object remains a single lowercase word. Sanitized kernel callable names
remain unique because their object/action boundary is fixed. The actual registry
and client adapter tests pin that property. This updates the action constraint
in #2003 without introducing a new object or another namespace level.

## Facts and owners

| Fact | Owner and evidence | Meaning |
| --- | --- | --- |
| Worker success/failure report | Worker; card-scoped `task.completed` / `task.failed` | A claim about this `attempt_id`, not independent verification |
| Execution status | Kernel `tasks` row | Ungated success becomes `done`; gated success becomes `verifying` until the gate finishes |
| Delivery | Kernel delivery row and candidate | `done` can coexist with delivery `pending` or `failed`; inspect `candidate.delivery` separately |
| Verification | Kernel gate and its exact target/log | `passed` verifies the target under the declared checks; `ungated` supplies no gate evidence |
| Semantic acceptance | Planner verdict on the producer attempt | Judgment recorded separately; rejecting it does not rewrite the execution terminal state |
| Track end | Planner `neige.track.close` | Goal met or cannot be met; not implied by any Worker report |

The persisted event names stay unchanged. Existing projections distinguish the
Worker card-scoped report from the Planner track-scoped verdict using the
recorded authority and scope. A Worker-supplied `result.status = "accepted"`
does not turn its report into a Planner verdict. Splitting event types would
require a separate persistence/replay contract, rather than an incidental rename.

`neige.plan.list` exposes execution status, candidate delivery, and verification.
For ungated reports, a successful report transaction sets `finished_at_ms` before
Git delivery settles. Report receipts and plan descriptions must not call this
end-to-end settlement. Report arrival also does not prove that the Worker turn,
provider session, or process ended. Workers must stop changing their workspace
and end their turn after reporting; the kernel owns subsequent Git delivery.

## Dependency and publication audit

The generic dependency contract is execution ordering: `checkout_admission`
requires every dependency to be `done`, then separately applies checkout
occupancy. This is not an artifact acceptance rule. Reviews and repairs may need
an unaccepted producer's output; requiring a Planner verdict for every dependency
would change that contract and can obstruct those tasks. No new acceptance fence
is introduced by this rename. Workflows requiring delivered or accepted inputs
must establish that evidence before releasing their consumers; a purpose-bound
input/acceptance protocol remains part of #1501.

`neige.track.publish` checks both a stored candidate at the actual branch tip and
that candidate's producer being `done`; it does not infer a candidate from the
Worker report. Thus `done + pending/failed delivery` alone cannot satisfy the
candidate-at-tip check. Publication eligibility is not semantic acceptance:
Planner must judge the implementation when its workflow requires it. A review
report does not accept the producer it reviewed.

`neige.task.verdict` currently checks the Planner role and attempt's Track
ownership and records the judgment. It does not universally fence delivery or
gate completion, or alter `done`. Guidance must describe that limitation rather
than claim an enforcement mechanism that does not exist. Adding a fence requires
a separate design for all attempt kinds and recovery paths.

These source-level checks do not establish a production race or incorrect
acceptance. A change to dependency readiness, verdict admission, or task terminal
states needs a real-entry-point reproduction and its own reviewed change. The
rename preserves report admission, atomic lease release, idempotency, gate and
delivery recovery, and deferred Planner notifications.

## Verification and review

This change is L2 because saved recipe migration crosses a persistence boundary.
Use two independent complete-diff reviews. Exercise the real MCP and CLI report
entry points, role-filtered discovery, retired-name rejection, and recipe reads.
Check historical names remain recorded and unchanged recipes retain their
revision/time. Run focused report/ownership/idempotency and delivery/gate tests,
the real prompt/registry golden generators, text ratchets, and Rust preflight.
Do not run real Codex E2E on the shared production host.
