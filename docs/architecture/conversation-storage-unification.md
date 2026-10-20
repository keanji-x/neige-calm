# Unify conversation storage and replace a same-card carrier

Status: proposed decision, 2026-10-20. Baseline: `origin/main` =
`ed6a15b0d436396a73b60b82aeb5fbe0ac1ec57a`. Documentation only; implementation
requires the decisions in §9. Design review: L2, because the proposed work crosses
persistence, execution identity and credential authority boundaries.

## Decision and acceptance

Retire the historical **execution/input storage contract**, not the user's history.
Every Track Planner and Assistant gets one canonical V2 input carrier at creation,
including empty conversations. Each creation owner composes its existing allocation,
resource admission and Truth acceptance; there is no preparation manager, second
queue, provider capability table or canonical-to-historical error fallback.

Keep #2843's immutable messages, revisions, logical turns, submissions, factual
attempts and presentation order. Replacement is a separately authorized end of an
idle carrier, never recovery of an accepted execution through a replacement worker.
No new author or batch semantics are decided here; those belong to #2671.

Exit: every entry below creates canonical storage; every provider executes and
recovers its original accepted identities; imported history paginates through the
canonical presentation index; no live V1 serializer/parser remains for these cards.
Races, cold restart and lost responses must prove no resend, no authority inheritance
and no loss of original attribution. Workspace repoint restrictions remain.

## 1. Current creation inventory

Paths in tables are relative to `crates/calm-server/src/` unless qualified.
“Historical” describes fresh creation, not replay of an already canonical key.
ACP is a protocol; persisted provider includes `opencode`, not a literal `acp`.

| Entry / function | Provider today | Storage today | Actual owner |
| --- | --- | --- | --- |
| `POST /api/tracks`, `routes/tracks.rs::create_track`, keyed first message, blank managed User Area | Codex | Canonical; `creation_plan::first_message_storage_contract` → `dormant::accept_dormant_human_track` | `routes/tracks/create/{creation_plan,dormant}.rs`; Truth `conversation/creation.rs` |
| Same route, blank without first message, keyed or unkeyed | Codex/Claude/ACP | Historical; `create_track_with_planner_harness` | `routes/tracks.rs`, `routes/tracks/create.rs` |
| Same route, template, recipe or `template_input` | Codex/Claude/ACP | Historical | `routes/tracks/preparation.rs::prepare`, `allocation.rs::allocate_track_structure_tx` |
| Same route, explicit cwd / attach / isolation variants | Codex/Claude/ACP | Historical | `preparation.rs`, folder-claim admission in `tracks.rs`, Workspace owner |
| Same route, `admin`, `fork_report_from`, `allow_cross_area_cwd` | Codex/Claude/ACP | Historical | `tracks.rs`, `allocation.rs`, `area_admin.rs`, `fork_guard.rs` |
| Same route, otherwise blank with first message | Claude/ACP | Historical | `tracks.rs` → historical start adapter |
| `neige_track_add`, `routes/tracks/add.rs::RouteTrackCreator::add` | Creator's provider | Historical keyed mail/start; template/recipe/resource admission | `add.rs::CreatorAdmission`, `create.rs`, `preparation.rs` |
| `daily_planner::reconcile` → `tracks::create_managed_track` | Codex | Historical allocation; no start until first send; System Area | `daily_planner.rs`, `managed_track.rs`, `tracks.rs` |
| `POST /api/tracks/{track_id}/conversations`, `create_track_conversation_inner` | Codex/Claude/ACP | Historical lazy card mint and first-message enqueue in start operation | `routes/track_conversations.rs`, `operation/planner_harness_start_adapter.rs` |
| Side discussion via same Assistant route (`body.side`) | Codex/Claude/ACP | Same historical mint; frozen excerpt, not a transcript fork | `side_conversation.rs`, Assistant route |
| Today summary → `create_track_conversation_inner` | Codex | Same historical mint, server-authored bootstrap | `routes/today_summary.rs` |
| Worker watcher → `create_track_conversation_inner` | Codex | Same historical mint, server-authored bootstrap | `worker_watch/watcher.rs` |

`allocation.rs` uses `initialize_track_tx` for Planner/Report structure, report
initialization and template/recipe/fork provenance. Report cards and dispatched
Worker/PlainChat cards are outside this proposal; do not globally remove their
session or transcript infrastructure. Generic card CRUD is not an alternate
Planner/Assistant admission door; preserve role validation there.

## 2. Historical storage is several contracts

