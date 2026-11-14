# Link sharing and execution approval

Status: L2 scoped-session implementation for #3633, extending PR3 shared reads and accepted-turn execution.
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
| PR4 | Complete browser experience | ProductionApp scoped-session composition, existing Report/conversation UI and narrow ports, copy/reopen/multiple-link cookies/origin/revocation and Area navigation; real preview and browser/Tier 1 coverage. |

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
access facts: a SHA-256 hash of a server-generated random token, immutable `expires_at`, and monotonic `revoked_at`.
SQL fences prevent delegation edits, access resurrection and physical link
removal while the target exists. Revocation never changes delegation, pending
input, messages or receipts.
PR3 accepted work refers to immutable delegation facts and does not check
token revocation; reads and new acceptance require active access. Target deletion
uses existing lifecycle/FK cascades: Track links disappear with their Track, Area
links with their Area. Revoking a link never invokes target deletion. Track-scoped history keeps immutable delegation facts without an FK to the link.
A Track moved out of an Area survives deletion of that former Area and its links;
its accepted guest work remains held.

Protected POST/GET `/api/tracks/{id}/share-links`, POST/GET
`/api/areas/{id}/share-links` and DELETE `/api/share-links/{id}` are the entire PR1
HTTP surface. Creating a link approves chat by default, without `allows_input`.
Migration `0218_share_link_expiry.sql` replaces the branch-only token migration.
It stores no plaintext secret. Creation accepts `{}` and returns `{ link, token }`
once. Management reads expose expiry and revocation without recovering the token.
The server sets `expires_at = created_at + 86_400_000`; SQLite checks integer expiry
strictly after creation and prevents subsequent edits. Existing links receive the
same lifetime measured from their original creation time. DELETE idempotently
revokes access and immediately evicts all process-local sessions for that link.

Released migrations are byte-frozen; follow-up changes use additive migrations.
Run the real API/wire generator and update migration fixtures. PR2 notification work
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
The task gate replays `share_links` and `openapi_statuses` once. Ordinary tests need no
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
one-time token delivery and hash-only persistence, management replay and monotonic revocation, deletion,
reopen and migration of existing data. Approval reload and current validation
must be consumed by actual management transactions, not a test-only constructor.
PR3 will additionally bind and test exact message provenance and accepted work.

Before claiming coverage, use `cargo nextest list` on the actual registered
module and record the nonempty names. Focused runs and gate replay use
`--no-tests fail`; absent modules are not passes. Worker verification owns only
focused tests, real generation and formatting/necessary lint fixes. The kernel
gate owns text checks, full L2 contracts, Rust quick, one focused replay and
OpenAPI statuses. No frontend implementation changes are included. CI owns broad suites.
No real Codex E2E or workspace-wide nextest runs are permitted here.

Two independent L2 review channels remain Planner-owned. Critical scope,
issuer/current ownership and monotonic revocation assertions require the
repository's single-factor mutation verification in an exclusive recoverable
worktree, with the full predicted/actual red set compared before safe restore.

Verification plan: the worker runs focused owner/share HTTP tests,
formatter and real API generation; the kernel gate runs text, full L2 contracts,
quick Rust, focused replay and OpenAPI statuses. Planner owns
the two independent L2 reviews. PR3 acceptance is retained behind the ordinary scoped input entry below.

## Resource reads and retired token API

PR4 retires all eleven `/api/share/{token}/...` HTTP and WebSocket paths,
including the root token lookup and guest input adapter. REST revision 46 records
this compatibility break; the current frontend already uses ordinary APIs. There is no redirect,
compatibility forwarding or fallback. Existing clients must unlock a public
link identifier and use the scoped-session ordinary API described below; a raw
token is never treated as a public link id.

`report_resource` remains the owner of ordinary Report, series and captured-source
reads. Owner reads retain `require_detail_access` before assembly and
`require_detail_publication` afterwards. Conversation reads retain exact card
containment, publication checks, cursor/history behavior and local-image path
redaction. Scoped requests consume those same owners and full production
contracts, with the additional live scope middleware fence.

