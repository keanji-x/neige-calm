# Area conversations: ownership and lifecycle

Status: canonical input foundation for #2671; shared execution and named-account
activation are not delivered by this foundation. The implementation uses the
published operation, worker, native protocol and Workspace owners.

## Domain boundaries

| Owner | Owns | Does not establish |
| --- | --- | --- |
| Auth | The current live Human issuer and registered user identity | Area access or permission to spend a deployment's credentials |
| Organization access / Truth membership | Current Area content and management permissions | A shared execution environment |
| Conversation inputs | Input acceptance and current execution-source admission, with one bound submission plan | A second queue, process driver or Workspace lease |
| Truth conversation storage | Immutable message versions, original submission identities, one current input checkpoint, factual result transitions | Reusable authorization from a recorded author |
| Existing start adapter / operation driver | Activation of the already accepted worker and the original operation | Replacement messages or executions after an uncertain outcome |
| Backend / native transport | Actual dispatch, receipts, original execution targets and qualified END | Completion from an interrupt acknowledgement or an absent local cache |
| Workspace service | Existing resource identity, materialization, occupancy and recovery facts | Area membership or conversation policy |
| Conversation read / frontend projection | Protected display facts, one transcript and existing outbox reconciliation | New execution authority or authors inferred from text |

Area is the collaboration boundary. Track organizes work and selects its actual
workspace. Conversation holds messages; a logical turn consumes fixed versions.
The sender's identity remains distinct from the execution identity.

The current production execution path is intentionally narrower than this domain:
only the existing deployment owner can use its Private legacy environment, and
its live issuer must own the exact Human revisions being dispatched. Area
membership alone never authorizes that environment.

## One original lifecycle

1. The actual authenticated HTTP ingress selects the persisted input contract.
   Acceptance writes the immutable message, original receipt and pending reference
   in the same transaction. A native wake failure does not undo that acceptance.
2. The start fence wakes the original worker through the installed start adapter
   and operation driver. Retained activation cannot allocate a replacement worker
   or reinterpret canonical storage as a historical queue.
3. The conversation owner binds current source, fixed revisions, original worker
   and submission together. Truth claims that plan on the sole checkpoint. Body
   hydration reads those versions; native echoes never become canonical inputs.
4. Input dispatch commits a crossing marker before the native effect. The native
   transport invokes admission again after its connection gate, using the same
   owning policy and actual original execution fence. Passing a preflight is not
   permission across a later await.
5. Returned receipts and native content are factual writes. They may still be
   recorded on the exact current Failed carrier; doing so never restores authority
   to start more work. Only qualified END closes the original execution.
6. Boot restores facts. A new live issuer may resume an idle original thread or
   re-admit its exact unattempted Initial claim through the original recovery owner.
   An unresolved active claim with any existing input attempt remains held: a new
   login or appended message cannot turn CrossingUnknown, Unknown or Accepted
   into permission to resend. Completed submissions do not block new input.

An accepted Stop binds its original native target and durable intent. That drain
can finish after logout or failure of new-work admission. A later request must
pass current authorization, and a stale target cannot stop a later turn. An
interrupt acknowledgement is not END.

## Compatibility and exit conditions

| Branch | Reason and lifetime | Condition for removal or expansion |
| --- | --- | --- |
| Historical persisted snapshots and released wire contracts | Necessary compatibility for existing records; each worker selects exactly one format | Remove only after an explicit reviewed data migration or retirement of those records; canonical errors never select this branch |
| Historical first-message preparation for existing templates, resources and providers | Their actual canonical creation and resource qualification are not yet implemented | Move each supported creation variant through its owning canonical acceptance and real resource lifecycle, with production-flow acceptance; do not add another preparation manager |
| Canonical retained Codex Planner activation | This is the provider/profile whose original start, dispatch and recovery have been integrated | Expand only with that backend's actual start, dispatch, result and recovery implementation and tests; a capability flag alone is insufficient |
| Private deployment-owner execution and exact live-source binding | The deployed daemon uses private deployment credentials | Add a real approved common environment and shared provenance at the organization, Truth, Workspace and provider owners before admitting other authors; never relax only the server-side author comparison |
| Negotiated conversation display alongside historical raw reads | Existing clients retain their released response shape | Retire the old read only through the client compatibility policy; both shapes read the same stored facts, not independent message histories |

A historical acknowledgement may legitimately have no editable entry identity.
Source authentication does not upgrade that data contract. Unknown history stays
unknown. Published historical queue identity is the complete `{id, rev}` pair;
partial metadata, equal text and the current viewer do not establish identity.
Canonical message identity remains its original message and author identity.

## Explicit remaining dependencies

The existing Workspace identity and executor primitives do not supply a public
resource resolve/launch/content service or a shared Area environment. #2666's
closed issue state and the stopped draft #2770 do not establish those capabilities.
The published executor work in #2795 also does not qualify a shared provider.
Implementation must consume real delivered contracts rather than define unused
adapters, construct a fake Ready value or inherit a member's private HOME.

Historical edit/delete/steer/Stop authority closure is tracked separately by
#2775. Public named-account activation also requires the remaining live access,
session revocation, filesystem, proxy and global-resource closure. These are
requirements for the multiuser release, not claims made by this foundation.

## Acceptance

The integrated target must exercise the actual password/session HTTP boundary,
commit a message, activate its original worker, claim and dispatch its exact
versions through a real local native protocol peer, record qualified results,
close and reopen the database, and continue on the original identities. Verify
both unattempted re-admission and attempted no-resend cases; preserve pending
messages and truthful held feedback across failure and restart.

Component tests and independent forward/reverse reviews do not replace that
flow. The final combined commit also requires generated-contract verification,
focused historical compatibility checks, frontend/browser checks, the repository
gates and two independent L2 approvals. Do not run real Codex E2E on the shared
production host.
