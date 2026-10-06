# Explicit conversation rebuild with retained history

Related: #2192, #2131, #2243. Review tier: L2, because rebuilding replaces
persisted session ownership and provider credentials. This design is the contract
for the server slice, followed by the maintained frontend slice; it does not
claim either slice is implemented.

## Outcome

A human can explicitly start a fresh provider thread for an unrecoverable
conversation, including an unconfirmed Stop. The card and readable transcript
remain. The new thread receives the card's normal profile/template instructions
and current model selection, but no previous transcript or system observations.
Previously queued, unsent human messages retain their complete queue identity,
revision, enqueue time, message IDs and attachments. They are not model history.
The confirmation must explain that these unsent messages will be sent in the new
thread. No automatic rebuild occurs on Send, reconnect, or page load.

An existing system-error conversation that can resume its original thread keeps
using human-send recovery. The destructive operator `/planner/reset` remains
separate; `fe/` never calls it. Fix its failed-predecessor queue gap through the
same authoritative queue-transfer primitive, without changing its explicit
transcript-clearing contract.

## Admission and durable identity

- Add `POST /api/cards/{id}/planner/restart`, with a required `Idempotency-Key`
  and required `expected_worker_session_id`. Only a human may admit it.
- Use the existing operations journal, key parser and keyed-answer contracts.
  Repeated attempts under the same key and request return the same operation and
  successor; changed card/predecessor intent is a typed key-reuse conflict.
  Resolve an admitted replay before mutable eligibility or provider readiness.
  Do not rebuild the replay fingerprint from a changed workspace/profile/model.
- Hold `CardStartFence` for HTTP admission and normal completion. Cancellation
  releases that process lock but cannot cancel or authorize a different durable
  operation, so the lock is not the ownership proof.
- The start payload carries an optional, omitted-by-default restart contract;
  old serialized operation payloads and hashes remain byte-compatible. For that
  contract require a new thread with transcript clearing disabled, no
  create-card seed, first message, goal, or opening briefing. Keep existing
  payload modes valid. No released migration is edited.
- Validate card/profile ownership and the workspace through their existing
  owners. Do not infer recovery eligibility from a Track label. Closing a Track
  commits its scheduling flag; it does not prove that all old work stopped.
  Deletion and workspace retirement still fence recovery.

## Quiescence before minting

The frontend offers an explicit restart attempt for `interrupt_timeout`; that
state does not prove the old provider turn stopped. The server owns verification.
A successful existing best-effort shutdown is not a stop receipt: it can swallow
interrupt errors, and a successful interrupt RPC is not a terminal turn event.

Before transferring queue or minting a successor, close the predecessor's local
issuance/observation gate and obtain a settled snapshot. Through the provider
owner, check the exact old thread, interrupt a positively identified active turn,
and perform bounded read-back until no active/in-progress/unknown turn remains.
Do not treat a cache miss, failed read, malformed response, interrupt acceptance,
or deadline expiry as confirmation. Claude's scoped process stop must propagate
its result. A missing provider cannot admit a new operation; a recorded replay
must still resolve. Provider-specific stop policy belongs to the backend layer,
not the route or frontend.

If stop cannot be confirmed, retain the old carrier, history and queue and return
an unconfirmed outcome. Do not mint a new thread or replay an accepted-but-not-
checkpointed batch. The operation journal must distinguish stop requested from
stop confirmed and allow an explicit same-key retry to finish this phase. Do not
mark an uncertain stop as terminal failure and then silently create a second
intent. The server slice must implement the lifecycle below before exposing the
restart action in `fe/`.

## Required lifecycle extension

The current operation driver turns every interaction error into compensation,
and its `Parked` state requires spawn artifacts. Neither represents an
unconfirmed stop. Add one generic nonterminal `AwaitingRetry` phase and a deferred
interaction outcome; do not special-case planner/provider identities in the
driver. Persist it through a new migration that preserves all existing operation
rows, keys, indices and parked-resource constraints. Sweep phase serializers,
claims, recovery plans, result readers, registries and goldens.

