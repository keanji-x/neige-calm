# Managed conversations and provider ownership

Assistant and Planner share one execution, tool capability and approval chain.
This supersedes the former restricted Assistant product policy. See
[conversation permissions](design-conversation-permissions.md) for the current
L2 design and [verification](verification-conversation-permissions.md) for stable
commands. The Planner arranges the two formal L2 review channels.

## Identity and capabilities

A conversation keeps its actual `CardRole::Assistant`, `WorkerContract::Assistant`,
`WorkerSessionKind::ManagedAssistant`, provider binding and `AiManagedSession`
actor. Planner keeps its existing identities. The Track still has exactly one
current Planner for scheduling, tenure, root identity and ordinary task
notifications. Sharing execution capabilities does not promote every card or
repoint the Track root.

`CardRole::capability_role` defines the shared capability interpretation.
MCP discovery and admission consume it; event writes resolve the actual live
session before using the existing Planner verdict. Principal authorization
continues to prove the actual card, Track, provider, current pointer and live
session. Credential authentication and cross-Track restrictions remain in their
existing owners. Report attribution stays Assistant for Assistant writes.

Task declarations, objectives, terminal operations, ordinary questions and
Planner tools use their existing owner contracts. User-controlled declarations,
immutable standards, kernel lifecycle facts and external plugin grants retain
those contracts. There is no special Assistant Hold authorizer or separate
responder service. The shared Planner instructions owner serves creation,
issuance and recovery; the old restrictive Assistant prompt copies are retired.

## Creation and recovery

The existing creation operation freezes provider, model, effort, input and
permission choice atomically. Ambiguous retries reuse the exact original
request. Typed provider metadata remains owned by `PlannerBackend`; adapters own
native policy, availability, discovery, images and compact behavior. UI consumes
those declarations without maintaining another provider policy list.

Codex, Claude and OpenCode use the same managed conversation dispatch and held
request table. Native answers go only to the original live responder. New asks
carry their real source card in the event envelope and their real session actor.
Activity projection requires source card and nullable original session (null
only for historical direct-card actors). Missing session provenance never falls
back to the current Planner. The answer route and shared drawer serve every
conversation; canonical event acceptance and recovery route Wake answers to the
original card/session. General task scheduling notifications retain the current
Planner owner.

Existing typed Assistant sessions and serialized creation operations remain
readable. Existing provider-native instruction snapshots are not rewritten in
place: frozen running/retry turns retain their original inputs. The normal
restart/new-thread owner renders shared instructions; MCP capability changes
apply to the existing credential's live identity immediately. No card-role conversion or new identity migration is required. Released
migrations remain byte-frozen; draft 0222 retains the independent Never backfill.
Queued inputs, unknown native outcomes and frozen invocations retain the existing
recovery contracts. A restart never recreates a native Allow responder.

## Verification and delivery

Focused real fake-provider requests must exercise Allow and Deny on Codex,
Claude and OpenCode, with real MCP credentials and HTTP answers. Tests cover
same-Track conversation separation, stale source sessions, provider connection
loss, retry/resume, role switching and first-turn Never/Ask/Full choices. Worker
runs focused checks, real generators and the minimum critical mutation; the
kernel gate owns final text, quick Rust, contracts, server/provider replay,
full frontend/browser and Tier 1. No real Codex E2E runs on the shared host.

Real fake-provider browser integration and formal L2 reviews remain explicit
acceptance requirements even when delivered as subsequent integration work.
There is no overall product acceptance until those results exist. New permanent
checks should enforce shared identity and lifecycle contracts, without another
Assistant tool/provider/test-name inventory.