| Persisted fact / format | Writers and readers | Disposition |
| --- | --- | --- |
| `worker_sessions.handle_state_json`, V1 `HarnessSnapshot`: body-bearing `pending_queue`, parallel `pending_envelope_ids`, `pending_entry_meta` (`id,rev,queued_at_ms,attachments`), `pending_message_ids`; projection client id, old issued segments, native ids, watermark | `harness/snapshot.rs`, `harness/queue.rs`, historical run loop; start adapter/input transfer; boot recovery | Retire executable V1 frames after drain or explicit abandonment; archive exact evidence |
| `cards.session_id`; `worker_sessions` identity/contract/provider/state/thread/agent-session/spawn-op fields | Truth `session_mirror.rs`, `session_row.rs`; profile, retained start, recovery | Keep identity ledger; supersede old carriers, never rewrite provenance |
| [Native transcript table](../../crates/calm-truth/src/db/sqlite/transcript_projection.rs): `worker_session_id,card_id,track_id,thread_id,turn_id,item_uuid,item_type,method,params,input_segments` | `db/sqlite/transcript_projection.rs`, `transcript_turn_input.rs`; run-loop content; raw transcript GET | Keep rows; canonical presentation index points to them |
| `planner_input_idempotency`: key, `payload_hash='v1:…'`, worker and nullable entry id | `routes/planner_input_send.rs` and Truth `planner_input_idempotency.rs`; historical send replay | Retire replay semantics; retain permanent key tombstones |
| `track_create_idempotency`: resource ids, fingerprint version and first-message digest | `routes/tracks/create.rs`, allocation, Truth create bindings | Preserve resource identity and used-key barrier; do not manufacture canonical first-input ACK |
| `operations.payload`, `tx_output.initial_input_receipt`, retry `#N` keys; old `AssistantStartWire::V1` and unwrapped Codex payload | start adapter, `conversation_initial_input.rs`, `conversations_shared.rs`, `assistant_start_wire.rs` | Preserve journal evidence; retire historical retry/mint interpretation |
| `events` enqueue observations, `planner_recovery_*`, native correlation/turn evidence | historical run loop, dispatcher, recovery | Keep audit facts, not proof of accepted canonical input or permission to resend |
| `card_mcp_tokens.hashed_token`; `worker_sessions.mcp_token_hash` mirror | `mcp_server/wiring.rs`, Truth `card_composite.rs`, `session_row.rs`; MCP handshake and live identity checks | Revoke both stores on replacement, all providers |
| `assistant_conversation_archives`, metadata/title, side-source payload | archive/title owners and conversation lists | Preserve visibility and source context; not a second transcript |

Canonical does **not** replace every table above. Migration 0175 adds
`conversation_messages`, `conversation_message_revisions`, `conversation_turns`,
`conversation_input_submissions`, `conversation_submission_inputs`,
`conversation_presentations`, `conversation_human_track_creations` and the Area
history contract. V2 `canonical_inputs` in the current worker snapshot remains
the sole pending checkpoint. `planner_input_idempotency` uses `v2:` for canonical
source-bound receipts. Native items already use that same transcript table.
`conversation_read.rs` / `conversation_items.rs` currently negotiate canonical
presentation pages or historical raw rows; neither native echoes nor enqueue
events establish a human author.

## 3. Target admission at the entry owners

Use required typed `PlannerBinding { profile, provider }`, current worker id,
resource identity and an explicit input variant: `Human(HumanInputSource, payload)`,
qualified `System` source, or `Empty`. These are proposed composition inputs, not
a new service. Empty means a required empty V2 checkpoint, not an absent snapshot.
Optional native ids remain genuinely absent until start. Do not default a missing
principal, provider, profile or source to the deployment owner.

* **Track REST:** extend `creation_plan.rs` and `dormant.rs` in place after the
  Claude track's predicate changes. Keep immutable key replay before mutable
  catalog/readiness checks. Allocate empty canonical carriers for message-less
  creation; no key still means independent resource creation. With first input,
  allocation, acceptance, original ACK and pending ref commit together. Availability
  failure after acceptance cannot delete the card or change storage.
* **Template/recipe/input/fork:** `preparation.rs` retains catalog and resource
  qualification; `allocation.rs` reads recipe revision and copies Report facts
  once in its transaction. Template context and forked Report are context, not
  human messages; bind their frozen versions in the existing render-plan owner.
  A report fork never copies the source's authors, queue or execution grants.
