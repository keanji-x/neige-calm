# Unify conversation storage and replace a same-card carrier

Status: proposed decision, 2026-10-20. Baseline: `origin/main` = `ed6a15b0d436396a73b60b82aeb5fbe0ac1ec57a`.
Documentation only; implementation requires the remaining owner decisions in §9. Design review: L2, because
the proposed work crosses persistence, execution identity and credential authority boundaries.

## Decision and acceptance

Retire the historical **execution/input storage contract**, not the user's history. Every Track Planner and
Assistant gets one canonical V2 input carrier at creation, including empty conversations. Each creation owner
composes its existing allocation, resource admission and Truth acceptance; there is no preparation manager,
second queue, provider capability table or canonical-to-historical error fallback.

Keep #2843's immutable messages, revisions, logical turns, submissions, factual attempts and presentation
order. Replacement is a separately authorized end of an idle carrier, never recovery of an accepted execution
through a replacement worker. No new author or batch semantics are decided here; those belong to #2671.

Exit: every entry below creates canonical storage; every provider executes and recovers its original accepted
identities; imported history paginates through the canonical presentation index; no live V1 serializer/parser
remains for these cards. Races, cold restart and lost responses must prove no resend, no authority inheritance
and no loss of original attribution. Workspace repoint restrictions remain.

## 1. Current creation inventory

Paths in tables are relative to `crates/calm-server/src/` unless qualified. “Historical” describes fresh
creation, not replay of an already canonical key. ACP is a protocol; persisted provider includes `opencode`,
not a literal `acp`. Hereafter `S=crates/calm-server/src`, `T=crates/calm-truth/src/db/sqlite`,
`Y=crates/calm-types/src`, `M=crates/calm-truth/migrations`.

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
| Child-track operation → `initialize_track_tx(Subtrack)`; scheduler bootstrap | Parent provider | Historical | `operation/child_track_adapter.rs:214,253`, `track_creation.rs:83`, `scheduler/mod.rs:1232,1255` |
| Today launchpad ensure / legacy adoption | Configured Planner provider | Historical | `routes/today.rs:264,323,342,530` |
| Today summary → `create_track_conversation_inner` | Codex | Same historical mint, server-authored bootstrap | `routes/today_summary.rs` |
| Worker watcher → `create_track_conversation_inner` | Codex | Same historical mint, server-authored bootstrap | `worker_watch/watcher.rs` |

`allocation.rs` uses `initialize_track_tx` for Planner/Report structure, report initialization and
template/recipe/fork provenance. Report cards and dispatched Worker/PlainChat cards are outside this proposal;
do not globally remove their session or transcript infrastructure. Generic card CRUD is not an alternate
Planner/Assistant admission door; preserve role validation there.

## 2. Historical storage is several contracts

| Persisted fact / format | Writers and readers | Disposition |
| --- | --- | --- |
| `worker_sessions.handle_state_json`, V1 `HarnessSnapshot`: body-bearing `pending_queue`, parallel `pending_envelope_ids`, `pending_entry_meta` (`id,rev,queued_at_ms,attachments`), `pending_message_ids`; projection client id, old issued segments, native ids, watermark | `harness/snapshot.rs`, `harness/queue.rs`, historical run loop; start adapter/input transfer; boot recovery | Map proven settled frames in place; otherwise drain or approved abandonment; export exact evidence |
| `cards.session_id`; `worker_sessions` identity/contract/provider/state/thread/agent-session/spawn-op fields | Truth `session_mirror.rs`, `session_row.rs`; profile, retained start, recovery | Keep settled current identity in place; terminalize only explicitly abandoned or rotated carriers |
| [Native transcript table](../../crates/calm-truth/src/db/sqlite/transcript_projection.rs): `worker_session_id,card_id,track_id,thread_id,turn_id,item_uuid,item_type,method,params,input_segments` | `db/sqlite/transcript_projection.rs`, `transcript_turn_input.rs`; run-loop content; raw transcript GET | Keep rows; canonical presentation index points to them |
| `planner_input_idempotency`: key, `payload_hash='v1:…'`, worker and nullable entry id | `routes/planner_input_send.rs` and Truth `planner_input_idempotency.rs`; historical send replay | Retain original worker/null entry ACK and card-lifetime unique key; factual replay only |
| `track_create_idempotency`: resource ids, fingerprint version and first-message digest | `routes/tracks/create.rs`, allocation, Truth create bindings | Preserve resource identity and used-key barrier; do not manufacture canonical first-input ACK |
| `operations.payload`, `tx_output.initial_input_receipt`, retry `#N` keys; old `AssistantStartWire::V1` and unwrapped Codex payload | start adapter, `conversation_initial_input.rs`, `conversations_shared.rs`, `assistant_start_wire.rs` | Preserve journal evidence; retire historical retry/mint interpretation |
| `events` enqueue observations, native correlation/turn evidence | historical run loop, dispatcher, recovery | Preserve qualified prefix and original facts; no resend authority |
| `acp_managed_sessions`, `acp_submissions` | `M/0157:3,7`; `S/acp_planner/recovery.rs:16,26,108`; `T/acp_submission.rs:65,79` | Preserve registration digest, native/worker/client/turn ids, input/outcome JSON and state; Sending/Unknown blocks cutover |
| `tracks.root_session_id`, historical parent/requester links | `T/session_mirror.rs:309`, `T/session_row.rs:145` | In-place migration preserves links; role composition repoints root in its transaction |
| Older-backup `planner_recovery_*` | `M/0127_drop_planner_recovery_bindings.sql:3` already drops these tables | Classify/export only when present before ordinary schema upgrade; not current live stores |
| `card_mcp_tokens.hashed_token`; `worker_sessions.mcp_token_hash` mirror | `mcp_server/wiring.rs`, Truth `card_composite.rs`, `session_row.rs`; MCP handshake and live identity checks | Revoke both stores on replacement, all providers |
| `assistant_conversation_archives`, metadata/title, side-source payload | archive/title owners and conversation lists | Preserve visibility and source context; not a second transcript |

V2 `canonical_inputs` is the sole pending checkpoint; 0175 canonical messages/revisions,
turns/submissions/attempts and presentations retain their identities. Native rows already share the transcript
table. Neither native echoes nor enqueue events establish Human authorship.