| Step | Durable state and effect | Who may advance it |
| --- | --- | --- |
| Admission | `prepare_tx` validates current predecessor and exclusive intent, then records intent and phase receipt only; no session retirement, queue move or placeholder | Initial explicit human request |
| Stop requested | Checkpoint exact predecessor/thread identity before provider effects; block this carrier's issuance and recovery | Owner adapter under journal lease |
| Stop confirmed | Checkpoint the provider proof and settled predecessor snapshot | Provider owner, after read-back |
| Stop unconfirmed | Atomically enter `AwaitingRetry`, persist receipt and release lease; carrier, history and queue stay retained and blocked | Owner adapter on timeout, unreadable proof or uncertain response |
| Retry | CAS `AwaitingRetry` back to the saved interaction continuation; retain the operation ID and key | Explicit same-key human retry only |
| Queue commit | Current-link CAS, full user-entry move/undo journal and operation placeholder in one transaction | Adapter after confirmed stop |
| Mint/bind/spawn | Mint a fresh thread; checkpoint and spawn only while own placeholder remains current | Existing start owner with ownership guards |
| Success | Durable receipt names the original successor and ready state | Existing completion path |

`AwaitingRetry` is excluded from ordinary drive claims, boot recovery execution
and background sweeps. Receipt reads do not rearm it. The HTTP wait is bounded
and returns the persisted unconfirmed receipt without classifying it as success
or terminal failure. Concurrent explicit retries claim the continuation once;
an expired retry lease cannot let two owners perform a queue move or bind.
Cancellation or process restart during a stop check retains the admission and
conservatively defers the continuation; it never automatically mints a thread.
Once queue commit has landed, normal idempotent checkpoint recovery may finish
that admitted successor, and always checks current ownership.

Admission checks current-link equality and the absence of a competing unresolved
intent in the same transaction as its journal write. Concurrent different-key
requests cannot both pass admission; the loser gets a typed competing-intent
conflict. A same-key replay resolves that recorded
intent rather than readmitting it.

A pending admission fences human Send, boot/lazy recovery and all other start
modes for its exact card/predecessor through a shared owner-level query, not a UI
flag. Competing explicit reset or deletion must settle/cancel that admission
under the same ownership fence before proceeding. Cancellation must not clear
an unconfirmed provider stop or reopen ingress implicitly. Keep the old blocked
carrier until a later proof, supersession or explicit deletion retires it. Every
path that can restore authority must consult this persisted fence.

Use the provider owner's existing full-thread recovery validation as the basis
for stop proof; extend it for exact-turn interruption rather than duplicating a
cache-only liveness rule. Domain proof details stay in the adapter's receipt
payload; the driver only handles deferred lifecycle, lease and explicit retry.
Every proof checkpoint, queue commit, credential bind and compensation checks
the current journal lease as well as card ownership. An expired lease owner
cannot commit after another owner takes over. Keep the issuance fence in force
from proof through queue commit; stop proof cannot survive a newly issued old
turn. Reset/deletion cancellation prevents later rearm of the canceled intent.

## Transactional ownership and queue transfer

After quiescence, the ownership/queue transaction verifies `cards.session_id` equals the
requested predecessor and that its state/snapshot still meets eligibility. It
atomically journals the exact predecessor snapshot/status, retires it, moves only
undelivered `UserMessage` entries and installs this operation's placeholder.
No card-wide harvest of unrelated retired queues is part of a fresh-context
restart. Reject an unreadable predecessor queue rather than discard it.

Use complete serialized `QueueEntry` values for both transfer and compensation;
the existing text/message-ID harvest journal loses attachments, revision and
enqueue time. Do not inherit `SystemContext`, `TrackGoal`, replay watermarks,
completed turns, or ambiguous accepted batches into the new thread. If a pending
batch may already have been accepted by the old provider but its delivery has
not been checkpointed, retain it and enter `AwaitingRetry` until exact old-thread
history and durable message attribution prove whether it was delivered. A
confirmed delivered message stays in old history; only proven unsent entries
move. Missing proof never permits silently dropping an entry or replaying it.
Crash-window tests must pin this classification through the production path. The transfer
must be atomic and one-time, including failed predecessors. The reset path may
keep its existing system-context policy, but shares the full-fidelity user-entry
move/undo primitive.