* **Attached/cwd/cross-area:** existing folder claim, isolation and Workspace
  admission execute before allocation effects; recheck target/source Area access
  in the authoritative transaction. Materialization/start stay with Workspace and
  the existing start adapter. Persist accepted inputs during provider outage, but
  never pretend an unresolved resource is Ready. Failed preparation cannot select
  V1. Repoint continues to refuse retained accepted work under #2843.
* **Admin:** `area_admin` alone admits admin creation. Commit its grant and the
  canonical allocation together; storage choice grants no extra tool authority.
* **Assistant:** `track_conversations.rs` owns durable card + empty carrier + first
  input + create-key receipt, before wake. Extend Truth's creation receipts for
  this entry; do not force Assistant through the Planner-only trigger in 0175.
  Preserve side-context and opening briefing through the existing context owner.
  `conversation_initial_input.rs` reads original canonical acceptance, not the
  current queue as evidence of the original ACK. Remove compensate-and-reenqueue.
  Reuse `planner_attachments/bind.rs` for required bound-attachment facts before
  canonical acceptance; integrate edit/withdraw/steer and replacement refusal
  with existing canonical owners, never translate a body back into a V1 queue.
* **Internal entries:** `add.rs` keeps CreatorAdmission and cap checks; accepted
  mail must use its owning qualified source. `create_managed_track` commits an
  empty canonical carrier with its managed identity; daily Planner still starts
  on input. Today summary and watcher must provide qualified durable System
  references through their existing event/dispatcher owners. `Actor::server_send`
  alone is not a source. If the present source contract cannot express bootstrap
  input, obtain #2671's decision before switching that entry; do not impersonate
  a Human or introduce a second queue.
* **Providers:** consume `PlannerBinding` and the existing factory/start/recovery
  owners. Codex Assistant must support retained activation with Assistant policy;
  Claude's Planner lifecycle comes from track 85f4c565, then extend its Assistant
  profile. ACP needs actual lazy new/load, input crossing, receipts, Stop, END,
  compact and recovery integration. Remove the explicit canonical refusal only
  with those production paths tested. A capability flag is not implementation.

Until each entry's PR lands it retains its explicit original format. That is
bounded rollout by entry, never a runtime fallback or dual write. Final removal
waits for every row in §1, including internal callers, to exit.

## 4. Data migration and retirement

Choose **lossless factual migration plus explicit execution retirement**, not a
claim of lossless conversion of every historical conversation. Stop ingress and
quiesce all affected provider drivers for the deployment cutover. Back up DB and
owned attachments; inventory counts and hashes on that backup, not production
from this design task. Use NEW numbered migrations (0175 and every released
migration remain byte-frozen). Migration code belongs to Truth's upgrade path;
no runtime adoption or historical parser on a canonical read failure.

| Record class | Classification and concrete treatment |
| --- | --- |
| Existing canonical messages/revisions/turns/submissions/attempts/presentations/create ACKs | Lossless: unchanged ids, authors, hashes, fixed refs, order, source evidence and current carriers; never restamp or “upgrade” author/batch fields |
| Historical native transcript items and input segments | Lossless factual copy/index: retain original transcript row ids and runtime/thread/turn attribution; add explicit `imported_item` presentations in stable per-card item-id order, using Neige-owned row ids as source identities. Preserve user echoes/projections as imported facts, not canonical Human messages or new native observations. Do not deduplicate by equal text |
| Archived/failed/closed cards and side/title/archive metadata | Lossless facts: same card, archive flag, source excerpt and transcript; index through presentations even without a live worker. No wake on read |
| Historical empty/settled current carriers | Retire native continuation: retain original worker row and evidence, revoke tokens, install a new empty V2 carrier on the same card. Preserve known push watermark; do not resume its old native thread |
| Pending V1 user entries, folded bodies and attachment bindings | Not losslessly canonicalizable: full `{id,rev}` may exist, but authenticated author and exact original message versions generally do not. Drain with old binary first, or explicitly abandon with owner-approved export of raw snapshot/attachments and retirement receipt. Never silently drop, remint from text or backfill author |
| Historical active, Unknown/CrossingUnknown/Accepted-but-unclosed execution, unresolved holds/Stop/start/compact/rewind | Block upgrade for that deployment until original lifecycle resolves, or explicit human-approved abandonment after provider quiescence. Archive exact facts and reason; do not mark END or BeforeSend without evidence. No automatic replacement/resend |
| Pending System observations and watermark | Drain before historical retirement; preserve exact qualified observation/event prefix where it exists. Do not carry guessed events or use MAX(events.id) to skip history. If drain is impossible, a separate explicit retirement decision must identify the abandoned prefix; #2671 owns any new source representation |
| V1 input receipts and historical operation retry keys | Retire executable replay; new migration records scope+original key as a typed retired-intent tombstone. Old key returns explicit `410 historical_intent_retired`, never mint/enqueue. Retain exact original journal/receipt in audit export; no V1 hash parser or retry adapter after removal |
| Historical create bindings and initial-input ACKs | Preserve resource ids and used keys; keep resource-only replay when exactly provable, otherwise explicit retired-intent response. Never fabricate message/revision/author. Missing entry identity stays unavailable. Canonical immutable creation ACKs are untouched |
| Old V1 frames, recovery records and enqueue events | Retire execution meaning; retain byte-exact offline audit export with manifest/checksums and original ids. Remove live V1 handle JSON only after export verification. Shared worker/event tables used by other roles remain |
| Credentials, local sessions and caches | Deliberate invalidation, not lossless migration: revoke old card and mirror hashes, seal/close old native handles using provider contracts. Clear registry/source/held-request/role caches; never copy private provider files or guess session paths |