The retired narrow `SharedTrack`, `SharedTarget`, `SharedRun`, transcript and
series projections and Truth published-only page abstraction are removed.
Ordinary presentation reads still preserve current revisions, retry provenance,
Native/Imported history and pending content through their existing owner.
The old scope-only snapshot/invalidate/closed socket protocol is retired;
scoped clients use ordinary `/api/events` replay/live envelopes.

Share authorization facts remain owned by `share_access::ShareRead`,
`resolve_hash`, `resolve_id` and the owning Truth factual readers. They still
validate active access, current deployment issuer, registered User Area ownership
and containment. EventScope and EventBus access wakes remain shared infrastructure.
No released migration, stored delegation, historical guest author or execution
provenance is rewritten or removed.

Existing API registrations and the real generator remove the retired contracts;
there is no permanent retired-route census. Shared test fixtures live independently
of retired endpoint tests. Owner permission/publication races, the scoped Report revocation/publication race,
and scoped acceptance, receipt, expiry, revocation and creation regressions remain registered.

## PR3 guest execution boundary

`POST /api/cards/{id}/planner/input` with a selected scoped-session credential
requires `Idempotency-Key` and accepts text through the ordinary request contract.
Nonempty attachments and replacement fields are refused. Request Actor fields
cannot choose the guest author. Active access, current
issuer, registered User Area ownership (`AreaAccess::Delete`), dynamic scope
and a canonical Planner/Assistant binding are required before receipt replay
or fresh acceptance. Historical V1 cards return 409. The canonical receipt hash
includes the link id, so a key cannot cross Owner/Guest or Guest/Guest sources.
Only the link id reaches messages, events and receipt hashes; raw tokens never
enter the canonical queue or execution context. Acceptance emits the existing
`HarnessUserMessageEnqueued` event.

`MessageAuthor::LinkGuest` identifies the link without registering a visitor.
`SubmissionRenderInput::LinkGuest` names the canonical message revision and
link id; `input_ref()` remains `AcceptedInputRef::Human`. The existing Human V1
JSON and digest remain unchanged. `ConversationExecutionProvenance::ShareDelegation`
freezes the full immutable delegation with provider/profile on the logical Turn.
Truth reads those facts without joining the live link, so history remains
readable when the old Area and its approval disappear. The additive migration
is `0217_share_guest_execution.sql`: upstream already released
`0216_acp_shared_restart_release.sql`, which remains byte-frozen.

`InputSource::Share` retains the actual deployment issuer declaration from
`ValidatedApplicationAuth::from_state` on scoped acceptance and from the installed
Dispatcher on recovery. Claim, model reads, Workspace admission and native
crossings revalidate the live delegation, current deployment issuer, registered
Area owner and current Track/Area containment. They deliberately ignore
`revoked_at` and `expires_at` for accepted work. Revocation blocks new reads/sends and receipt
replays; ownership, issuer or scope loss holds the original input without
reminting its identity or deleting its history. Area scope excludes area-chat.
Cold reconciliation reloads pending and unattempted guest sources through the
same owner. No session cookie, raw token or stored Actor supplies execution authority.

Owner and each link select only their contiguous qualified pending prefix.
Different execution provenance cannot enter an active Turn through supplemental
claims; it stays pending for the next logical Turn. Owner and scoped-session transcript
reads decode guest text with the public label `Link guest`. Ordinary conversation lists use their existing metadata owner.

Verification ownership remains L2: the worker runs focused HTTP/lifecycle tests,
real API generation, formatting and single-factor ownership/scope/revocation
mutations. The declared kernel gate owns text gates, all contract gates, Rust
quick gates, frontend lint/build/OpenAPI contract checks and one focused replay.
Planner owns both independent review channels. The migration rebuild preserves
the dependent graph, indexes, triggers and presentation sequence and checks only
new foreign-key violations. Ordinary new tests use the existing registrations;
no new test census or CI invocation is introduced.

Guest presentation reuses ProductionApp, TrackRouteBody, NewTrackPage and the existing conversation components with an explicitly selected share credential. The earlier public token read/input routes are retired without compatibility forwarding or fallback.

## Separated link and unlock session