## 3. Target admission at the entry owners

Use required typed `PlannerBinding { profile, provider }`, current worker id, resource identity and an
explicit input variant: `Human(HumanInputSource, payload)`, qualified `System` source, or `Empty`. These are
proposed composition inputs, not a new service. Empty means a required empty V2 checkpoint, not an absent
snapshot. Optional native ids remain genuinely absent until start. Do not default a missing principal,
provider, profile or source to the deployment owner.

* **Track REST:** extend `creation_plan.rs` and `dormant.rs` in place after the Claude track's
  predicate changes. Keep immutable key replay before mutable catalog/readiness checks. Allocate
  empty canonical carriers for message-less creation; no key still means independent resource
  creation. With first input, allocation, acceptance, original ACK and pending ref commit together.
  Availability failure after acceptance cannot delete the card or change storage.
* **Template/recipe/input/fork:** `preparation.rs` retains catalog and resource qualification;
  `allocation.rs` reads recipe revision and copies Report facts once in its transaction. Template
  context and forked Report are context, not human messages; bind their frozen versions in the
  existing render-plan owner. A report fork never copies the source's authors, queue or execution
  grants.
* **Attached/cwd/cross-area:** existing folder claim, isolation and Workspace admission execute
  before allocation effects; recheck target/source Area access in the authoritative transaction.
  Materialization/start stay with Workspace and the existing start adapter. Persist accepted inputs
  during provider outage, but never pretend an unresolved resource is Ready. Failed preparation
  cannot select V1. Repoint continues to refuse retained accepted work under #2843.
* **Admin:** `area_admin` alone admits admin creation. Commit its grant and the canonical allocation
  together; storage choice grants no extra tool authority.
* **Assistant:** `track_conversations.rs` owns durable card + empty carrier + first input +
  create-key receipt, before wake. Extend Truth's creation receipts for this entry; do not force
  Assistant through the Planner-only trigger in 0175. Preserve side-context and opening briefing
  through the existing context owner. `conversation_initial_input.rs` reads original canonical
  acceptance, not the current queue as evidence of the original ACK. Remove
  compensate-and-reenqueue. Reuse `planner_attachments/bind.rs` for required bound-attachment facts
  before canonical acceptance; integrate edit/withdraw/steer and replacement refusal with existing
  canonical owners, never translate a body back into a V1 queue.
* **Internal entries:** `add.rs` keeps CreatorAdmission and cap checks. Human input uses
  `Human { message, author: FrozenHumanAttribution { registered_user_id, display_name } }`;
  agent-written creator first input uses `System { observation_id, payload }`. The mail owner
  binds full first text to a durable mail row and wake in the allocation transaction. Ordinary
  mail and creator share its transaction helper; ordinary mail retains its summary wake. Existing
  Dispatcher qualification accepts it, with no resend or re-accept on replay. Origin kinds
  remain closed; registered Human identities are open. `create_managed_track` commits an empty
  canonical carrier with its managed identity; daily Planner still starts on real input, with
  no bootstrap or historical-date backfill. Today summary and watcher use qualified durable
  System references through their existing event/dispatcher owners. `Actor::server_send` alone
  is not a source; do not impersonate a Human or introduce a second queue. This source decision
  is settled by the parent; #2671 is deferred until this migration is complete.
* **Child/Today:** child allocation keeps inherited provider, parent Area and Workspace
  qualification (`child_track_adapter.rs:214,237,253`); scheduler bootstrap keeps `CardStartFence`
  (`scheduler/mod.rs:1255`) and needs a qualified System source, not a fabricated Human. Today
  launchpad allocates an empty canonical carrier inside `today_launchpad_ensure_tx`; ensure/start
  remains under its existing fence (`today.rs:530`). Legacy adoption must stop deleting transcript
  rows (`:325`), preserve imported history and qualify retained Workspace rebind or refuse before
  mutation; adopting a title/purpose is not permission to abandon native context.
* **Providers:** consume `PlannerBinding` and the existing factory/start/recovery owners. Codex
  Assistant must support retained activation with Assistant policy; Claude's Planner lifecycle comes
  from track 85f4c565, then extend its Assistant profile. ACP needs actual lazy new/load, input
  crossing, receipts, Stop, END, compact and recovery integration. Remove the explicit canonical
  refusal only with those production paths tested. A capability flag is not implementation.

Until each entry's PR lands it retains its explicit original format. That is bounded rollout by entry, never a
runtime fallback or dual write. Final removal waits for every row in §1, including internal callers, to exit.

## 4. Stopped upgrade: preserve proven context

D1 is distinct from explicit rotation (§5): settled historical carriers migrate **in place**, with the same
worker id, provider, profile/contract, native thread and agent-session ids, runtime header and push watermark,
plus an empty V2 checkpoint. `Y/harness/snapshot.rs:14,35,58` supports this shape;
`T/conversation/carrier.rs:105,121` requires exact current identity and strict V2, not historical canonical
authors. `S/operation/planner_harness_start_adapter/retained.rs:90` checks native association;
`retained/recovery.rs:43` permits idle recovery. Codex exact-thread recovery exists
(`S/harness/backend/factory/recovery.rs:111,166,188`); Claude resume (`S/claude_planner/session.rs:200`) and
ACP load (`S/acp_planner/session.rs:302`) still need qualified canonical host integration. Missing authors
forbid reminting old input; they do not require destroying settled native context.

The migration qualifier requires an exact current resumable row/pointer and matching binding/header/native
ids; empty V1 queue and all parallel metadata; idle/qualified completed phase; no active turn, start/recovery
reservation, crossing/Accepted-but-unclosed execution, hold/native question/approval, compact, rewind, Stop, wedge or unknown activity in snapshots, operation
journals, live/native ownership and ACP journals. Preserve completed attempt facts. Provider recovery must
prove the original association, credentials or qualified cold rebind, idle state and load capability (ACP also
registration digest). An absent registry handle is no proof. Strictly map each runtime header field and remove
V1-only fields; no lenient defaults. Only records that fail this proof block the deployment for old-binary
drain or explicit human-approved abandonment after quiescence. Never clear pending to pass.