NEW schema adds an `imported_item` presentation source pointing to the existing
raw item row, with an explicit imported-fact display variant and original runtime
attribution. This is a one-time migration path into the sole canonical index, not
a V1 read fallback. Existing `presentation.rs::native_source_identity` correctly
refuses user echoes, projections and missing native ids; do not weaken it to
import them. Read imported rows directly through the canonical page query and
redact attachments as today; do not call the old raw-list route or V1 codecs.
NEW schema may also add a retired-intent table and carrier-boundary receipts.
They store identities and retirement decisions, not a shadow body queue. Audit
exports are evidence, not an application transcript read fallback. Require a
manifest covering every affected card/key/carrier, row and attachment checksums,
index uniqueness/count reconciliation and no unclassified executable snapshot.
Abort atomically on malformed/unclassified records; never “best effort” migrate.
Rollback is restore the pre-upgrade backup with the old binary while ingress is
stopped; binary downgrade after new canonical input is not supported.

## 5. Same-card carrier swap and the #2774 caller

Proposed Truth primitive, in `db/sqlite/conversation/carrier.rs` (split a sibling
`carrier_swap.rs` if necessary):

```rust
replace_idle_carrier_tx(tx, expected: ExpectedCarrier,
                        next: PlannerBinding) -> CarrierReplacement
```

`ExpectedCarrier` requires card, Track, worker id, original snapshot comparison,
`RuntimeExecutionFence` and binding. Result requires old/new worker ids and a
presentation boundary. No nullable profile/provider, inherited token, new queue
manager or replacement of a live accepted turn. The caller owns authorization;
Truth owns identity/CAS and atomic writes, not role policy.

The proposed server orchestration API is `swap_conversation_roles(source, key,
expected_planner, expected_assistant)`; both expected values are required. Its
internal transaction calls the primitive twice: demote the former Planner to
Assistant and promote the selected Assistant to Planner. Same open Track,
distinct cards, exactly one Planner before and after, explicit provider/profile
bindings; keep same-provider policy until a separate product decision permits
provider changes. Each card keeps its id and history and gets a new empty carrier.
Do not publish this two-card API until the #2774 scope decision in §9.

1. Acquire existing card/start and operation lifecycle fences in stable card-id
   order. Authorize the current live Human, Area access and role-change permission;
   resolve provider readiness and resource availability through actual owners.
   Quiesce idle handles using backend proof, not absence from registry. No SQL
   transaction spans provider awaits; any failure before cutover leaves bindings
   and credentials usable. Final transaction repeats all mutable checks.
2. In one transaction load both exact current carriers via #2843
   `current_input_carrier_tx`, validate `PlannerBinding`, worker contract and
   snapshot CAS. Require Idle, no active submission/native turn, no pending Human
   **or System** refs, no unprocessed accepted content, unresolved input crossing,
   provider hold, start/compact/rewind/Stop intent or unknown activity. Completed
   attempt history alone is not busy. Refuse before mutation; never transfer or
   discard pending content. Recheck watermark against accepted System prefix.