Before provider credential rotation, binding or card updates, the checkpoint
transaction verifies this operation's placeholder still owns `cards.session_id`.
It must never supersede a raced-in occupant. The spawn boundary rechecks the same
ownership. Compensation restores the predecessor/queue only while this
operation's placeholder or successor still owns the slot; it cannot reauthorize
or overwrite a newer session. Cleanup targets only this operation's provider
resources. The adapter owns teardown; the route adds no second shutdown.

The success reply names the operation ID and its checkpointed successor session
and thread IDs. It is read from that operation's durable output, not from whatever
session happens to be current when HTTP resumes. Retrying an old successful
operation cannot restart or replace a newer session.

## Frontend contract

`GET /planner/run` declares the recovery action and expected predecessor through
one backend-owned eligibility projection. Valid no-loss resume, explicit
restart, and no available action are distinct typed cases. The UI consumes that
contract; it does not parse failure strings, copy snapshot rules, or special-case
Today, providers or Track identities.

Use the #2131 four-state model and the existing admitted mutation/feedback path:
pending, confirmed, refused and unknown. The domain owns the failure table and
fixed unknown text. A stable intent key survives an unknown response; do not mint
a replacement key while that result is unresolved. Reconcile the operation
receipt and current run/history after settlement. A matching operation receipt
confirms the restart even if the card later changed; a different current session
alone does not prove this intent landed. Offline presses are refused immediately
and are never queued for reconnect. No raw transport error is shown locally.

The confirmation names preserved history, fresh model context and retained unsent
messages. While processing or unknown, prevent conflicting Send/rebuild actions.
History remains readable. Show confirmed recovery only when a recorded successor
is ready, not merely when a stop or restart request was accepted.

## Acceptance and delivery

Server slice, fake provider transport only on the shared host:

1. Same key admits one operation and one thread; changed request conflicts;
   replay resolves after provider/workspace/model eligibility changes.
2. Concurrent different-key admission admits one intent; Send, reset and boot
   recovery cannot bypass the unresolved intent. Cancellation/restart during
   stop and provider-accepted-before-checkpoint windows retains data, defers
   safely and never automatically mints or replays ambiguous messages. Exercise
   crashes before/after stop request, after proof and after queue commit. Only
   the last interval may finish minting automatically. Expired lease owners
   cannot checkpoint, move queue, bind tokens or compensate; canceled intents
   cannot rearm. The additive migration preserves old operation rows, key
   uniqueness and parked constraints; phase codecs/registries classify all rows.
3. A running or unknown old turn never mints a successor before confirmed
   quiescence. Stop timeout retains carrier/transcript/full queue; explicit
   same-key retry resumes its operation. No best-effort shutdown is a receipt.
4. Race cancellation, concurrent Send/reset/restart/deletion before prepare,
   between provider mint and checkpoint, and during compensation. A newer
   occupant's session, queue and credentials remain untouched.
5. Preserve transcript and full user QueueEntry metadata from active and failed
   predecessors; omit system observations, old history and replay watermarks.
   Verify rollback and no duplicate transfer through production entry points.
6. No-loss system-error Send still resumes its original thread. Reset still
   explicitly clears transcript, and carries failed user queues without loss.
7. Operation replies retain their original successor identity after later
   rebuilds. No automatic real-session reset is executed during verification.

Frontend slice: unit tests and real-browser integrated coverage at desktop and
390px for eligibility, confirmation, all four outcomes, same-key unknown retry,
offline refusal, history visibility and queue continuation. Keep the existing
assertion that the maintained frontend never posts to `/planner/reset`.

Both implementation slices require two independent L2 reviews covering ownership
boundaries, duplicate policy and application assumptions. Mutation-verify the
small load-bearing ownership, no-mint-before-quiescence, full queue fidelity and
same-key assertions with predicted complete red sets. Run focused tests, contract
and text gates, relevant compile/schema/frontend gates and CI before merge.
