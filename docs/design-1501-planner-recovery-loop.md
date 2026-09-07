# Planner recovery after an isolated execution stops (#1501)

Status: reproduced on `fb318f57`; implementation in progress. This slice follows the
[task continuity principles](architecture/1501-task-continuity.md). It does not
claim automatic artifact delivery or completion of #1501.

## Outcome

An auto-declare Planner task fails after doing useful work. Planner can read the
failure immediately. If recovery is initially unavailable because the old isolated
execution is still stopping, it can end its turn and learn when that prerequisite
has been satisfied. It then chooses one authorized same-contract recovery, reads
the successor's actual result, and completes the work without a user wake-up,
replacement declaration or manual lifecycle repair.

The failure and its history remain visible. A notification is an invitation to
read current capability, not authorization to retry or proof that a successor has
started. The existing recovery service rechecks the exact attempt, unchanged
contract, declaration/release, Track lifecycle, caller authority, recovery bound
and original namespace stop evidence.

## Scope decision

The current-main audit found that the candidate A-to-verified-Git-to-B scene needs
several protocol boundaries still absent from production: writer quiescence,
sealed verification input, claim-time content binding and downstream preparation.
The artifact library itself has shipped, but has no production consumer. A cwd
or path-copy shortcut would not satisfy the design. That remains a follow-up.

This slice instead connects existing failure reporting, isolated stop evidence,
Planner observation and recovery. It adds no automatic business retry policy,
new task-state writer, file handoff, configuration page or general supervisor.
Already-executed legacy workers without a supported stop fence remain ineligible.

## Checks before implementation

- Reproduce the failure/stop timing through the existing isolated provider fixture
  and production reporting/observer/Planner paths. A native failure may be visible
  while the Operation is still parked and recovery correctly refuses it.
- Determine whether a later production notification actually reaches Planner when
  the failed Operation has committed its stop evidence. Do not infer this solely
  from the absence of one event mapping or from model behavior in a fast run.
- Update the stale `calm.plan.recover` description: current admission supports
  stopped isolated failures as well as preparation failures. Describe the same
  supported scope as the capability read and retain the one-recovery authority.

## Required properties

- Failure stays immediately observable; an unproven stop cannot advertise safety.
- Live completion and restart/reconciliation use the same persisted facts and
  deduplication. An in-memory-only callback must not be the sole wake-up path.
- Repeated cleanup or observation replay cannot create unbounded Planner turns.
- A superseded attempt cannot advertise itself as the current recoverable work.
  The actual command always rechecks current authority, even after a valid hint.
- Existing User-only recovery restrictions and Planner generation limits remain.
- Old report, failed attempt and files remain evidence; a recovery starts in a new
  empty workspace and does not claim to repair or inherit failed candidate files.

## Acceptance

A focused production regression first demonstrates the missing transition. After
fixing it, the exact failed attempt produces a bounded actionable wake-up after
stop, `calm.plan.list` supplies its capability, and a Planner-role
`calm.plan.recover` request creates one successor under the same logical key.
The successor reports success through the actual Worker report path. Exercise
replay/restart, stale attempt and unproven stop as well as the successful path.
Mutation verification must pin the wake-up invariant without mutating fixtures.

A separate local instance runs a real Planner and isolated Codex Worker. Use a
small explicit failure injection after writing useful output, then the same
unchanged goal on its successor. Record task and Operation identities, failure,
stop and observation ordering, Planner reads/writes, any external guidance and
the actual successor result. A fast baseline run that happens to recover is not
proof the timing defect is absent. Local helper/setup faults remain separate
from product recovery findings. Stop only this experiment's owned processes.

Implementation details and actual evidence are recorded below after reproduction;
focused tests, two independent full reviews and required CI precede squash merge.

## Selected integration

The red regression holds the actual parked Operation lease while the native
Worker failure reaches Dispatcher and PlannerHarness. Planner's capability read
is initially false. After lease release, the owned observer proves namespace stop
and commits the failed Operation; capability becomes true, but no second Planner
observation arrives. This pins the missing transition without a second recorder
or an assumption about model latency.

Add a kernel-only `task.execution_settled` event identifying the exact task
attempt and Operation. An adapter completion hook appends it in the same
transaction as successful parked completion, after validating the original typed
stop evidence. Publish after commit. Live and restart reconciliation share the
existing completion path; a repeated completion cannot append another event.
The event describes failed isolated execution settlement, not recovery permission.

Dispatcher and harness boot replay resolve that recorded execution and use the
existing SystemContext observation, durable queue and watermark. Suppress obsolete
or withdrawn work. Ask Planner to read current `calm.plan.list` and choose the next
permitted action, including explaining when User action is required. Retain the
actual recovery command's checks. Session exit alone is insufficient: it precedes
Operation settlement, and session/card deletion must not erase retained evidence.

The new event follows the repository's event-version compatibility process. No
new task-status writer, recovery policy, notification table or polling service is
introduced. The exact files, tests, mutation results and product experiment are
reported with the implementation PR.

## Generated contract ownership (CR-1501-SETTLED)

The orchestrator approves the additive settlement event and its generated wire
bindings for this scene. Run the real frontend generators and include all affected
artifacts. Generic frontend transport, styles and unrelated interfaces retain their
current contracts. Any modified frozen generated file receives its exact
`OWNERSHIP-CHANGE` trailer in the commit, PR body and squash body.

The schema parity build caught the required runtime-decoder update after actual
wire generation. Both existing FE and legacy web discriminated unions accept the
new event with required task and Operation identities; focused decode regressions
reject missing identities. This extends their existing mirrored contract only.
The orchestrator approves these exact frozen core changes:

OWNERSHIP-CHANGE: fe/core/api/schemas.ts — decode approved isolated execution settlement event (#1501)
OWNERSHIP-CHANGE: fe/core/api/schemas.contract.test.ts — verify approved settlement event contract (#1501)
OWNERSHIP-CHANGE: fe/core/api/generated/wire.ts — add approved isolated execution settlement event (#1501)

Settlement also changes the derived recovery capability. The existing event
invalidation policies therefore refresh task evidence with the same scope rules
as failure, without parsing opaque attempt IDs or adding a new UI flow.

OWNERSHIP-CHANGE: fe/core/events/invalidation-plan.ts — refresh evidence for approved settlement event (#1501)
OWNERSHIP-CHANGE: fe/core/events/invalidation-plan.test.ts — verify approved settlement invalidation (#1501)