3. Retire both old sessions (`superseded`), clear **all** affected
   `worker_sessions.mcp_token_hash` credentials for both cards (including stale
   retired mirrors) and delete both card-level token rows. Do not rely on `session_supersede_active_tx`'s provider-dependent clearing.
   Create idle V2 workers with empty checkpoints and new immutable contracts;
   native ids, spawn claim and token hashes start absent. Preserve consumed event
   watermark on each card, not another card's prefix. CAS card pointers, role and
   provider payload keys together; do not mutate old session contracts.
4. Record immutable idempotent swap receipt (request digest, original and resulting
   ids, roles, boundaries, actor and watermark) in the same transaction. Same key
   and digest returns original result after lost response; changed digest conflicts.
   Competing append/dispatch/replacement wins or loses the same carrier/snapshot
   CAS, never half a role swap. Rollback leaves both old bindings intact.
5. After commit invalidate old harness/source-index/role/connection/held-request
   caches and publish committed facts only. Failed cleanup is recoverable from
   SQL; it cannot restore authority. Every consequential MCP mutation and late
   native pre-send revalidates persisted current session/contract, including calls
   admitted before cutover. On next authorized turn retained activation mints a
   fresh credential pair for the new worker and uses its new role instructions.
   No raw token is persisted or replayed in the swap receipt.

Reuse `save_input_carrier_tx`'s full-snapshot/current-pointer compare and
`RuntimeExecutionFence`; note that the latter intentionally excludes pending refs
and push watermark, so it alone is insufficient for swap. Explicitly compare the
whole snapshot and event prefix too. Retained recovery still cannot replace work.
A stopped/failed current carrier is not automatically eligible just because its
handle is absent. Previously started turns require qualified END; a never-started
empty carrier requires proof that no start/crossing was attempted.

History reads remain card-scoped canonical `conversation_presentations.sequence`
pages across every carrier; store a boundary sequence for optional conversation
windows without deleting earlier pages or invalidating history cursors. Old
turns/submissions/receipts and transcript `worker_session_id` remain bound to old
workers. Commit/gate/plan/approval/task ownership is never reassigned from current
card role. Late old native facts cannot mutate the new carrier; existing current
fact loaders must reject the retired worker, while original durable facts remain.

The current #2774 issue body explicitly excludes Assistant promotion; #2935 asks
for canonical blank-conversation support. The same one-card primitive can support
that narrower new-empty conversation flow. The two-card composition above is the
requested future contract, not a claim that #2774's published v1 approves it.

## 6. Removal and lifecycle compatibility rows

Delete per-entry historical creation branches and start compensation/retry paths
once their production canonical flows land. At final cutover remove V1 queue
serialization/lenient parallel-array codecs, legacy projection-segment reads,
`InputStorageContract::Historical` selection for these roles, historical send/
edit/delete/steer/Stop branches, `historical.rs` start invocation and queue harvest
from `input_transfer.rs`. Remove old Assistant start wire decoding, V1 replay hash
formatting, historical raw cursor/projection routing and related UI outbox branches
only after released-client retirement. Keep shared Worker/PlainChat mechanisms
until their own owners no longer need them; do not delete entire shared tables.
Regenerate OpenAPI/wire artifacts for changed contracts with the real generator.

Replace the lifecycle document's compatibility rows in the final removal PR:

| Row | New lifetime / exit rule |
| --- | --- |
| Historical snapshots and wire contracts | Migration complete; no executable V1 Planner/Assistant data. Offline audit and typed retired-key refusals remain; remove refusal only with key-retention policy, never allow key reuse |
| Historical first-message preparation | Removed; every entry's existing owner admits canonical allocation and real resources |
| Canonical retained activation | Codex/Claude/ACP and Planner/Assistant only after each actual backend lifecycle is integrated; no provider-name exception in generic kernel policy |
| Explicit idle carrier replacement | User-authorized terminal boundary, empty successors, atomic revocation/CAS and immutable original attribution; never recovery or Workspace repoint |
| Negotiated/raw display | Canonical presentation pages only after client compatibility retirement; native historical facts have one presentation index |

Keep private-deployment/live-source and retained Workspace-repoint restriction
rows unchanged. Storage unification does not deliver shared execution or #2818.

## 7. Ordered delivery slices

Each row is a separate PR; estimates include tests/generated deltas and target
≤1,500 changed lines. Split again before exceeding that budget; no giant final
“cleanup” PR. All implementation slices are L2 (two independent review channels):
persistence/authority boundaries remain even when only deleting a branch.
This design-only PR changes one file; Planner arranges design review.
Paths use `S=crates/calm-server/src`, `T=crates/calm-truth/src/db/sqlite`,
`Y=crates/calm-types/src`, `M=crates/calm-truth/migrations`. “Generated API/wire”
means `fe/core/api/generated/{openapi.json,wire.ts}`; include other generator
outputs selected by the actual schema changes, splitting the PR if needed.

