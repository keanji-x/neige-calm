# Task stall follow-up

Issue: https://github.com/keanji-x/neige-calm/issues/3466

## Outcome

A task waiting for admission or settlement must not prevent other Tracks or the
Planner's exception path from making progress. The kernel reports observed facts;
the Planner decides whether to wait, use an independent route, or stop work with
an operation the current lifecycle actually permits.

## Evidence and ownership

The two reported incidents and the investigation Track reproduce three gaps:

- Eight terminal attempts remain dispatched while their operations are pending.
  `Scheduler::dispatch_task` and `resume_dispatched` retain the global dispatch
  permit across the operation wait. The same semaphore gates Planner event
  handling. An unrelated ready task therefore gets neither a worker nor a wake.
- The boot sweep avoids that wait, but the steady-state sweep still awaits
  resumed dispatches and pending scheduling passes. An exhausted pool can stop
  subsequent reconciliation, including exception handling.
- A gate can record `Stopping { Timeout }` and remain parked until its execution
  settles. This is necessary resource protection, but suppressing the worker's
  completion until the gate verdict leaves the Planner without an exception.

Terminal completion already checks the recorded exit code. A queued task has no
exit code to check, and an unconfirmed stop is not a successful cleanup. The fix
belongs in task scheduling and follow-up, using the existing kernel wake channel,
task cancellation authority, operation ownership, and execution settlement facts.
The quiet-worker watcher only handles a readable worker screen; it cannot cover
tasks with no worker, a running terminal, or a parked gate.

Further read-only incident inspection identified the upstream causes:

- One workspace-reclaim execution remained `Unknown` after executor restart.
  The reclaim owner's global terminal reservation correctly stayed held. Every
  terminal operation repeatedly met `WorkspaceBusy` and remained Pending. The
  scheduler's shared permit lifetime then spread that reservation to unrelated
  worker admission and Planner notifications. There is no evidence of a
  permanently locked operation drive mutex in this incident.
- The document gate failed before its payload ran. On the deployed Linux 5.4
  host, `close_range(..., CLOSE_RANGE_CLOEXEC)` returned ENOSYS in the helper's
  `pre_exec`. `command.spawn()` reported the failure, but the executor mapped all
  spawn/setup errors to `Unknown { receipt: None }`, which receipt-based recovery
  cannot settle. The gate log remained empty. A typed, proven pre-exec NoChild
  outcome must be distinguished from errors after a child may have started.

Do not clear old Unknown journals based on their error strings or elapsed time.
Do not remove the descriptor allowlist or global reclaim reservation to make a
test pass. Add a real executor-entry regression for failed helper exec and keep
post-spawn uncertainty conservative. The reclaim journal's original failure
before restart is no longer known.

## Constraints

- Keep the existing execution owner as the only authority for process outcome,
  replay, and cleanup. An elapsed threshold must not release a lease, fabricate a
  gate verdict, or relaunch an operation with a new identity.
- Keep reconciliation and Planner wake delivery responsive when startup capacity
  is exhausted. Preserve per-attempt single flight and the claim transaction's
  current-state, Track-open, and checkout checks.
- Report the exact attempt, phase, elapsed time, and available evidence. Treat
  elapsed time as a reason to inspect, not proof of failure.
- Follow-up must distinguish a bounded wait from a bypass and a supported
  cancellation. Record the decision and reason; a wait must schedule a later
  check. Reject stale attempts and cross-Track actions.
- A bypass preserves the original task and its unresolved execution protection.
  The Planner must use an independent, authorized route. A cancellation is only
  available when the existing owner can perform it safely.
- Failure and stall observations require a concrete follow-up decision in the
  Planner's instructions, rather than a status summary followed by silence.

## Chosen contract

The shared dispatch permit covers admission only. Each claimed attempt retains
its single-flight guard while its operation runs outside the Track scheduling
lock; callers may still join those drives after releasing the lock. Steady-state
reconciliation pokes admissions and resumes dispatches without joining them.

Task follow-up observes pending, dispatched and verifying tasks, plus running tasks
without the existing worker-liveness owner (terminals, compactions and child Tracks).
It never converts elapsed time into exit evidence. Ordinary worker liveness retains
its current owner. Dispatched child startup retains the existing child-bootstrap
progress-obligation owner and its durable parent-notice retries, avoiding a second
notice for the same handoff. Child tasks still receive general stall follow-up
before admission and after they reach running.

Task follow-up is a scheduler-owned record keyed by the current attempt, phase,
phase timestamp and gate number. After a bounded period without a phase change,
the kernel atomically stores a fresh notice and emits `track.wake_requested`
with source `task-stall`, observed facts, and the allowed actions. Repeated scans
and restarts reuse the durable next-review time. A notice is an inspection
request, never a process failure or a release authority.

The Planner answers the current notice through `neige_followup_settle`: `wait`
requires a bounded review interval, `bypass` records an independent route while
keeping periodic follow-up, and `terminate` uses the existing cancellation
implementation. Each choice requires a reason. The write checks current attempt,
Track, phase and notice; it cannot apply a stale observation to a successor.
Existing cancellation restrictions remain authoritative. Pending observations
distinguish dependencies and checkout occupancy from available admission.

## Acceptance and review

L2: scheduling, persistent follow-up, and cancellation cross lifecycle and
authority boundaries. Use two independent read-only reviews after the final
implementation, with fresh reviews after behavioral fixes.

Focused regressions exercise production scheduler, wake, and tool entry points:
capacity exhaustion; deferred admission; a gate that has timed out but not
settled; notice deduplication/restart; bounded waiting; bypass; permitted and
refused cancellation; stale attempts; and unchanged resource protection. Keep
tests registered through the existing suites, without adding a parallel roster.
Mutation verification covers only the assertions that uniquely protect these
boundaries. Run the repository's text, quick Rust, full contract, and Tier 1
integration gates. CI owns broad suites; real Codex E2E stays off the shared host.

The investigation uses an independent local clone because the deployed task
queue is itself affected and the Track checkout's Git metadata is read-only.
Local checks and commits must be reported as such, never as a passed kernel gate
or a successful result of the canceled queued review.
