# Workspace identity and launch protocol (S2a implementation plan)

Status: draft implementation decisions for S2a of #2666. The parent contract is
[track-workspace.md](track-workspace.md). This document does not claim an
implemented capability. S1 remains a separate behavior-preserving delivery.

## Outcome

Worktree execution and file access use a persistent `WorkspaceRef` and kernel
authorization. An absolute display path cannot authorize a launch. Existing
worktree behavior, wire events, operation payload hashes, upstream selection
timing and cleanup policy remain unchanged. No view backend is selectable.

## Ownership and types

`workspace` owns validated `WorkspaceId`, `WorkspaceCwd`, `WorkspaceRef`, opaque
runtime handles, directory identity and filesystem/Git mechanisms. Cwd is a
UTF-8 relative path; root is `.`; absolute paths, parent traversal and NUL are
rejected on construction and deserialization. No runtime generation is part of
`WorkspaceRef`. `DisplayPath` has no path conversion or `AsRef<Path>`.

Truth owns persistence, binding/freeze transactions and compare-and-swap.
The kernel owns principal/purpose authorization, selection, materialization,
admission and lifecycle fences. Providers only encode an authorized execution
target into their external protocol. No adapter selects a backend by identity.

`resolve(principal, purpose, reference)` is the kernel entry point. It validates
the track/resource association and the requested operation's existing authority,
then checks the registered directory identity, readiness and recovery state.
It returns an opaque handle for the current generation and installation epoch.
Resolving an identity never substitutes for checking a persisted source proof.

Three capabilities remain distinct: workspace content, kernel artifacts, and
fixed source-Git administration. Artifact paths cannot be supplied as workspaces;
Git administration does not accept arbitrary argv or execute repository code.

## Persistence and legacy decoding

A new migration adds independent resource rows and track bindings; resource rows
do not cascade with track or area deletion. A resource has a versioned descriptor,
selection version, generation, installation epoch and explicit materialization
state. `Pending`, `Ready(receipt)` and `LegacyWorktree` are distinct shapes;
missing baseline evidence is never replaced by HEAD or a default value.

The existing single workspace writer updates its binding in the same transaction
as the four track columns and shares their freeze latch. The launchpad exception
stays in kernel policy. Same physical legacy checkout roots share a resource/lock
domain; distinct owned track worktrees remain distinct resources. Resolve/open
validate directory identity using controlled descriptors and Git-reported roots,
not lexical path prefixes alone. A replaced or missing root requires recovery.

Schema migration preserves legacy source facts; filesystem verification and
adoption occur in an explicit recoverable kernel operation outside a DB writer
transaction. Until adoption succeeds, the resource cannot produce a new handle.
New operations persist required typed targets. Historical operation payloads,
outputs, events and their hashes are never rewritten. A legacy decoder validates
the original cwd against the frozen workspace identity and stores a CAS sidecar
binding keyed by operation ID plus the original payload hash. Replay consumes
that binding; it cannot fall back to a track's current cwd. Unverifiable history
is refused with recovery information.

Legacy task/gate/terminal/session/child/forge records are decoded at their owning
boundary. Explicit user cwd is converted to a relative cwd after root containment
and symlink checks. Existing worktree cwd outside the root uses an explicit
`ExternalDir` variant with its own authorization. It carries no resource identity
or source evidence; an external gate remains unbound. Later view support must
reject this variant rather than use it to reach the source checkout.

Writer lease uniqueness and supersede use workspace ID. Read/write occupancy
still comes from the existing checkout admission logic; this adds no competing
scheduler or reader policy.

Identity, binding authorization, task admission and destructive quiescence are
separate typed conclusions. Sharing their resource facts does not merge their
policies into one Busy boolean. A live ordinary terminal or idle Planner is not
a new worker-admission blocker. It does block destructive quiescence while it
can access the resource. Reference accounting likewise separates permanent
identity/audit history from active execution/storage and protected Git content.

## Materialization receipt and baseline pin

Ensure still refreshes upstream first, takes the common metadata lock, probes
registration and branch, and selects base/upstream only for a new branch. No
selection moves into track creation. Kernel persists a materialization intent
with the chosen commit before creating its protective ref; the protected object
must exist before the receipt becomes Ready. Retries inspect the actual branch
and original intent, including a branch created by a failed worktree add.
Existing legacy materializations retain an explicit unknown creation baseline
until their conversion contract supplies evidence; they never adopt the current
source checkout tip as an invented creation baseline.