| PR / estimate | Depends on | Files touched / ownership | Focused acceptance tests |
| --- | --- | --- | --- |
| U1: imported facts/index, 1,200 | Human approves migration/display policy; #2671 interface agreement | NEW `M/NNNN_conversation_imports.sql`; `T/conversation/{presentation,storage_contract}.rs`, `T/conversation_read.rs`, `Y/conversation_display.rs`; generated API/wire; `fe/core/api/schemas.ts` and existing conversation display consumers | Imported user echo/projection/missing native id; original order, attribution, redaction, page boundaries and erasure; keep `native_source_identity` refusals |
| U2: canonical ACP transport, 1,300 | Delivered #2836 owner interface or coordinate/rebaseline it | `S/harness/backend/{dispatch,execution_end}.rs`, `S/acp_planner/{session,recovery}.rs`, owning ACP protocol modules | Real local ACP peer: start/load, receipt, uncertain crossing, no-resend and qualified END |
| U3: ACP retained/control lifecycle, 1,300 | U2 | `S/operation/planner_harness_start_adapter/retained.rs`, `retained/recovery.rs`, `S/harness/run_loop/canonical/{control,resume}.rs`, ACP compact/Stop owners | Boot/reopen idle original worker, stale Stop, hold/compact failure and END; no replacement |
| U4: canonical Assistant acceptance, 1,400 | U1; Claude slice 5 for shared retained edits | NEW `M/NNNN_assistant_creation_receipts.sql`; `S/routes/{track_conversations,conversation_initial_input}.rs`, `T/conversation/creation.rs`, `S/operation/planner_harness_start_adapter/retained.rs`, `S/harness/profile.rs`, `S/planner_attachments/bind.rs` | Extend `track_conversations_keyed` and `canonical_flow_tests`: real authenticated HTTP accept during outage, lost-response replay, side context, correct Assistant tools; Codex/Claude |
| U5: ACP creation admission, 800 | U3,U4; Claude slice 5 | `S/routes/tracks/create/{creation_plan,dormant}.rs`, `S/routes/track_conversations.rs`, provider creation tests | ACP first/no-first input; Assistant and Planner; outage never selects V1 |
| U6: template/recipe/fork admission, 1,300 | Claude slice 5,U5 | `S/routes/tracks/{preparation,allocation}.rs`, `create/{creation_plan,dormant}.rs`, `S/routes/tracks.rs` | Extend dormant plan/binding and canonical HTTP flow tests: recipe revision, template input, fork Report/context, key replay after roster change |
| U7: attached/cross-area/admin, 1,300 | U6, actual Workspace qualification contract | `S/routes/tracks.rs`, `tracks/{preparation,allocation,fork_guard}.rs`, `S/area_admin.rs`, owning Workspace admission tests | All isolation variants, denied cross-area/admin, materialization failure, deletion race, retained repoint refusal |
| U8: internal entry admission, 1,400 | U6,U7; #2671 decision if bootstrap source is missing | `S/routes/tracks/add.rs`, `S/routes/tracks.rs::create_managed_track`, `S/daily_planner.rs`, `S/routes/today_summary.rs`, `S/worker_watch/watcher.rs`, `S/dispatcher/canonical_inputs.rs` | Creator cap race, mail replay, daily no-start creation, qualified summary/watcher bootstrap and watermark cold recovery |
| U9: idle replacement and revocation, 1,400 | U3,U4; no pending source semantics changes | NEW `M/NNNN_carrier_replacements.sql`; `T/conversation/carrier_swap.rs`, `carrier.rs`, `T/session_mirror.rs`, `T/card_composite.rs`, `S/mcp_server/{handshake,wiring,transport}.rs` and owning tool transaction authorization | Extend `session_conversation_tests` and canonical flows: full-snapshot CAS, pending append race, both token stores, stale admitted MCP, rollback, next-turn fresh credential |
| U10: atomic role-swap composition, 1,300 | U9; human approves two-card scope | NEW `S/conversation_inputs/carrier_swap.rs` role-change composition, `S/harness/profile.rs`, `S/operation/planner_start_fence.rs`, `crates/calm-truth/src/card_role_cache.rs`; #2774 caller route and generated contract in its delivery | Two swaps in one transaction, one Planner invariant, forbidden source, active/System pending refusal, lost reply and cold recovery, original task/approval attribution |
| U11: deployment retirement upgrade, 1,400 | U1–U9; approved abandonment/key policy | NEW `M/NNNN_retired_conversation_intents.sql`, `crates/calm-truth/src/lib.rs`, `T/infra.rs`, NEW `scripts/conversation-storage-retirement.py` offline inventory/export command, `T/planner_input_idempotency.rs`, `T/conversation/creation.rs` | Mixed-format backup upgrade, exact manifests, malformed snapshot abort, old-key 410/no resend, canonical untouched, restore rehearsal |
| U12: delete historical input/control, 1,400 | U11; Claude slice 6 | `S/conversation_inputs/{historical,storage_contract}.rs`, `S/routes/planner_input*.rs`, `S/harness/{queue,snapshot}.rs`, canonical/historical loop branches and tests | Canonical send/edit/withdraw/steer/Stop flows; Worker/PlainChat regression; no executable V1 Planner/Assistant frame |
| U13: delete historical creation/activation, 1,300 | U12; Claude slice 6 | `S/routes/tracks/create{.rs,/delivery.rs}`, `S/operation/planner_harness_start_adapter{.rs,/historical.rs,/input_transfer.rs}`, historical Assistant wire; related recovery paths | Every §1 production entry; original key barriers; retained original-worker cold recovery |
| U14: retire raw read/wire and update lifecycle, 1,200 | U13; approved released-client window | `S/routes/{conversation_items,conversation_read,conversation_initial_input}.rs`, `Y/conversation_display.rs`, generated API/wire, `fe/core/api/schemas.ts`, `fe/core/domain/conversation-display.ts`, `fe/web/src/app/router/conversation-read-data.ts`, `fe/web/src/app/conversations/history.ts`, `docs/architecture/area-conversation-lifecycle.md` | Existing `canonical_presentation`, browser canonical pagination/outbox and credential-free Tier1 integrated creation/history |

