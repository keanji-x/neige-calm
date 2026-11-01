# Objective approval presentation (#3423)

Review tier: L2. This extends a persisted ask contract used for user authorization.

## Outcome and acceptance

Every supported objective can request approval regardless of the generated title's
length. Users can inspect the complete frozen definition. Approval still binds the
track, objective key and existing standard hash; edits to standard fields invalidate
approval, while title, budget and compact edits do not. Repeated requests reuse the
same open ask. Only the authenticated user's approval option authorizes.

## Contract and ownership decision

Add optional plain-text `detail` to the generic AskQuestion vocabulary. Ordinary
questions need no detail. New objective approvals always supply the complete
serialized ObjectiveDefinition as detail and use a constant bounded title. The
snapshot is written in the existing ask.requested event, atomically with its action.
This avoids an additional store, mutable Report links and any definition truncation.
The objective approval adapter validates that its stored snapshot parses as the
owning domain's ObjectiveDefinition and hashes to its action identity. Shared ask
validation transports detail unchanged; UI renders it as text, never executable HTML.

The existing domain definition and standard_hash remain authoritative: do not copy
standard field selection into server/UI code or change canonical hashing. Full
snapshot serialization intentionally includes title, budget and compact as context;
those fields remain outside approval identity. A second public standard model is
unnecessary for this presentation-only change.

New asks carry `objective_approval_v2`; its detail is mandatory at the objective
adapter. Older frontend schemas reject that unknown action rather than offering
approval while dropping detail. Both action versions share one hash/authority path.
Historical events without detail must parse their complete inline JSON definition
and match the action key/hash to keep their existing inline-title contract. No
migration or rewrite of historical events is needed. New requests have no path to
omit their detail; legacy support is explicit at the objective adapter. Keep generic
ask and provider/native title bounds unchanged. The Report admission limits (2,048 characters per string and 256 KiB canonical
payload) remain authoritative. Detail introduces no additional definition size limit: the existing persistence path already accepts the same full definition.
An independent resource bound would need definition-admission design and must not
silently truncate criteria or fail only at approval time.

## Surface and verification

Sweep Rust AskQuestion constructors and validators, generated TS, frontend runtime
schemas and domain types, and the existing Planner ask renderer. Planner answer wake observations must also
carry the complete persisted detail so approve/decline still names the exact standard;
live and boot catch-up share that same generic rendering entry point. Snapshot display
must preserve the last criterion on desktop and narrow screens. Queue labels remain
short. Regenerate bindings through the real generator.

First reproduce the >2000-character generated title through neige_objective_ask.
Cover ordinary/oversized definitions, exact snapshot/hash, repeats, changed standards,
metadata-only edits, stale answers, invalid snapshots, user authority and rollback.
Mutation-verify the snapshot/hash fence in an exclusive worktree. Run focused Rust
tests, complete contract gates, quick Rust gates, text gates, frontend tests/lint/build,
relevant browser tests and a real browser preview. Two independent review channels
inspect the frozen final change in separate worktrees; fix and re-review blockers.
CI owns broad Rust suites. Squash merge only after required CI and reviews converge.

## Risk and rollback

Older clients reject the new approval action; they must refresh to view it. Server
and frontend must deploy together. Older stored questions remain readable. Rollback requires retaining the versioned action reader, since pre-change servers
cannot deserialize new event variants. No released migration is changed.

## Frozen interface change request

Approved by the task orchestrator under the user's authorization to implement #3423:
core/api adds optional question detail and recognizes objective_approval_v2; generated
bindings are regenerated. No other frozen frontend interface or style policy changes.

OWNERSHIP-CHANGE: fe/core/api/schemas.ts — approve complete ask details and versioned objective approvals (#3423)
OWNERSHIP-CHANGE: fe/core/api/schemas.contract.test.ts — verify complete approval detail wire decoding (#3423)
OWNERSHIP-CHANGE: fe/core/api/generated/wire.ts — generate complete ask detail and versioned approval types (#3423)

## Compatibility negotiation

Raise the server and maintained bundle web compatibility floor from 67 to 68 to
request refresh before handling the new approval presentation. The V2 action also
fails closed for already mounted older tabs that have not repeated preflight.
No sync event version bump: this is an additive nested action kind plus optional
question detail, not an envelope/source-identity reinterpretation. Both action
versions remain readable; historical source bytes are untouched. No new REST
endpoint or answer contract is required, so REST_API_VERSION remains 44.