The owned pin is an explicit new S2a Git effect. Upstream remains branch-config
authority until S3; this slice does not detach HEAD or introduce delivery policy.

## Launch fence, ticket and supervisor

Each invocation of a runtime handle checks generation, installation epoch,
admission and purpose again under the lifecycle fence. A durable launch intent
is recorded before a ticket is issued. A ticket is unpredictable, single-use
and bound to resource/generation/epoch, executor and process/operation identity.
An unresolved ticket counts as occupied; cancellation of an HTTP request or a
readiness wait cannot release that occupancy.

The supervisor control handshake advertises its execution-envelope version.
The worktree envelope names a registered resource and relative cwd, never an
arbitrary namespace PID/path. The supervisor opens and verifies the registered
root, consumes the ticket under its resource fence immediately before fork, and
reports `Forked`, definite `NoChildCreated`, or `Unknown`. Kernel settlement
retains unknown occupancy until supervisor/process evidence resolves it. Kernel
restart reconciles every outstanding intent before admitting destructive work.

Close/delete first stop ticket issuance, then revoke unconsumed tickets and wait
for acknowledgement or positively identified launched processes to drain. An
executor that cannot account for a ticket keeps the resource Busy. Reopen or
recovery increments generation after reconciliation; old handles and tickets
remain invalid. A late fork cannot cross a completed quiescence transition.

Old supervisor compatibility is an explicit negotiated legacy-worktree mode;
it cannot accept a view or claim the stronger envelope contract. Upgrades never
kill unknown existing PTYs to obtain a handshake. Quiescence evidence explicitly
states whether it covers kernel-owned direct launches, supervisor descendants,
and provider turns; idle task/session rows alone are never process evidence.

S2a adds no namespace fd fields. The S2b envelope extension must preserve the
same ticket/fence linearization and validate controlled namespace capabilities.

## File and caller consolidation

Reuse track read/raw routes and add track list/status/diff. Relative inputs,
authorization, descriptor-relative symlink protection, MIME behavior and size
limits remain at their existing owning layers. Attachments retain managed-only
admission and use the same content capability for their actual IO. Frontend
preview/report/image/HTML callers use track ports rather than an absolute cwd.

Consolidate all launch families from the parent design's facts: Planner and
conversation providers, worker/restart/rewind/readiness, terminals and terminal
tasks, supervisor pipe/PTY, gates/checkpoints/sampling, forge probes/actions,
today/child/repoint and recovery. Remove `agent_cwd()` after its consumers move.
Host artifact and source-Git accesses receive their distinct capabilities.

Clippy disallowed-method checks cover raw launch/path IO with narrow, reasoned
exceptions at the owning mechanisms and artifact boundaries. Do not add a file
allowlist or an inventory of current callers/tests as a permanent gate. Runtime
guards reject raw host operations on registered view sources when S2b exists;
compiler/source checks alone do not prove isolation.

## Acceptance and review

### Cross-version submit comparison

Add required operation input-codec metadata in a new migration. Existing rows
are marked `v1`, an opaque version interpreted by their registered adapter.
Only adapters whose request representation changes register `workspace-ref-v2`
and its strict v1 conversion. Unaffected adapters keep v1 and the original hash
contract, including workspace-free card/recipe operations and `commit_keyed`.
The driver has no workspace-kind list and never requires their bindings.
This metadata does not rewrite payload, tx_output, event bytes or payload_hash.
The driver looks up the stable operation key and its codec before comparing a
new encoding's hash. Same-codec requests keep the existing hash comparison.
For a legacy row, the registered operation adapter compares the incoming typed
request with that row's original request decoded through its verified sidecar.
The generic driver does not know cwd field names or domain equivalence rules.

Only a proven representation change may return `SameRequest(existing_op_id)`;
every other request field and authorization-relevant value must match. A changed
target or argument remains Conflict. A missing/unverifiable legacy binding is a
recoverable refusal, not permission to ignore the hash. Sidecar adoption occurs
outside the driver writer transaction, then comparison repeats under that
transaction against the same operation ID, codec and original hash. Replay uses
the original stored payload and adapter decoder, never the new payload. The
sidecar cannot turn a new request into an old operation by key alone.