Test names above name existing targets/modules or describe tests to add, not
unverified runnable filters. Resolve exact filters before declaring each gate;
reject empty selections. Worker runs focused production-entry tests and formatter
fixes; kernel owns text/contract/quick gates and one focused replay. CI owns broad
suites. Mutation-verify only load-bearing no-resend, CAS and credential fences in
an exclusive worktree with predicted complete red sets. Browser-visible contracts
need preview/browser and Tier1 E2E; never real Codex E2E on this host.
No permanent checklist of entry/test names is needed in generic kernel code:
entry owners and typed constructors enforce admission, existing registries expose
variants. Later provider additions change their owner and tests, not a new manager.

## 8. Adjacent ownership and ordering

Claude track `85f4c565` owns blank managed Track Planner Claude local start,
Stop/END, retained activation/recovery, canonical compact, opening blank creation
to Claude and no-first-message creation, and deleting blank-Claude legacy create.
Per Planner message `3f4b86ee5b4d4f3e87e52a6e9d727242`, its slice 5 extends
`first_message_storage_contract` / `canonical_creation_supported` in place, with
no capability table; it does not touch card tokens/session mirror. Our predicate
edits land **after its slice 5**; final removal **after its slice 6**.
Shared files: `routes/tracks/create/{creation_plan,dormant}.rs`, `routes/tracks.rs`,
`operation/planner_harness_start_adapter/retained.rs`, `retained/recovery.rs`,
`harness/run_loop/canonical/{control,resume}.rs`, backend dispatch/END, compact
owners, canonical flow tests. Serialize overlapping PRs, preserve its lifecycle
and re-run changed-path checks after integration. U9 owns both credential stores.

#2671 C3 owns message authorship, batch identity and shared authorization; preserve
all current immutable fields/checkpoints. Per Planner message
`6c66419625434406a3bfb64b5839edd6`, C3 has no in-flight implementation; the other
track currently works on C5 listing. Overlap: migrations, Truth `conversation/*`,
`conversation_read.rs`, types, source admission and dispatcher. Coordinate schema
numbers and contracts before U1/U8/U11; no source/authorship expansion hidden here.
#2836 owns neutral runtime contracts, not product queue/retirement policy; serialize
ACP changes with its ACP migration and rebaseline behavior matrices.

