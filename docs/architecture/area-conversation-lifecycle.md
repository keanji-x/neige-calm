# Area conversations: ownership and lifecycle

Status: canonical input foundation for #2671; shared execution and named-account
activation are not delivered by this foundation. The implementation uses the
published operation, worker, native protocol and Workspace owners.

## Domain boundaries

| Owner | Owns | Does not establish |
| --- | --- | --- |
| Auth | The current live Human issuer and registered user identity | Area access or permission to spend a deployment's credentials |
| Dispatcher | A current producer and qualified durable event observations | Human authorship, a reusable execution grant or its own message queue |
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

The current production execution path is narrower than this domain. The actual
boot binds one validated deployment configuration to Auth and Dispatcher before
publishing the router. Private execution requires the configured owner to be the
only registered user; a User Area must contain exactly that owner, and a System
Area must have no members. Human input additionally needs its current live
issuer to own the exact revisions. System input needs its actual current
Dispatcher and durable event binding. Area membership alone never grants use of
the deployment's private credentials.

## One original lifecycle

1. The actual authenticated HTTP ingress selects the persisted input contract.
   Original creation-key replay takes precedence; a new supported profile uses
   its declared format. Native availability and authorization errors cannot
   select historical storage as a fallback.
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

Human and System references share that checkpoint and its order. The source
index binds an accepted reference to its live issuer; it neither orders nor
hydrates messages and caches no permission. A System source retains its original
event/content evidence for binding checks; SQL still owns the accepted input.
The index lifetime follows the original worker,
including dormant native handles, so accepting a second source must not depend
on restoring the first source's execution. Every claim and native crossing still
asks the source owner for current authority. Only explicitly consumed or retired
references can be removed; publishing an older frame cannot erase a newer
accepted source.

Dispatcher consumes the existing durable event prefix. Qualification, mapping
and enrichment stay in their existing event owners; a failed qualification read
holds the prefix rather than treating an unknown fact as permission. Acceptance
and the existing push watermark commit together. Cold recovery verifies the
original event row against its once-accepted frozen observation; later report
enrichment does not rewrite that accepted input.

Closed, idle conversations use the existing Planner hibernation sweep and loop
quiescence. The current checkpoint and native facts determine whether work is
still pending; historical accepted attempts alone do not mean a turn is active.
Sleep releases the native handle and provider resources while retaining the
original record and pending-source ownership. A later authorized wake uses the
same start and recovery owners. Resource changes that require replacing this
record are refused before worker, native or file effects.

A canonical accepted Stop binds its original native target and durable intent. That drain
can finish after logout or failure of new-work admission. A later request must
pass current authorization, and a stale target cannot stop a later turn. An
interrupt acknowledgement is not END.

## Compatibility and exit conditions

| Branch | Reason and lifetime | Condition for removal or expansion |
| --- | --- | --- |
| Historical persisted snapshots and released wire contracts | Necessary compatibility for existing records; each worker selects exactly one format | Remove only after an explicit reviewed data migration or retirement of those records; canonical errors never select this branch |
| Historical first-message preparation for existing templates, resources and providers | Their actual canonical creation and resource qualification are not yet implemented | Move each supported creation variant through its owning canonical acceptance and real resource lifecycle, with production-flow acceptance; do not add another preparation manager |
| Canonical retained Codex Planner activation | This is the provider/profile whose original start, dispatch and recovery have been integrated | Expand only with that backend's actual start, dispatch, result and recovery implementation and tests; a capability flag alone is insufficient |
| Workspace changes on a Track with retained accepted work | The existing repoint lifecycle retires workers and creates replacements. It must refuse before fencing workers or moving files, including pristine directories and work saved before native startup | Remove this restriction only when the resource lifecycle can rebind the original worker and native context while preserving input versions, plans, creation acknowledgements and recovery; prove the production migration and refusal/rollback paths |
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
The published executor work in #2795 and the Private lifecycle protocol in #2820
do not qualify a shared provider. #2820's executor/session contracts are consumed
unchanged; they establish process facts rather than Area authorization. The
engine and ports in #2826 likewise need their real kernel, authorization and
resource-service consumers before this feature can use them for shared work.
The missing production resource path is tracked in #2818.
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
Include mixed-source cold recovery with no second send, failed System
qualification with no watermark advance, workspace-change refusal with the
original conversation still usable, and close/sleep/wake on the same identities.

Component tests and independent forward/reverse reviews do not replace that
flow. The final combined commit also requires generated-contract verification,
focused historical compatibility checks, frontend/browser checks, the repository
gates and two independent L2 approvals. Do not run real Codex E2E on the shared
production host.
