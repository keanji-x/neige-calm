# Link sharing and execution approval

Status: proposed L2 design for #3633; implementation is blocked on the resource
authorization owner described below. This document does not introduce a usable
share capability or claim execution support. Source inspection used baseline
`f2d204ca594aa1817adf538c9d1b74d276683fa1`.

## Outcome and delivery boundaries

The product shares the original Track or Area, Report and conversation. An Area
link includes its present and future Tracks. Possessing a link will permit chat
without account registration; creating a link explicitly approves that use.
There is no invitation, guest fork, manual Report edit or `allows_input` switch.

| Slice | Closed outcome | Boundary and evidence required |
| --- | --- | --- |
| PR1 | Protected management and persistent approval foundation | Authenticated deployment issuer, owned User Area, typed scope and immutable execution approval bound to real resources; independent hash-only access token and monotonic revocation; real HTTP transactions and reopen tests. No anonymous routes or execution cutover. |
| PR2 | Shared reads and invalidation | Narrow audience-aware Report/history/data projection, resource-owner decisions and scope-only live invalidation, including deletion and subscribe/snapshot races. No guest send endpoint yet. |
| PR3 | Original-conversation guest work | Explicit link-guest author and ShareDelegation provenance; acceptance binds exact original message/revision to approval. Reuse canonical receipt, pending order, source qualification, claim, dispatch and cold recovery. Revalidate current resource approval; loss holds the original work. |
| PR4 | Complete browser experience | SessionGate-independent composition, existing Report/conversation UI and narrow ports, copy/reopen/multiple-link cookies/origin/revocation and Area navigation; real preview and browser/Tier 1 coverage. |

Every slice is L2 because it changes authority or persistence boundaries. PR1
must not change MessageAuthor, dispatch, Private history or old Legacy JSON.
It must not register a visitor as a user or member. Sharing is not advertised as
executable until PR3 consumes the real resource approval through the existing
execution owners.

## Existing owners and the missing prerequisite

The real call paths establish some, but not all, of PR1's prerequisites:

- [Live request source](../../crates/calm-server/src/auth/input_source.rs)
  resolves session/dev authentication and rechecks the issuer in
  `HumanInputSource::current_principal`. A supplied Principal, role or Actor is
  not a substitute. [Session](../../crates/calm-server/src/auth/session.rs)
  separately records credential provenance; deployment management policy must
  use the actual issuer rather than infer provenance from the display role.
- [Validated deployment auth](../../crates/calm-server/src/auth/deployment.rs)
  provides only `LegacyPrivateExecution`, backed by a weak reference to current
  AuthConfig. [Boot](../../crates/calm-server/src/application_boot.rs) activates
  that declaration for dispatcher and maintenance execution. It has no durable
  deployment resource binding or approval identity/version.
- [Organization access](../../crates/calm-server/src/organization_access.rs)
  owns the registered-owner/Private execution decision. Its Human and automation
  wrappers converge on `require_legacy_deployment_area_tx`. The
  [Truth membership owner](../../crates/calm-truth/src/db/sqlite/area_membership.rs)
  supplies registration and current membership facts, expressly not permission
  to execute using another authority. System Areas have no membership.
- [Workspace selection](../../crates/calm-truth/src/db/sqlite/workspace_resource.rs)
  exposes `WorkspaceObservation`, selection version and Pending/Observed directory
  facts. [Migration 0166](../../crates/calm-truth/migrations/0166_workspace_resources.sql)
  expressly states that these do not authorize launch, admission or destruction.
  A selection ID, observed inode, pathname or database identity cannot become an
  environment grant.
- A real execution engine exists. Its
  [ExecutionAuthority](../../crates/workspace/src/execution/mod.rs) contains grant,
  authorization binding and version. However, the
  [provider launch owner](../../crates/calm-server/src/provider_execution/launch.rs)
  and [terminal owner](../../crates/calm-server/src/operation/terminal_execution_create.rs)
  currently mint fresh grant/binding IDs with version 1 inside each launch's
  `PrivateSource`. Those identify individual execution intents, not a persistent
  organization resource approval that link management can issue and revalidate.
- The [canonical execution owner](../../crates/calm-server/src/conversation_inputs/source.rs)
  validates the actual source and records only LegacySingleUserDeployment. It
  cannot manufacture a Share approval by relabeling that result.

The missing owner contract is a current deployment resource authorization
decision, issued by auth/organization access and durably represented by Truth:
it must identify the actual resource set and authorization binding/version,
the approving deployment identity, the approval identity/version, the owned
target and the execution purpose. It must resolve and validate the same binding
after reopen, and define
what real resource/configuration changes invalidate it. Workspace/execution
owners must identify the resource they actually consume; share management must
not create a parallel resource registry or infer that identity from a path.