Sweep producers as well as replay readers, including scheduler worker payloads,
conversation/today/child starts, terminal and worker submissions, forge and
verification. Test current production `submit` on pre-upgrade dispatched/running
rows: original requests reuse the original operation; changed cwd/arguments
conflict; old hash and payload bytes remain identical. This explicitly covers
the early comparisons in `operation/repo_sqlite.rs` and `operation/driver.rs`.

### Shared resources and binding-scoped lifecycle

Change the existing occupancy/admission entry point's scope to workspace ID and
aggregate its task, reader/writer lease and unsettled-delivery facts through all
bindings. Its callers (claim, ready projection, supersede and cleanup) consume
that same task-admission result. Pending launches and positively identified live
processes join the resource facts for destructive quiescence, not as additional
worker-admission exclusions. Only a pending launch that belongs to an existing
task claim participates in that task's existing exclusion. An ordinary terminal
or idle Planner may coexist with a worker and still prevents reclaim. Do not
create a second scheduler or infer task policy from process presence.
Writer/reader conflicts span bindings, readers may share, and distinct worktree
roots remain distinct even when they share a Git common directory.

Bindings have a monotonically increasing admission version. A runtime handle
and ticket carry its authorized binding/version in addition to resource
generation/epoch; this does not add fields to persistent WorkspaceRef. Closing,
deleting or repointing one binding seals/revokes only that binding and waits for
its executions. It does not close another track's binding or bump the shared
resource's generation. Repoint attaches the new selection and releases the old
binding transactionally. Rollback restores authorization through a new binding
version rather than reviving old tickets.

Resource retirement is separate from binding release: it is allowed only after
the last binding and all active execution/storage references are released.
Permanent identity/audit references do not imply a storage lease. Protected Git
content has its own pin references, separate from physical directory lifetime.
Borrowed legacy directories are never destruction targets. Existing
ignored cleanup still applies only to owned track worktrees after resource-wide
quiescence. This adds no S3 archive or retention policy. Multiresource fences
are acquired in workspace-ID order after existing card/drive/track/resume locks;
no code acquires those existing locks while holding a workspace fence. Git
metadata locks follow workspace fences; DB transactions stay short and contain
no filesystem/network wait.

### Executor authority, fast paths and crash states

Kernel truth owns durable launch authorization and occupancy; the executor's
owned journal is the single authority for ticket execution facts. The kernel
intent names the exact executor boot identity and ticket, so an unavailable or
replaced executor cannot answer for it. A control connection is authenticated
as the registered local kernel client; an unguessable registered ticket is a
capability, not a client-supplied resource path. S2a worktree registration opens
the root and verifies its directory identity; later namespace registration is
not smuggled in through arbitrary PID/path fields.

The supervisor holds one per-resource executor gate across identity validation,
ticket lookup/consumption, actual pipe/PTY fork and process registration. Revoke
uses the same gate. Before fork it also atomically reserves the globally unique
proc_id in the existing registry, recording the complete execution identity.
The registry contains Reserved as well as Spawned entries. A different resource
or ExternalDir racing for the same proc_id sees that reservation and is refused;
an exact retry inspects/waits for its original receipt without another fork.
Reservation and its journal identity are durable before fork. Startup rebuilds
the reservation index before accepting requests; ambiguous duplicates require
recovery. The global registry guard is short-lived, never spans fork/IO, and is
acquired only after the resource/ExternalDir executor gate; no path holds it
while acquiring another executor gate. Concurrent connections do not create
independent gates or overwrite a reserved entry. Journal transitions are:

| State | Allowed next state / evidence |
|---|---|
| Prepared | Revoke persists Revoked, or launch persists Forking before calling fork |
| Revoked | Terminal no-fork receipt; delayed launch is refused |
| Forking | Definite syscall failure persists NoChild; successful fork and verified process registration persist Spawned |
| Spawned | Receipt includes resource, binding/version, generation, epoch, operation, specification digest, PID/start-time/nonce and executor identity |
| NoChild | Terminal receipt with positive creation-failure evidence |
| Unknown | Blocks quiescence until execution evidence resolves it; never treated as Revoked/NoChild |

