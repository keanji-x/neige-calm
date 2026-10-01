# Shared read-only tasks in the Track checkout

The execution ownership boundary is superseded by
[Execution ownership before concurrent read tasks](design-execution-manager.md).
This draft records the earlier per-entry guard approach; it is not merge approval.

This adapts the reviewed read-only guard change to #1830 S2 and #1893: workers
use the Track checkout; there is no per-Track concurrency budget and no isolated
file delivery. Released migrations remain frozen.

## Current review status

The original implementation is preserved on `codex/readonly-legacy-reviewed`.
This adaptation is incomplete and must not merge or deploy. The latest main
uses one Track checkout for native Planner and Worker execution, which invalidates
the original isolated-worktree review conclusions.

The selected design is a unified guard over Planner and Worker execution. Native
turns acquire a durable writer before provider issuance; waiting Planner inputs
stay queued. A provider completion event alone never releases a writer: Codex
also needs an empty background-terminal roster, and Claude needs a successful
marker-based process stop. Worker task delivery must select its task lease,
never its newer native execution reference.

Remaining blockers are terminal and asynchronous Forge lifetime integration,
ordinary plugin write authority, recovery of ambiguous turn issuance, upgrade
admission for pre-existing writers, fair reader/writer handoff, and aligning
pending diagnostics with access-aware claim admission. Each execution reference
must be acquired and released through one component with positive stop evidence.
Automatic approval review rejected adding terminal and Forge references before
their stop/release paths were implemented, because permanent held leases could
block subsequent access. Those rejected changes were not applied.

## Outcome and boundaries

An attached Codex task can declare `context.neige_workspace.access = read_only`.
The checkout is already the Track checkout, so no source directory or producer
lease is borrowed. Ordinary depends_on selects ordering prerequisites. Writable
execution remains the default. Claude and terminal read-only execution, and
machine gates on read-only tasks, are refused until their runner enforces them.

Move-only ReadTaskGuard / WriteTaskGuard wrap persisted lease admission. Read
leases share a canonical checkout; write leases exclude all active readers and
writers. Claim admission and pending diagnostics use the same generic resource
predicate. Unsettled Git deliveries and writable gates continue to block reads.
Queued writers must not be starved by later readers.

The read-only Codex sandbox is enforced at start and resume; readers cannot call
external plugin tools that escape the sandbox. Read workers never create Git
candidates or commit the checkout. Their report is a result, and their lease
remains until positive stopped-turn or dead-worker evidence. A read report does
not claim that the implementation is accepted. Source HEAD/branch and directory
identity are pinned at preparation and rechecked before launch/resume. Read
probes never execute repository helpers with host write permission.

Planner/assistant conversation orchestration is retained. While readers hold
leases, kernel-mediated writable tools or new writable sessions may not enter
the checkout. No retired isolated backend, task budget knob, private worker
worktree, released migration edit, or lifecycle FSM is reintroduced.

## Follow-up usage

After the kernel capability PR merges, expose workspace access as a typed task
field, make report-only acceptance explicit, unify tool guides, and present
resource wait/refusal reasons. This follows existing report task declarations
rather than adding another task API or hidden scheduling mechanism.

## Verification

Focused real admission, native start/resume, report/release and pending-reason
regressions; mutation verification of reader/write exclusion and sandbox policy;
text ratchets and quick Rust gates. Rebase reviews explicitly check abstraction
boundaries, duplicate logic and hardcoded application assumptions. Broad Rust
and real provider end-to-end checks remain CI/dedicated-host responsibilities.
