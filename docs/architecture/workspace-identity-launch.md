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

Verify new/legacy create and materialization retries, shared legacy lock domains,
repoint/launchpad and frozen binding behavior; immutable historical event and
operation bytes; every persisted replay owner; direct and supervisor launch
races, ticket replay/revoke/crash recovery, PTY and pipe readiness/attach;
content traversal/symlink/revision cases and real frontend previews. Mutation
verification targets stale-generation and ticket fences on an exclusive tree.
Run the parent design's G checks and applicable frontend/browser tests. Two
independent Codex L2 channels review abstraction boundaries, duplicate policy,
hardcoded assumptions, external contracts and maintenance cost before merge.