Historical native rows/segments remain factual imports: original row, worker, thread/turn attribution,
payload, attachments and explicit Unknown/legacy origin; never authenticated Human messages inferred from
viewer, text or folded entries. Archived/closed/failed cards keep title, archive, side context and history
without waking. Existing canonical identities/revisions/refs/plans/attempts stay unchanged. Pending folded V1
bodies cannot prove original authors/versions: export exact snapshot, attachments and disposition on approved
abandonment, not a fake message. Pending System work drains through its source owner; never skip it with
MAX(id).

### One index, receipts and crash protocol

Add `imported_item` referencing existing raw rows to the existing single `conversation_presentations` query
(`T/conversation_read.rs:70,96`), with explicit imported display provenance. Keep `native_source_identity`
exclusions (`T/conversation/presentation.rs:100`). No second body table or raw-route fallback. U2 installs
schema/read support only: population waits for atomic cutover, because any presentation already makes a card
canonical (`T/conversation/storage_contract.rs:9`). Exclude rows already indexed by native row identity, never
text equality. On historical-only cards assign stable item-row order; on mixed cards retain every existing
sequence/cursor and append missing imports in stable row order with an explicit import boundary (do not claim
reconstructed interleaving). Approval of that mixed-history presentation is §9. Erasure stays identity-based,
preserving digest/provenance and attachment redaction through the existing owner.

| Namespace | Compatibility after migration/rotation |
| --- | --- |
| Send: `planner_input_idempotency` | Retain key, versioned request hash, original worker and nullable entry. Same digest/current authorized access returns original ACK, including null; changed digest conflicts. Never look at current worker to reconstruct it |
| Track create: `track_create_idempotency` and canonical creation ACK | Retain original resource/fingerprint/first-input facts; replay original result before mutable readiness/roster checks; never allocate/start on replay |
| Assistant create / operation retry `#N` / initial-input receipt | Owning creation/operation reader preserves provable original result from committed journal. Missing original result returns typed `historical_result_unprovable` with namespace/reason; key stays consumed, never retry native execution |

`M/0139:4,7` and `T/planner_input_idempotency.rs:17,32` already store null ACKs;
`S/routes/planner_input_send.rs:365,397` replays them without enqueue. Move factual replay ahead of
storage-format routing; retain versioned digest verification (`:197,206`) as receipt compatibility, not a V1
execution decoder. No blanket 410 or new retired-intent table. Existing send-key retention is card lifetime
(0139:9); create keys keep their owning retention contract. “Permanent” forbids reclamation while the
namespace survives; any stronger post-deletion retention needs #2671's explicit decision. No new table without
evidence of an uncovered namespace.

1. Stop all ingress, scheduler, recovery/reaper and provider writers; obtain owner quiescence
   proofs. Back up DB plus owned attachments. Export byte-exact V1 frames, operations, receipts,
   transcript and ACP journal/configuration facts to an fsynced manifest with deployment/schema
   identity, row/attachment hashes, counts, each card/key disposition and approved abandonment ids.
   Missing or malformed/unclassified records abort before destructive writes.
2. Truth's explicit offline upgrade applies new numbered schema migrations only (0083, 0175 and all
   released bytes stay frozen). A durable upgrade-required marker/version is installed before data
   population. New binary checks it **before** cache seed, recovery, dispatcher, native
   subscriptions or HTTP/MCP admission; absent/mismatched completion for a deployment requiring
   conversion also blocks boot. Incomplete application never selects historical fallback.
3. Under one deployment-wide BEGIN IMMEDIATE transaction, verify manifest against current rows and
   qualified provider evidence; map all eligible frames in place, index history, retain receipt
   barriers, remove executable V1 state only after export verification, and apply approved
   abandonment/revocation explicitly. Preserve settled credentials needed by qualified recovery; do
   not blanket revoke settled context. Commit reconciliation counts/hashes and completion marker in
   this same transaction; no externally visible half-indexed card.
4. On restart before apply commit, marker blocks boot; repeat against unchanged manifest or restore
   backup while stopped. A crash during apply rolls back all data/completion writes. After commit,
   boot verifies completion/manifest version, rebuilds caches from SQL and resumes only qualified
   original contexts. A crash before cache publication repeats this step. Release ingress only after
   that barrier. Schema-only installation must remain compatible with old data until this explicit
   upgrade; never automatically start a partially prepared binary.

Backup restore with the old binary is rollback only while stopped and before any new canonical input. After
acceptance there is no binary downgrade or compensating resend. Test crashes at every durable boundary, lost
upgrade response, malformed record rollback, mixed formats, exact native continuation and restore rehearsal.

## 5. Early one-card rotation primitive; #2774 owns composition

```rust
replace_idle_carrier_tx(tx, expected: ExpectedCarrier,
                        next: PlannerBinding) -> CarrierReplacement
```

Caller supplies the SQL transaction and already-held lifecycle guards. Expected requires card/Track/current
worker, original full serialized snapshot, execution fence, role/provider binding and accepted event prefix.
Expose a required caller revision-CAS input/hook when the caller has a role revision; Truth validates it in
that transaction using the caller-owned revision contract. Do not define a role-revision schema/policy here.
Result contains old/new worker, old/new binding, per-card watermark, history boundary and required cache/root
publication facts. No internal lock acquisition, commit, provider await or role-swap orchestrator. A caller
can invoke it twice before committing; all writes roll back together.

* Caller authorizes through the existing source/resource owners and validates new binding/readiness.
  The primitive accepts **any valid new role/provider**; #2774's same-provider policy is its own. No
  inherited native context or tokens.
* Refuse noncurrent/malformed/historical/unavailable carrier, changed binding or full
  snapshot/prefix, pending Human/System/steers, active execution or unresolved crossing/Unknown,
  open hold/question/approval, start/compact/rewind/Stop or recovery reservation. Native owner must
  prove idle and safe detachment; absent handles, failed/stopped state or a completed attempt alone
  do not prove it. Caller supplies protected owner proofs for non-SQL predicates; durable ones are
  rechecked before writes. Do not transfer work or manufacture END.