The existing
[Workspace launch proposal](workspace-launch-admission.md#authority-consume-c1-do-not-invent-identity)
places AreaEnvironmentBinding with collaboration authorization and explicitly
does not deliver it. Its future Ready/PreparedLaunch APIs are not present-day
approval issuers. PR1 must not construct them to fill this gap.

Deciding whether the resource is an explicit deployment credential/environment
binding or another already-owned resource requires its owning contract and
invalidation semantics. Creating a UUID in share storage, pinning version 1,
or serializing the current owner does not establish those facts. This is the
specific prerequisite to resolve before implementing PR1's approval migration
and successful create transaction. No schema-only approval with no real issuer
or current-resource validator should be shipped as the promised foundation.

## Proposed PR1 contract after that prerequisite

`ShareScope` is a required typed Track or Area target. It has one authoritative
stored representation; access tokens and approvals reference it instead of
storing independently mutable copies. Track containment is resolved from Truth
in the management transaction; an Area scope includes future children without
enumerating them. System targets and cross-Area targets are rejected.

`ShareExecutionApproval` is immutable and has a stable identity and required
approval version, approving deployment user, scope,
resource authorization binding/version and typed execution purpose. These are
server-issued facts, not request-controlled owner/resource/version assertions.
The approval represents the creator's explicit delegation for chat. It is not a
Ready state, execution queue or promise of provider availability.

`share_access` owns scope resolution, management policy and access revocation;
`organization_access` owns the sole current deployment resource decision used
by share management and future Human/Dispatcher/Share adapters. Truth stores and
loads facts and enforces persistence invariants. Routes consume these decisions
without duplicating authorization SQL. Revalidation checks the stored approval
against the current issuer identity, owner, target, resource, purpose and
version. Missing or mismatched approval fails closed without Legacy fallback.

Protected routes are POST/GET `/api/tracks/{id}/share-links`, POST/GET
`/api/areas/{id}/share-links` and DELETE `/api/share-links/{id}`. Creation uses
live issuer proof and registered ownership of the actual User Area. Listing and
revocation require the current authenticated approving deployment owner and
current target authorization. Merely owning an Area or presenting Actor headers
cannot mint deployment approval. Mutation checks run inside the committing
Truth transaction and recheck the actual source at that boundary.

Access tokens use cryptographically strong random bytes, with only their hash
persisted. Create returns the secret once; listing exposes neither secret nor
hash. No API promises retrieval of a previously issued full link. Revocation is
an idempotent one-way access fact and preserves approval, attribution and
history. PR1 has no public token exchange/read/send endpoint.

The approval and token may use separate tables within one aggregate, but token
revocation cannot update the approval or pending/message/receipt facts. Future
accepted input references the immutable approval, never reconstructs it from a
currently active token. Resource invalidation holds accepted work through its
original owner; access revocation only prevents future access and acceptance.
Target deletion uses existing lifecycle behavior and retains history needed by
still-existing work. Reopen or target recreation cannot revive a revoked token
or rebind an old approval to a new resource.

Add the next migration only at implementation time; the inspected head is
0214. Released SQL bytes remain frozen. Existing rows are not backfilled as
Share or rewritten as a different Legacy source. Real generators update every
API/wire artifact and current migration/head-schema fixtures.

## Architecture checks and permanence cost

| Check | Required ownership and maintenance consequence |
| --- | --- |
| Abstraction boundaries | Auth proves the live issuer; organization access decides resource authority; share access decides scope/access; Truth persists; routes and future UI consume declared results. Workspace and execution owners retain actual resource identity and lifecycle. |
| Duplicate logic | Extend the existing organization resource decision; do not copy its checks into a second share-only no-users branch. Reuse one canonical conversation, pending queue and source registry in PR3. |
| Hardcoded application assumptions | No local-owner special case, per-event sharing list, new file/test/route census, or fake Ready. Closed scope/purpose variants are declared once in their owning typed contracts. Area descendants are resolved dynamically. |
| External contract facts | Use issuer/session facts and real Workspace/executor contracts. Never infer environment authority from filesystem paths, database IDs, provider private layouts or generated launch IDs. |
| Cost of permanence | Persist scope to give token and accepted work one stable target; persist immutable approval for exact provenance/reload/history; persist token hash and monotonic revocation to enforce access across restarts. These facts require one additive migration and owning codecs/fixtures, not a new execution ledger, cleanup worker or synchronization state machine. Ordinary new tests/routes use existing registration mechanisms and do not edit a second census. |

The current global `extra_users` guard is not a product invariant. This PR1
leaves the old execution chain unchanged as scoped. Removing that SQL check is
an independent narrow change that can preserve deployment identity,
Area/Private/membership and exact-source checks; it does not require completing
link sharing first. New delegation must not copy the population heuristic into
a parallel authorization branch. Full Share source integration is a separate
PR3 obligation.

## Acceptance evidence and verification ownership

Real HTTP cases belong to `domain_api_suite::share_links` in the existing
`api_suite` target. They must prove protected-session enforcement, forged Actor
rejection, unregistered/non-deployment/foreign owner refusal, System refusal,
scope/target/resource/purpose/version mismatch, unrelated second-user behavior,
one-time token secrecy, management replay and monotonic revocation, deletion,
reopen and migration of existing data. Approval reload and current validation
must be consumed by actual management transactions, not a test-only constructor.
PR3 will additionally bind and test exact message provenance and accepted work.

Before claiming coverage, use `cargo nextest list` on the actual registered
module and record the nonempty names. Focused runs and gate replay use
`--no-tests fail`; absent modules are not passes. Worker verification owns only
focused tests, real generation and formatting/necessary lint fixes. The kernel
gate owns text checks, full L2 contracts, Rust quick, one focused replay,
OpenAPI statuses and frontend lint/build/full tests. CI owns broad suites.
No real Codex E2E or workspace-wide nextest runs are permitted here.

Two independent L2 review channels remain Planner-owned. Critical scope,
issuer/resource binding and monotonic revocation assertions require the
repository's single-factor mutation verification in an exclusive recoverable
worktree, with the full predicted/actual red set compared before safe restore.

At this design checkpoint no management routes, new migration, generated API
changes or share tests have been implemented. Resource authorization issuance
and current validation remain unresolved; there is no PR1 execution-readiness
claim or L2 review/gate-pass claim.