Read #2774, #2935, #2843, #2671 and #2836 via read-only `gh issue view`.
`docs/architecture/planner-canonical-new-conversation.md` is absent.
The supplied #2774 cross-track artifact and #2671 constraint-table report were
attempted through this track's `neige track cat` view; both returned “path not
available in this view” (-32602). Cross-track reads are prohibited by this worker's
contract. No inaccessible design or table is treated as evidence or authority.

## 9. Human decisions before implementation

1. Approve the recommended drain-or-explicit-retire policy: historical native
   continuity and unprovable pending author/batch identity cannot be migrated
   losslessly. Who approves abandonment, and how long are audit exports retained?
2. Approve retired V1 keys returning 410 rather than replaying old input ACKs;
   choose released-client retirement window and operator cutover/backup window.
3. Resolve #2774's current blank-only scope versus the requested two-card promotion.
   Should U10 ship, or should #2935 first consume only one-card replacement?
   Provider changes remain refused until explicitly approved.
4. Supply the #2671 table captioned `跨Track canonical存储迁移` (near `b_65f8` in
   track `f9c388d18bfe42e6959036b740e6bf7c`) through an accessible handoff. Its
   rows could not be read, so row-by-row satisfaction is unresolved and dependent
   implementation must wait. Confirm imported-fact display/provenance boundaries,
   internal bootstrap System sources, and any required author/batch migration
   contract; this design does not decide them on #2671's behalf.
5. Supply the inaccessible #2774 role-change design's sessions/credentials/caches
   inventory; reconcile it against §5 before implementation. Confirm readiness/
   quiescence guarantees, especially ACP driver join, with the provider owners.
6. Confirm audit/index migration on an actual stopped deployment backup. The
   supplied production counts are planning premises, not a verified empty-data
   exemption; even zero canonical rows do not make old history disposable.

## Appendix: verification of supplied background claims

Line evidence is pinned to the baseline above; `S`/`T`/`M` expand as in §7.

| Supplied claim | Verdict and evidence |
| --- | --- |
| Only blank managed keyed Codex Track Planner creation with first message, User Area, no template/cwd/attach/admin/fork/cross-area is canonical | **Confirmed for fresh creation.** `S/routes/tracks.rs:748` gates on first message; `S/routes/tracks/create/creation_plan.rs:18` requires key, `:23` requires text, `:67` requires User Area, `:81` enumerates Codex and every excluded field including recipe/input. `S/routes/tracks/create/dormant.rs:71` rejects System target. Qualification: `creation_plan.rs:41` and `:47` replay original persisted storage before fresh capability checks; `:63` preserves prior historical start. It is not a universal classifier for arbitrary restored/imported data |
| All other listed fresh cards are historical: Claude/ACP, all Assistants, template/recipe/input, attached/cwd, admin/fork/cross-area | **Confirmed for checkout creation paths.** Predicate exclusions above; historical fallthrough `S/routes/tracks.rs:797`, preparation `:950`, mint/start selection `:969`. Assistant mint-on-first-message `S/routes/track_conversations.rs:152`, first message `:247`, lazy Assistant seed `:230`; actual canonical retained restriction `S/operation/planner_harness_start_adapter/retained.rs:85`; backend refusal `S/harness/backend/dispatch.rs:57`. Internal examples `S/routes/tracks/add.rs:154`, `S/routes/tracks.rs:1458`, `S/worker_watch/watcher.rs:62`, `S/routes/today_summary.rs:196`. “All other cards” is **refuted if read as a runtime format inference**: `T/conversation/storage_contract.rs:9` checks persistent evidence independent of current provider/liveness; never downgrade known canonical data |
| Production at deployed 1846fc277: 161 Planner/Assistant cards, none canonical, zero message/turn rows, all input receipt hashes v1 | **Confirmed only as a user-reported premise; not independently confirmed or refuted as database facts.** No production access performed. No checkout file:line can establish deployment SHA or row counts. Relevant schema/meaning only: `M/0175_conversation_inputs.sql:13` messages, `:63` turns; `T/conversation/storage_contract.rs:9` shows that empty message/turn tables alone do not prove no canonical cards; `T/planner_input_idempotency.rs:13` defines v2, `S/routes/planner_input_send.rs:206` writes v1. Counts require a human's deployment evidence, never fabricated source evidence |

The lifecycle's binding exit policy is `docs/architecture/area-conversation-lifecycle.md:96`
(explicit migration/retirement), `:97` (entry owner, no preparation manager), `:98`
(real backend lifecycle), and `:103` (unknown history stays unknown). These rules,
not the production-count premise, determine the design.