* In the supplied transaction supersede the old worker, revoke the card token and **all**
  card-associated session mirror hashes (including stale retired mirrors), create an idle canonical worker (runtime frame V5)
  with new immutable binding/empty checkpoint, absent native ids/spawn claim/token, and CAS
  pointer/role/provider payload. Preserve this card's watermark and original history. Reuse
  carrier/save rules (`T/conversation/carrier.rs:105,147`); execution fence alone omits pending and
  watermark (`T/conversation/runtime_frame.rs:23`). Require exact affected counts.
* Do not implicitly repoint Track root while demoting/promoting: expose explicit link facts/options
  so #2774 sets root to the promoted fresh worker in the same transaction
  (`T/session_mirror.rs:309`, `T/session_row.rs:145`). Preserve old parent/requester links. Ordinary
  same-role Planner replacement must likewise leave root consistent at commit. Never mutate old
  execution contracts/facts.
* Caller keeps admission fenced through postcommit publication and invalidation of old
  live/dormant/reserved slots, source indexes, live reply writers, native subscriptions,
  thread-card/role mappings, connections/held requests and role snapshots. Cancel reservations by
  identity; existing shutdown only removes Dormant on a handle miss (`S/harness/registry.rs:181`).
  Live replies are separate (`S/harness/live_replies.rs:18`); native mapping/unsubscribe are
  separate (`S/shared_codex_appserver.rs:1268,1559`). No new cache manager.
* U1a class A: card/conversation-scoped SQL writes made on behalf of a worker session
  (events, report/task/plan writes, asks, input/receipt writes, native pre-send and native
  registration facts) resolve the calling worker as current inside the owning SQL transaction:
  active session, card link and contract must agree. Refuse a retired worker even if handshake
  or per-call admission preceded retirement (`S/mcp_server/handshake.rs:66`, `transport.rs:1651`).
  Extend the existing transaction session resolution; admission alone is an early refusal.
* Class B: deployment- or track-level operations carrying no per-worker data, or operations
  outside an SQL transaction (including MCP maintenance VACUUM and pure external handler effects),
  retain existing handshake/per-call admission. U1a changes none of these operations. U1b must
  refuse rotation while the carrier has an active native turn **or an in-flight MCP tool call**,
  using the native owner and the new per-worker `ToolCallAdmission` owner in MCP transport,
  including native issuance before active-turn publication. Both registered kernel and plugin
  calls retain admission until completion/cancellation. A rotation fence requires zero in-flight calls at acquisition and recheck, and refuses new calls; the entry is removed after the
  last call/fence drops, rather than living for the AppContext lifetime.
  An idle session is not proof: MCP admission allows idle sessions and ACP registration precedes
  publication of its active turn (`T/session_row.rs:161`, `S/acp_planner/session.rs:261,412`).
* Class C: work durably admitted before retirement (forge action payloads, recorded approvals/asks,
  dev publish, task declarations and receipts) keeps exactly its existing attribution: card, Track,
  KernelDispatcher and worker where present. Do not rewrite, backfill or re-attribute it, add a
  worker field, or re-authorize its later execution against the current carrier. The card persists,
  so its commits/plans/receipts keep their card attribution. U1a changes none of these semantics.
* Cold boot rejects retired rows and seeds committed roles. Fresh credentials appear only on
  next authorized retained activation; receipts never contain raw tokens.

U1b-pre supplies owner-local prerequisites, not replacement, revocation or publication.
Rotation **parks issuance**, never ingress. Under each owner's existing issuance lock,
verify its idle predicate and install an identity-bearing park token, then release the
lock. Acquisition and recheck use the same predicate, including source `is_current`
(reader/stream liveness) and source generation. Harness ticks and explicit issuance,
Codex start/steer/control, ACP/Claude issue entries and MCP admission refuse or skip a
parked owner. A parked canonical tick returns before opening SQL. No quiescence owner
retains a lock guard through the caller's commit/publication window; dropping a token
clears only that token's park, never a newer one. Parks are independent per card; #2774
can park two cards without adding a lock order.

Issuance entries check the park, or enter MCP admission, **before awaiting a
resource rotation may hold**: card/drive/Track lifecycle locks, dispatcher push
admission or the SQL writer. An entry may first await its own brief issuance/issue
mutex (the mutex under which its park is installed), or make a provisional identity
discovery read. Discovery grants no authority: MCP resolves identity again under
`ToolCallAdmission`, requires the same worker and rejects retired/non-current sessions
on both registered and plugin dispatch. Harness Track admission checks the park
before repository/Track waits, releases any brief issuance guard before taking Track,
and retains the authoritative park/current-carrier checks under issuance afterwards.
Never invert the existing Track-before-issuance lock order to make an early check.

Human Retry is content ingress, so its authenticated resolution transaction may enqueue an
explicit successor while parked; it neither issues nor transfers execution. Resolution and
repeat-source admission resolve the exact transaction-current carrier; receipt replay keeps
its original attribution even after supersession. Successor claiming/sending uses the ordinary
parked canonical tick and issuance path. Codex composes the park refusal into the same native
source/activity/ticket predicate run after each asynchronous admission and immediately before
send. ACP and Claude retain their issuance mutex across admission/send, so that same owner
prevents a park from appearing between its early refusal and final admission. Do not add a
second retry-specific busy checker.

Notifications, native items, completion, status, questions and held approvals keep flowing
through the real serial run loop and shared broadcast while parked. There is no in-memory
hold, poison, exclusion queue or replay substitute. Ingress progress means parking itself
adds no stall: with no writer transaction open, sibling and leased-thread notifications
and held approvals are consumed and persisted without `input_lost`. Any held SQL writer
can already stall that serial consumer and overflow the broadcast; that separate defect
is tracked as “Harness can lose native notifications while the SQL writer is held”. U1b's
rotation transaction must therefore be **short and SQL-only**, with no provider/network
await. The park window is not permission to hold a writer during readiness or publication.

Acquire and asynchronously recheck `IdleCarrierOwner` **before opening the caller's writer
transaction**: canonical lifecycle checking opens its own `BEGIN IMMEDIATE`. Recheck durable
predicates inside the caller's transaction. No asynchronous owner recheck runs inside it.
Keep tokens through commit/publication, then invalidate old live/dormant/reserved slots and
native subscriptions. Nonpersisted live broadcasts from retired sources are acceptable only
when postcommit publication invalidation drops their old live writers and mappings.

