# Link sharing and execution approval

Status: PR1 L2 design for #3633, using the current deployment resource policy.
PR1 supplies protected management and durable delegation, not visitor execution.
This decision supersedes the earlier frozen environment binding prerequisite.

## Outcome and delivery boundaries

The product shares the original Track or Area, Report and conversation. An Area
link includes its present and future Tracks. Possessing a link will permit chat
without account registration; creating a link explicitly approves that use.
There is no invitation, guest fork, manual Report edit or `allows_input` switch.

| Slice | Closed outcome | Boundary and evidence required |
| --- | --- | --- |
| PR1 | Protected management and persistent approval foundation | Authenticated deployment issuer, owned User Area, typed scope and immutable consent to the current deployment resource policy; independent hash-only access token and monotonic revocation; real HTTP transactions and reopen tests. No anonymous routes or execution cutover. |
| PR2 | Shared reads and invalidation | Narrow audience-aware Report/history/data projection, resource-owner decisions and scope-only live invalidation, including deletion and subscribe/snapshot races. No guest send endpoint yet. |
| PR3 | Original-conversation guest work | Explicit link-guest author and ShareDelegation provenance; acceptance binds exact original message/revision to approval. Reuse canonical receipt, pending order, source qualification, claim, dispatch and cold recovery. Revalidate current issuer, ownership and source at execution; loss holds the original work. |
| PR4 | Complete browser experience | SessionGate-independent composition, existing Report/conversation UI and narrow ports, copy/reopen/multiple-link cookies/origin/revocation and Area navigation; real preview and browser/Tier 1 coverage. |

Every slice is L2 because it changes authority or persistence boundaries. PR1
must not change MessageAuthor, dispatch, Private history or old Legacy JSON.
It must not register a visitor as a user or member. Sharing is not advertised as
executable until PR3 consumes the real resource approval through the existing
execution owners.

## Current deployment delegation

The actual deployment owner approves conversation use of this installation's
currently configured resources in a specified Track or Area. Configuration
changes apply as they do to ordinary owner work. This product does not freeze
credentials, an environment version, or a Workspace binding. It needs no
AreaEnvironmentBinding, new resource UUID, environment registry or fake version.
Existing Planner tools and resource permissions remain unchanged; an HTTP share
scope does not establish a new sandbox isolation boundary or complete #2671 C4.

The real [HumanInputSource](../../crates/calm-server/src/auth/input_source.rs)
proves current session/dev authentication. Raw Principal, role and Actor fields
cannot mint approval. The organization owner checks current deployment identity,
registration and actual User Area ownership. The share owner resolves Track
containment through owning Truth reads in the same transaction, rejects System
Areas, and consumes the same approval decision for create, reload/list and revoke.
The live source is rechecked immediately before commit. Existing provider and
Workspace admission remains responsible for actual execution in PR3.

`ShareScope` is a typed Track or Area target, stored with mutually exclusive
foreign keys. An Area includes present and future Tracks without enumeration.
`ShareDelegation` has immutable id, scope, approver user, conversation purpose,
current_deployment policy and creation time. The record's id is the stable
approval identity. Management consumes reloaded typed facts and validates target,
purpose, policy, current approver and current ownership. There is no extra
approval state machine, queue or bootstrap execution capability.

A single `share_links` aggregate holds those immutable facts plus independent
access facts: a hash of a strong random one-time token and monotonic `revoked_at`.
SQL fences prevent delegation edits, access resurrection and physical link
removal while the target exists. Revocation never changes delegation, pending
input, messages or receipts.
Future PR3 accepted work refers to immutable delegation facts and does not check
token revocation; reads and new acceptance require active access. Target deletion
uses existing lifecycle/FK cascades: Track links disappear with their Track, Area
links with their Area. Revoking a link never invokes target deletion. PR3 must
keep accepted work references within that same target lifecycle.

Protected POST/GET `/api/tracks/{id}/share-links`, POST/GET
`/api/areas/{id}/share-links` and DELETE `/api/share-links/{id}` are the entire PR1
HTTP surface. Creating a link approves chat by default, without `allows_input`.
Create returns the token exactly once; storage holds only its hash and listing
exposes neither. DELETE idempotently marks access revoked. No anonymous endpoint,
visitor registration, Private history change, historical JSON rewrite, provider
change or execution dispatch is included.

Add the next migration after 0214; released migrations are byte-frozen. Run the
real API/wire generator and update migration fixtures. PR2 notification work
reuses EventScope and establishes a unified Report resource owner; PR4 copying
preserves fragments. No new permissions panel or warning workflow is introduced.

## Architecture checks and permanence cost

| Check | Required ownership and maintenance consequence |
| --- | --- |
| Abstraction boundaries | Auth proves the live issuer; organization access decides current deployment/Area ownership; share access decides scope/access; Truth persists; routes consume those contracts. Execution owners retain runtime admission. |
| Duplicate logic | Extend the existing organization resource decision; do not copy its checks into a second share-only no-users branch. Reuse one canonical conversation, pending queue and source registry in PR3. |
| Hardcoded application assumptions | No new local-owner special case, per-event sharing list, new file/test/route census, or fake Ready. Closed scope/purpose variants are declared once in their owning typed contracts. Area descendants are resolved dynamically. |
| External contract facts | Use issuer/session facts and real Workspace/executor contracts. Never infer environment authority from filesystem paths, database IDs, provider private layouts or generated launch IDs. |
| Cost of permanence | Persist scope to give token and accepted work one stable target; persist immutable approval for exact provenance/reload/history; persist token hash and monotonic revocation to enforce access across restarts. These facts require one additive migration and owning codecs/fixtures, not a new execution ledger, cleanup worker or synchronization state machine. Ordinary new tests/routes use existing registration mechanisms and do not edit a second census. |

The new tests join the existing API/lib test registrations and run once in their
assigned CI shard (apart from retries); no new workflow invocation is added.
The task gate replays the share management and execution modules once. Ordinary tests need no
new census; a migration adds the existing head-fixture entry. A new scope variant
would change its typed vocabulary and owning codec plus an additive migration,
rather than a per-caller authorization list.

The current global `extra_users` guard is not a product invariant. PR1 removes
only that SQL check, preserving deployment identity,
Area/Private/membership and exact-source checks; it does not require completing
link sharing first. New delegation must not copy the population heuristic into
a parallel authorization branch. Full Share source integration is a separate
PR3 obligation.

## Acceptance evidence and verification ownership

Real HTTP cases belong to `domain_api_suite::share_links` in the existing
`api_suite` target. They must prove protected-session enforcement, forged Actor
rejection, unregistered/non-deployment/foreign owner refusal, System refusal,
scope/target/purpose/policy mismatch, unrelated second-user behavior,
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
issuer/current ownership and monotonic revocation assertions require the
repository's single-factor mutation verification in an exclusive recoverable
worktree, with the full predicted/actual red set compared before safe restore.

Verification plan: the worker runs focused execution and share HTTP tests,
formatter and real API generation; the kernel gate runs text, full L2 contracts,
quick Rust, focused replay, OpenAPI statuses and frontend validation. Planner owns
the two independent L2 reviews. PR1 makes no visitor execution readiness claim.
