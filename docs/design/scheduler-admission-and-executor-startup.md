# Scheduler admission and executor startup responsiveness

Related: #3466, #3494, #3495. Deferred Planner follow-up: #3497.

## Outcome and scope

A task awaiting operation admission or settlement must not consume the shared
scheduler/Planner admission permit or retain the Track scheduling lock. Periodic
reconciliation schedules recovery without joining unbounded operation waits.
Background triggers acquire the existing Track lock before spawning and coalesce
through its dirty flag, including the final unlock handoff. Repeated scans cannot
accumulate waiters behind exhausted admission. Per-attempt single flight and
transactional claim fences remain authoritative.
Actual operation side effects retain the existing shared OperationRuntime drive
lock; releasing scheduler waits does not add parallel startup authority.

#3495 prevents new unconfined workspace cleanup from creating this incident's
original blocking dependency. Already admitted unresolved cleanup keeps its
execution identity and protection. Other operations can also wait indefinitely;
this change prevents those waits from spreading to unrelated admission.

The executor distinguishes proven preparation/exec failure (`NoChild`) from an
error after successful helper spawn (`Unknown`). Linux kernels that reject
`close_range(CLOSE_RANGE_CLOEXEC)` with ENOSYS/EINVAL use procfs descriptor
enumeration to mark all non-stdio descriptors CLOEXEC before reopening explicit
channels. Permission failures still reject startup. No old Unknown journal is
rewritten, and no timeout grants cleanup or cancellation authority.

Planner policy asks for evidence and a concrete next step after task failure.
Durable stall reminders, periodic decisions, `neige_followup_settle`, and their
migration/registry changes are excluded. #3497 must consider the existing
progress-obligation primitive and independent watchdog, normal waits, duplicate
child supervision, and current execution/cancellation authority.

## Ownership and acceptance

L2: scheduler lifecycle and executor result/isolation boundaries.

- Shared admission and the Track lock remain available while the real operation
  drive is blocked. A second declaration can be claimed without duplicating an
  attempt; actual launch ownership stays with the operation/executor layer.
- Steady reconciliation returns while startup capacity is exhausted; the same
  pending attempt starts when capacity returns.
- Recovery keeps per-attempt single flight, frozen context, child bootstrap
  ordering, recorded-exit completion, and retry identity.
- A failed helper exec has a durable NoChild outcome through retry and restart;
  errors after successful spawn remain conservative Unknown outcomes.
- Descriptor fallback covers inherited descriptors above the soft descriptor
  limit and preserves only the explicit allowlist; EPERM never weakens isolation.

Use production-entry regressions, red checks and focused mutation verification
for descriptor isolation, two independent L2 reviews, text/quick Rust/full
contract gates, and Tier 1 stack E2E. CI owns broad Rust suites; never run real
Codex E2E on the shared production host.