Ingress writes obey a type rule (S = server, T = Truth; paths relative to `src/`):

| Class / boundary | Required transaction authority | Durable rotation comparison |
| --- | --- | --- |
| A: live native transcript, terminal outcome and canonical native presentation | Truth's raw insertion requires `CurrentCarrier<'tx>`. Only the transaction-current native carrier resolver constructs it. It exclusively borrows that transaction's connection and carries the resolved worker/card/Track, so an unfenced caller cannot supply a pool, another transaction or another identity. The repo-level pool insertion API is removed. Historical native dispatch, including `turn/plan/updated`, obeys the same rule as canonical dispatch. | Raw contents and presentation identities/content, including same-sequence updates and terminal rows |
| A: runtime metadata, input receipts, END and attributed events/asks | Existing checkpoint/frame resolver or actor-authorized event transaction remains mandatory. These contracts also protect execution/checkpoint and source authorization; the native transcript proof does not replace them. | Full checkpoint/snapshot, open asks, native binding/registration and relevant event prefix |
| C: receipt reconstruction | The separately named `transcript_receipt_restore` reconciles receipt-owned identities in original order. Its internal reconstruction insert is private to the projection owner; no raw reconstruction insertion is exposed to live callers. Test seeds use a fixtures-only seam. | Original admitted identity and body, including restored rows |
| C: admitted input projection upgrade | The explicitly named `transcript_admitted_projection_upgrade` updates only an existing pending projection. It cannot create a native row when no admitted projection exists. | Original projection identity and updated contents |
| C: publication of committed facts | Phase/item events publish the original committed snapshot/raw row and worker identity; they admit no new native fact. | Original attributed fact/event |

A receipt's original worker/card/Track identity is immutable. Reconstruction does not grant
replacement execution. The proof's private constructor and exclusive transaction borrow enforce
the live insertion invariant as callers grow; this table is explanatory, not an enumerated fence.
New native ingress uses the existing proof-taking entry without adding a policy list or test-name
pin. Ordinary later test/file additions need no changes to this rule. Focused retirement tests
exercise production dispatch; CI and the task gate own their declared replay frequency.

`ExpectedCarrier` must include the exact current card/Track/worker and binding, full serialized
snapshot/execution fence, accepted event prefix **and all relevant ingress event arrivals**,
per-card watermark, complete presentation/raw-row identity and content revision (max sequence
alone misses in-place updates and terminal rows), open ask state, native binding/registration,
retry submission rows (including root-plan references and effective/retired claims), retry-intent
relations and their exact resolution receipts, and caller-owned role revision when applicable.
These are required CAS inputs, not a claim that `replace_idle_carrier_tx` is already implemented. A committed ingress write precedes
rotation and invalidates that comparison, or follows rotation and is refused as retired.

Positive Codex evidence requires a current source, a known ended execution, and no pending,
steering or unresolved slot; `active_turn_id == None` and absent owner handles are insufficient.
`CardStartFence::require_settled_tx` returns a typed `SettledCardStart` from the existing operation
recovery inventory. Caller retains the card-start and drive guards: a retained `spawn_op_id`
whose operation completed is history, not a live reservation. ACP unresolved submission and
dispatcher event-prefix transaction forms reuse the original query predicates/event kinds.
Truth owns the persisted `PlannerBinding` decoder; the server admission wrapper retains its
backend `managed_conversation` capability check. The current canonical runtime frame is V5
(with explicit released V2/V3/V4 conversion); its header version is independent of submission proof/render-plan versions.

The caller acquires sorted card fences, then drive → Track lifecycle → resume locks as applicable
(`S/planner_hibernation.rs:97,104`); no new inverse Track-first lock order. Hold dispatcher push admission
plus native issuance parks through final recheck/commit/publication. Extend existing owner
park contracts, not a parallel busy checker. `lock_if_idle` checks issuance/ingress but is insufficient alone
(`S/harness/run_loop/hibernation.rs:9,107,113`). Protect ask creation, event arrival and unfinished native
registration through shared guards or transaction-current SQL admission. Reuse dispatch qualification/prefix
queries (`S/dispatcher/canonical_inputs.rs:278,324`), not a second event-kind list.

#2774 owns `/planner`, two-card composition, its role revision/ABA CAS, error codes,
exactly-one-Planner check, role-change receipt/fact and event baseline policy. Its
caller checks open wake asks **before** revocation (`S/ask.rs:48`), eligible but
unaccepted events and every row below. Pair publication must repopulate both
resulting role entries synchronously after commit while admission readers remain
excluded; cache invalidation alone denies valid new admission. Existing per-entry
DashMap writes (`crates/calm-truth/src/card_role_cache.rs:40`) are not atomic pair
publication, and dispatcher reads before its push lock (`canonical_inputs.rs:244`).
#2774 must protect/recheck those readers, not assume the lock already does so.
Acceptance races cover append, event arrival, ask creation and native registration versus rotation;
postcommit crash must not revive old authority. No U10, role route, provider-change policy or hydration
work is delivered here.

## 6. Explicit owner compliance

Source contracts (read in full): [#2671 constraint
table](https://github.com/keanji-x/neige-calm/issues/2671#issuecomment-6289075160) and [#2774
inventory](https://github.com/keanji-x/neige-calm/issues/2774#issuecomment-6289085706). S/T/Y/M mean server
src / Truth sqlite / types src / Truth migrations. Evidence is rechecked against this checkout; comment line
numbers are not treated as current proof. These tables are design handoff evidence, not permanent executable
pins.

| #2671 constraint row | Satisfaction / deciding owner |
| --- | --- |
| 作者与身份 | §§2–4 preserve ids/exact revisions/original actors; source auth stays independent of runtime/viewer. No remint on rotation |
| 历史来源与无损性 | §4 explicit imported Unknown, no fabricated CHECK author; folded bodies drain or approved abandonment; original provable ACK retained. #2671 approves imported representation |
| 单一真相与批次 | §2 sole checkpoint; existing claim owner freezes exact ordered refs/revisions/authors/plan/digest/logical identities transactionally; effective claim remains message-unique across revisions. No prefix=1/single-source protocol added |
| 换carrier与CAS | §§4–5 full current/snapshot/prefix CAS, original target and receipt proof; pending/active/Unknown refuse; no downgrade to NotDispatched or new-worker resend; null ACK/create/send namespaces preserved |
| 已接受输入与撤权 | Accepted input belongs to Area; removing author only revokes future access/events, never silently drops queued/mixed work. #2671 owns future grants; current sole-owner/Private/live-source fence stays; no borrowed HOME/credentials |
| 历史不可变与本人控制 | Preserve monotonic Private/SharedHistory and consumed history. Existing owner allows own pending edit/withdraw only before effective claim; Unknown retains claim. Explicit private erasure preserves digest/provenance, no rotation erase grant |
| 职责与文件协调 | This track owns migration/compatibility/primitive; #2671 owns authors/batches/C4. Its current C5 list handlers, track_access/list tests/OpenAPI and prospective C3 types/Truth messages,acceptance,claims,claim_validation,checkpoint,plans/read/server input/dispatch files serialize with our slices; no scheduled C3 writer; new migrations only |
| 设计交接验收 | §§1–5 specify provenance, folded limits, refusal/drain, namespaces/CAS and real provider send/END; no process driver/resource ledger/delivery-candidate state machine. Shared execution remains outside storage unification |

The next table covers every semantic bullet, refusal row, verified-impact row, role-guard group and
cache/credential item in #2774; repeated surfaces are kept explicit so owner obligations cannot disappear
behind generic “cache cleanup”.

| #2774 inventory item | Satisfaction / deciding owner (source where useful) |
| --- | --- |
| Status: #2774 paused until canonical cutover; primitive ownership | §7 early primitive, product release after final retirement; #2774 owns composition/revision/route |
| Same card/title/history; Planner role; Worker/Report unaffected | §§1,4–5; #2774 chooses roles, no CardRole addition (`Y/model.rs:13`) |
| Human /planner; atomic demote/promote; no clone/archive | #2774 caller only; primitive twice in one transaction (§5) |
| Demoted chattable; no Planner wakes; direct/catch-up role recheck | #2774 admission/publication; `S/dispatcher/mod.rs:1364`, `canonical_inputs.rs:244,267` |
| End old sessions including hibernated | §5 supersede/revoke, native owner detaches; hibernation is not retirement |
| Next turn fresh role/session/credentials; hydration dropped | §5 empty successor, no old-context resume under new binding |
| Preserve provider/preferences; normalize role payload; validate binding | #2774 policy; §5 accepts any valid binding, copies no other card's settings |
| Immutable old worker contracts/native ids/receipts/actors | §5 ledger retained; canonical current-carrier CAS reused |
| Card tokens and all retired session hashes; card-scoped sweep | §5; `S/mcp_server/wiring.rs:125,143,165`, `T/session_mirror.rs:262` |
| Harness slots/reservations, replies, sources, native subscriptions/mappings | §5 identity-safe retirement, all slot states; late callbacks current-fenced |
| Protected paired CardRoleCache publication; SQL role auth | #2774 fenced repopulation after commit; §5 hooks, no speculative writes |
| No MCP nomination tool; old connections lose authority | §5 transaction-current mutation/list/handshake checks; #2774 owns route |
| UI target id and Assistant list membership | #2774 UI; `S/routes/track_conversations.rs:382`; history identity unchanged |
| Current Planner from role, root repoint, historical parents retained | #2774 sets promoted fresh root in same tx; §5 explicit link facts |
| Nondeletable cards; old sessions not recovered/task-converged | Preserve `S/operation/planner_harness_start_adapter.rs:315`; reaper current-session fences (`S/reaper/mod.rs:103,167`) |
| planner_user_required | #2774 authenticates live Human at tx boundary; primitive caller precondition |
| planner_target_invalid | #2774 distinct Assistant/same open Track/conversation/unarchived/unique Planner check |
| planner_binding_unsupported | Existing profile decoder plus readiness owner; §5 typed binding |
| planner_historical_storage | §5 refuses historical; obsolete for #2774 release after final cutover |
| planner_pending_observations | §5 protected ingress + eligible unconsumed durable prefix; dispatcher owner query |
| planner_pending_messages | §5 pending Human/System/steers refuse; malformed is not empty |
| planner_turn_in_flight | §5 phase/active/native/start/rewind/compact/recovery/interruption/wedge refusal |
| planner_open_ask | #2774 uses `S/ask.rs:48` wake/hold plus native question/approval owners before revocation |
| planner_pending_submission | §5 crossing/Unknown, `T/acp_submission.rs:65` Sending/Unknown and unfinished native calls |
| planner_changed | §5 pointer/full snapshot/binding/prefix counts; #2774 expected Planner + monotonic revision CAS prevents ABA |
| planner_session_unavailable | §5 owner idle/detach/issuance proof; unknown/timeout refuses without mutation |
| Sequence 1: lifecycle guards | §5 sorted card → drive → Track → resume order; #2774 composes both cards |
| Sequence 2: producer exclusion | §5 ask/event/native producers share guards or transaction-current admission |
| Sequence 3: protected SQL reload | #2774 final BEGIN IMMEDIATE reloads both roles/revision/carriers/durable refusal predicates |
| Sequence 4: demote then promote; one-Planner index/update counts | #2774 calls primitive twice; `M/0083:68` unchanged |
| Sequence 5: root, revision, fact, new event baseline | #2774 only; primitive exposes facts and revision-CAS hook, never copies other watermark |
| Sequence 6: commit then synchronous protected publication | §5 hooks; #2774 holds admission through pair refresh/cleanup; SQL survives crash |
| No RPC-spanning tx / async swap machine / compensation | §5 no provider awaits/commit inside primitive; caller preflights then protected recheck |
| Card-scoped canonical carrier/history and acceptance tests | §§4–5 same index/pagination; old facts immutable; new send uses fresh carrier |
| M/0083:52,68 rewrite/index | Byte-frozen; #2774 adds revision in NEW migration |
| S/harness/profile.rs:237 current provider | Decoder resolves current role/binding (`:245`); #2774 rechecks, no creation-order inference |
| S/planner_hibernation.rs:19 candidacy | #2774 old Planner ineligible; fresh current Planner normal; retired rows cannot resume |
| S/track_activity/sql.rs:281 activity | #2774 verifies current-role query, no original-card pin |
| T/acp_submission.rs:79 admission | §5 current role/provider/contract/session remains required; original receipts immutable |
| T/conversation_title.rs:23 shared title | Keep card-based Planner/Assistant title/read path |
| T/read.rs:912 initial-prompt takeover | #2774 verifies retired worker cannot be adopted; no initial receipt as current pointer |
| T/session_projection.rs:888 boot, including Codex :875 | §4 boot barrier; §5 retired/new role/current-carrier agreement |
| T/session_system_error_recovery.rs:53 | §5 retired contexts unrecoverable; keep current role/contract predicates |
| S/decision_sink.rs:835,980; T/conversation_title.rs:368 | Test setup role writes are not a production primitive; #2774 owns real composition |
| M/0175:254 original creation binding | Keep immutable creation-time ACK; never current Planner authority |
| Role wire/DB Y/model.rs:19,29,39 | No encoding/enum change |
| Actor selection Y/ids.rs:62 | Original attribution unchanged; current execution role resolved by owner |
| Profile/session/MCP S/harness/profile.rs:23,43,52 | §5 new immutable binding, old context never reused for changed role |
| Report attribution S/decision_sink.rs; track_report/identity.rs::report_op_attribution | Preserve original author/task/approval/commit/gate/plan attribution |
| MCP terminal setup S/mcp_server/wiring.rs:87 | Next authorized activation binds new role surface |
| Forge cwd S/mcp_server/transport.rs:1110 | Existing current-role/resource policy; no migration authority expansion |
| Models S/routes/models.rs:226 | #2774 verifies current role/provider model lookup, no copied preferences |
| Track reset S/routes/tracks.rs:1378 | #2774 verifies current role branch and carrier fence; no recreation of accepted work |
| Live role gates T/role_gate.rs:168,182,233,455,474,486,568 | Keep SQL-current roles; §5 stale admitted mutations must fail |
| Creation/start role S/operation/planner_harness_start_adapter.rs:343,400,671,1195 | §§3,5 owning binding decoder; fresh contract, no inferred old role |
| Recovery S/harness/mod.rs:211,246,268,395 | §4 original qualified migration; §5 retired rows excluded, no V1 fallback |
| Dispatcher S/dispatcher/mod.rs:1364 / canonical_inputs.rs:244 | #2774 protects both direct/catch-up readers through pair publication (§5) |
| Ask admission S/ask/forge_approval.rs:53 | #2774 shared exclusion/SQL-current admission prevents ask race |
| Visibility S/mcp_server/tool_visibility.rs:137 | New role gates apply on calls/lists; no nomination surface |
| Shared CardRoleCache + boot seed T/card_role_cache.rs:22,40,63 | §4 seed after completion; §5 protected refresh, never remove-only |
| Transaction role/Area caches T/decision_gate.rs:227 | SQL-hydrated transaction-local caches; discard stale ephemeral role snapshots |
| Runtime slots/source indexes S/harness/registry.rs:18,25 | §5 retire Dormant/Reserved/Live ownership explicitly |
| Dispatcher push cursor S/dispatcher/mod.rs:1347 | §5 own prefix preserved; #2774 defines promoted event cut, no cursor copying |
| Native thread-card cache S/shared_codex_appserver.rs:1559 | §5 invalidate attribution and native subscription independently |
| Role/provider/session decoder S/harness/profile.rs:70,155,191 | §5 validates both binding and session contract, required fields |
| Token mint/mirror S/mcp_server/wiring.rs:119,143,165; T/session_mirror.rs:262 | §5 revoke both stores incl. stale mirrors; next activation mints fresh pair |
| Active-token T/session_row.rs:161 | Retired token rejected by persisted active authority predicate |
| Handshake/call/list S/mcp_server/handshake.rs:70; transport.rs:532,1634; transport/catalog.rs:40 | §5 stale connection and consequential in-flight call rechecks |
| Codex start/recovery factory/codex.rs:43, factory/recovery.rs:166 | §5 no loaded old credentials under new role; §4 settled same-role context preserved |
| Claude/ACP lifecycle claude_planner/lifecycle.rs:12,58; acp_planner/config.rs:346; session.rs:552 | Owner integration must use card-scoped revocation, not track-wide cleanup (§5) |
| TrackAreaCache / ephemeral snapshots | Membership unchanged; discard old role snapshots; #2774 refreshes pair only |

## 7. Ordered slices and gates

Every row is a separate closed-outcome PR, L2 (persistence/authority). Persistence and credential
slices stay at or below 1,500 changed lines including code/tests/generated artifacts; U1b-pre's
design-document update is excluded from its measured code/test cap. The three approved combined
provider/entry/deletion rows may reach approximately 2,500 lines. Split only when the applicable
limit is actually exceeded, and never separate acceptance from working retained activation.
#2774 product release waits for complete canonical cutover; U10 is deleted, not reassigned here.
U1b-pre is delivered before the one-card replacement primitive; the table has fifteen rows.

| Order / slice / target lines | Real dependencies | Owning files / closed production acceptance |
| --- | --- | --- |
| 1 / U1a current-carrier authorization fences / 900 | Existing Codex canonical Planner | MCP transaction owners/transport, native pre-send owners; stale already-admitted mutation and native-registration race fail after retirement; no new tool |
| 2 / U1b-pre owner-local quiescence prerequisites / 1,500 | U1a | MCP in-flight fence/bounded cleanup, positive native/harness safe-detach guards, unresolved-start typed tx proof, ACP/event-prefix tx queries, Truth binding decoder with server capability admission; production races, retained completed start and fence/idle mutation evidence; no replacement/revocation/publication |
| 3 / U1b one-card tx rotation + publication hooks / 1,400 | U1a, U1b-pre; protected owner proofs in §5 | Truth conversation/carrier sibling, session/token/link owners, registry/publication hooks; full-snapshot/prefix CAS, both token stores, rollback, original history, Codex same-role next-send and cold boot. Twice in one test tx; no product two-card flow/revision schema |
| 4 / U2 imported provenance schema/read / 1,200 | Imported representation agreement | New migration, Truth presentation/read, types/display/generated + consumers; schema-only, no population; echo/missing-native-id, mixed order/cursors, redaction/erasure |
| 5 / U3 Codex Assistant canonical create/activate / 1,400 | Existing Codex lifecycle; U1b for rotation integration | Assistant route, Truth creation receipt, retained/profile/attachment owners; authenticated real HTTP card+carrier+accept+ACK, side context, outage/replay and correct tools; no compensate/requeue |
| 6 / U4 Claude Assistant create/activate / 1,200 | U3; external Claude track slices 2–5 | Same entry/profile with actual Claude send/END/retained/control; lost reply/no resend, cold resume; Planner work is external |
| 7 / U5+U6 ACP host integration + Planner/Assistant admission / 2,200 | U3; external #2836 M3a lazy new/load/receipts/no-resend/driver join; H1 if stronger quiescence needed; Claude slice 5 for shared predicate | Host backend dispatch/END, retained/control/resume and existing creation_plan/dormant + Assistant entry; real local ACP peer qualified original-worker send/load/Stop/compact/END; empty/first input/outage, no V1 fallback or ACP transport reimplementation |
| 8 / U7 template/recipe/fork admission / 1,200 | Delivered provider lifecycle for admitted bindings; Claude slice 5 | Existing preparation/allocation/create owners; immutable recipe/report context, original key replay; need not wait for internal entries |
| 9 / U8 Workspace/attached/cross-area/admin admission / 1,300 | U7, actual Workspace qualification | Existing isolation/fork_guard/area_admin owners; denied/deletion/materialization races, no retained repoint or grant expansion |
| 10 / U9 creator/mail + managed/daily entries / 1,100 | Qualified source/provider contracts; U7/U8 where resources require them | add.rs, tracks.rs, daily_planner; cap race, original mail key, no-start daily carrier, qualified first input |
| 11 / U11+U12 child/Today + summary/watcher System entry / 2,100 | Source decision if bootstrap unrepresentable; source agreement; lifecycle/Workspace proof; U3/U4/U6 for admitted provider | child_track_adapter, scheduler, track_creation, today, summary/watcher/dispatcher; inherited provider/Area, no bootstrap remint, retained-safe adoption preserving history; authenticated issuer/prefix, replay/cold watermark, no server_send fallback |
| 12 / U13 offline classifier/export/boot barrier / 1,200 | U2; approved §9 dispositions | Truth upgrade/boot owner + offline command; no population yet, manifest covers all stores/keys, exact classification, boot fails closed when required |
| 13 / U14 atomic migration application / 1,400 | U13; all entries/lifecycles delivered and stopped backup rehearsal | Truth upgrade tx + receipt replay owners; in-place worker/native continuity, original/null ACK, atomic index/frame/completion, crash each phase/restore |
| 14 / U15+U16 historical input/control + creation/start deletion / 2,500 | U14; external Claude slice 6 | Historical queue/parallel codecs/input/edit/withdraw/steer/Stop and create/delivery/start/input_transfer/Assistant wire owners; canonical production paths + Worker/PlainChat regression, factual digest replay; every §1 entry canonical, original receipt replay, no queue harvest/retry mint |
| 15 / U17 raw read/wire retirement + lifecycle rows / 1,200 | U16; released-client window | Canonical read/types/generated/frontend consumers/lifecycle doc; browser pagination/outbox/title/Tier1; remove negotiated raw routing only after client retirement |

Worker runs focused production-entry tests and formatter fixes; kernel owns required text/contract/quick
checks and one focused replay rejecting empty selection. CI owns broad suites. Docs-only revision: kernel
ratchet only. Future security/no-resend/CAS tests get exclusive-worktree mutation verification with predicted
complete red sets. Browser changes require actual preview/browser and credential-free Tier1; never real Codex
E2E on the shared host. No new permanent test-name/entry/provider checklist: ordinary additions change their
owning typed constructors/decoders/tests; each required gate runs once plus the stated replay.

## 8. Adjacent ownership and compatibility exit

Claude track `85f4c565` owns blank managed Planner slices 2–6: local start, Stop/END, retained recovery,
compact, opening creation and legacy removal. Its slice 5 extends creation_plan predicates only, not card
tokens/session mirror. Serialize shared creation_plan/dormant, retained/recovery, canonical control/resume,
backend and tests; our predicate edits follow slice 5, final deletion slice 6. #2836 owns ACP
transport/runtime contracts; this track only consumes them and adds canonical host integration. #2671 owns
authors/batches/shared authorization, with C3/C5 overlap enumerated in §6; reserve migration numbers and
serialize shared files. #2774 owns all product role-change policy/composition; no parallel route or manager.

U17 updates lifecycle compatibility rows: historical executable snapshots and first-message preparation
removed after U14–U16; canonical retained activation expanded only by qualified provider/profile lifecycles;
explicit authorized empty carrier rotation added separately from recovery; negotiated raw display removed
after client retirement. Keep private/live-source and retained Workspace-repoint restrictions unchanged. Keep
original receipt replay and immutable execution attribution; audit exports are not an application read
fallback. Shared Worker/PlainChat tables survive. Released migrations are never edited; real schema/wire
generators must supply all artifacts. This work does not deliver shared execution.

## 9. Remaining human/owner decisions

1. #2671 approves concrete imported-fact schema/display/identity and mixed-history append boundary,
   and qualifies System bootstrap for child, Today, summary, watcher/mail where existing issuers
   cannot express it. No fabricated Human.
2. Operator identifies abandonment approver, exact unprovable records/prefixes, audit retention,
   stopped cutover/backup window and restore rehearsal. Proven settled contexts must be preserved;
   abandonment is never blanket retirement.
3. Owners set released-client compatibility window and resolve any demonstrated unprovable
   receipt/namespace or stronger post-deletion retention requirement; provable original ACK replay,
   including null, is already required, not optional.
4. Provider owners attest idle/detach/recovery/registration contracts and delivered dependencies;
   validate on a stopped deployment backup. Reported 161 cards at deployed 1846fc277 remain supplied
   premises, not independently verified data.