The browser navigates with the public link id; the separately delivered token is
submitted only to `POST /api/share-links/{id}/unlock`. `GET
/api/share-links/{id}/access` returns 404 for a missing, revoked, expired or
no-longer-owned target. A locked response contains only `status` and `expiresAt`.
An unlocked response additionally contains the typed scope. Both endpoints are
public OpenAPI operations. Unlock checks Origin and the existing per-peer login
throttle, then compares fixed-size SHA-256 digests in constant time. Wrong tokens
return 401; throttling returns 429 with Retry-After. Only the digest, public id,
and expiry remain in the process-local credential store.

Each link sets its own `calm-share-session-<id>` cookie with Path=/, HttpOnly and
SameSite=Strict. `X-Calm-Share-Link` or `share_link` selects only that cookie for
whoami, logout, HTTP and WebSocket authentication. Owner cookies, Bearer tokens,
other links and development auto-login cannot satisfy that selector. Conflicting,
duplicate or malformed selectors fail closed. Logout clears only the selected
cookie; restarting the process requires unlocking again.

`ShareAccessScope` owns a fail-closed HTTP allowlist in `shared_router`. Every
scoped request and upgrade rechecks live SQLite access, current owner and current
containment. Admission uses the credential authority captured during authentication,
so evicting both session stores cannot turn an in-flight ShareLink request into
an owner request. Finite responses repeat validation before publication. Track shares
include non-area-chat descendants through Truth's bounded tree query; Area shares
include current non-area-chat Tracks. Cards and terminals resolve their actual
Track. Owner-only global routes, sharing management, Area writes and Track DELETE
are forbidden. Lists of Areas, Tracks, overlays, mentions and backlinks are
filtered in their owning readers. Mentions filter reports before ranking so
sibling titles, tags and blocks cannot influence or enter the result. Read
receipts are synthetic for share sessions and never modify owner SQLite state.

Filesystem browsing uses the existing `listdir`, `readfile`, `readfile-raw`,
`gitstatus` and `gitdiff` routes, constrained to canonical in-scope `agent_cwd` roots;
`/api/fs/roots` lists only those roots. Git status filters changed files and rename
sources to those roots; diff checks both current and old paths. Workspace reads retain the existing
handle-based path fence. Markdown subresources may select a matching live share
cookie only for the exact workspace raw-file or card attachment route, when no
explicit selector and no valid owner/dev authentication exists.

The regular `/api/events` WebSocket filters both replay and live envelopes by
current EventScope, excluding System and area-chat. Access wakes and an expiry
timer close the socket on revoke or expiry. Terminal upgrades use the same
scope fence and their connections end on access loss. Scoped Planner text input
reuses the original LinkGuest acceptance path, never owner message attribution.
Accepted canonical turns retain PR3's ownership/scope checks while deliberately
ignoring revocation and expiry. These access changes cannot retract work already
accepted by the resource owner.

The scope allowlist lives in the share feature and does not add a generic kernel
route census. New global routes remain forbidden until intentionally admitted;
new Track/card subroutes inherit their owning resource fence. Public schema and
wire bindings are regenerated with `cd fe && npm run gen:api`. Frontend domain
and browser implementation belongs to the separate frontend task.

Scoped shell reads return an empty settings bag and empty Area folders, so mount
and new-Track flows never read owner settings or host folder claims. PUT settings
and folder mutations remain forbidden. Track recipes currently have a global
reusable contract with no private or Area-bound variants; GET list/detail uses
that existing owner. Any later recipe visibility contract must be enforced there.

Both POST /api/tracks and POST /api/areas/{area_id}/tracks admit creation only
for the selected Area share. The request Area must match; explicit cwd, admin,
cross-Area cwd admission and out-of-scope fork sources are forbidden. Empty
creation reuses dormant allocation. First-message creation and Add Assistant
reuse the same creation owner, immutable receipts, pending-message acceptance
and canonical wake, with LinkGuest author and InputSource::Share. Discussion
branches are forbidden. PATCH refuses admin grants and workspace mutations.

Migration 0219_share_creation_receipts.sql extends the existing receipt proof
to LinkGuest messages without weakening dormant carrier/allocation checks.
source_user_id remains the registered approver for factual attribution; replay
also compares the original immutable message author (including link id).
A key cannot cross Human/Guest or two different links. No visitor user or
membership is created. New tests use the existing share_links registration;
no extra CI invocation or permanent test census is added.