Journal state and its parent directory are fsynced before acknowledging the
transition. A revoke ACK is issued only after a durable Revoked/NoChild receipt,
or returns the identified Spawned receipt for the kernel to drain. Forking,
lost executor state or an interrupted registration returns Unknown. If a fork
wedges while holding the gate, revoke stays pending/Busy rather than succeeding
on a timeout. No new generation opens before these facts are reconciled.

The existing proc-ID fast path moves inside this gate and checks the complete
registered execution identity. An exact retry returns the original ticket's
receipt and never forks again. A reused proc_id with a different resource,
binding/version, generation, epoch, operation or specification is refused. Lost
fork replies recover by inspecting that ticket; they do not create a fresh
ticket. The original intent remains occupied until reconciled.

Attach/Probe use an authorized process capability bound to the same receipt;
they cannot adopt an old process by proc_id alone. Read-only observation of a
legacy process is a distinct artifact capability and cannot authorize stdin or
new work. WriteStdin is an execution action and rechecks current admission.
Known existing PTYs may be explicitly resumed only by a recoverable kernel op
that verifies their original process identity and unchanged binding/source
epoch, then advances the executor registration. Unknown PTYs keep the resource
Busy and remain observable; an upgrade does not kill them to manufacture idle.

### Minimal S2a resource cancellation and release

Materialization intents bind the selection and binding version. Binding release
first persists cancellation and closes admission under the resource fence;
materialization and its retries must check that version/cancellation before
each external effect and again before Ready CAS. Delete/repoint cannot race a
stale materialization into recreating an unbound workspace. The fence serializes
the final external effect with cancellation; crash recovery consults the same
durable intent before resuming.

Before deleting the last domain binding, persist an independent release intent
with the full descriptor, exact owned pin names/OIDs, retention disposition and
pending materialization IDs. Domain-row deletion cannot erase this information.
The existing recycle/area compensation and worktree sweep consume that intent,
record physical receipts and then CAS-release only this resource's owned pins
when no active execution/recovery or protected-content reference requires them.
A pin with an unexpected
OID is retained and requires recovery. A pin written before Ready is discoverable
from its original materialization intent and is included in cancellation.

Release is idempotent: a missing expected resource/ref can complete its receipt;
an unknown identity cannot. Partial failure retries siblings independently and
retains the release intent. Area rollback restores the same descriptor/binding
authorization and cancels the release through CAS; stale release drivers cannot
delete the restored resource. S2a preserves the existing seven-day trash policy
and worktree deletion behavior, without introducing S3 archives or branch policy.
Acceptance includes pin-written/Ready-not-written crashes followed by deletion,
repoint during ensure, area rollback, and repeated materialize/release replay.

Keyed operation rows and dedup sidecars remain permanent identity/audit history;
their existence alone holds neither execution storage nor a Git content pin.
Each registered adapter declares the active resource dependencies of its phases
through the same operation contract, without a second hand-maintained kind/phase
list. Pending, running or explicitly recoverable/stuck work retains the needed
dependencies until settlement or an authorized cancellation; terminal results
release execution dependencies transactionally. Independent source-evidence pins
remain while a real evidence consumer requires them. Completed-key replay returns
its original result using the original identity tombstone even after physical
release; it cannot resolve a current track, recreate storage or execute again.
Verify completed keyed work → track deletion → physical release → original-key
replay returning the original result, and workspace-free keyed retries. Also
verify a normal open terminal permits worker scheduling but prevents reclaim,
and different resources/ExternalDir racing for one proc_id create at most one
process without overwriting its receipt.

Verify new/legacy create and materialization retries, shared legacy lock domains,
repoint/launchpad and frozen binding behavior; immutable historical event and
operation bytes; every persisted replay owner; direct and supervisor launch
races, ticket replay/revoke/crash recovery, PTY and pipe readiness/attach;
content traversal/symlink/revision cases and real frontend previews. Mutation
verification targets stale-generation and ticket fences on an exclusive tree.
Run the parent design's G checks and applicable frontend/browser tests. Two
independent Codex L2 channels review abstraction boundaries, duplicate policy,
hardcoded assumptions, external contracts and maintenance cost before merge.
