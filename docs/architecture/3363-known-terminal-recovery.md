# Explicit recovery after a known terminal provider error

Review tier: L2, because recovery replaces execution authority and persists a
new carrier while retaining the failed carrier's facts.

The canonical lifecycle declares `recover_session` only for a current failed
Codex carrier with TurnCompleted/system_error, no active input, and an exact
persisted failed native outcome correlated to its accepted input attempt.
Unknown delivery, missing END, pending rewind/interruption and worker-bound
pending inputs remain held. END alone never restores authority.

The Human action uses the existing protected carrier replacement transaction
and durable idempotency receipt. The native owner reads the exact old thread,
requires the matching failed terminal, confirms resumed inactivity, and parks native
issuance through commit. Source, card binding, workspace, backend namespace,
credentials, registry slot, MCP calls and ingress are rechecked before commit.
Uncertain cleanup or ownership changes refuse recovery without modifying facts.

Recovery selects a fresh dormant carrier in the same backend namespace. The
failed snapshot, transcript, delivery/claims, Report and task results remain
untouched. Only card-scoped Human pending references, positions and accepted
watermark transfer. Ordinary current-source admission owns their later dispatch;
the failed original submission never transfers or resends. The frontend explains
that history is retained but a fresh model session has no prior model context.

Acceptance uses the authenticated canonical HTTP/native peer fixture: deliver
systemError before failed END, enqueue two Human messages, recover explicitly,
and check original failure/queue identity/watermark and at-most-once dispatch.
Cover restart, idempotency, revoked source, changed carrier/backend/thread,
missing terminal and unconfirmed native settlement. Preserve existing negative
controls for automatic system_error/runtime_failed END handling.

Verification: worker runs focused regression and mutation checks. Final gates
run quick Rust, text/contract checks, frontend checks, browser coverage and one
focused replay; broad Rust suites remain CI-owned. Two isolated independent
reviews check boundaries, duplicate policy and hardcoded application assumptions.

## Fact ownership and released frames

END closes factual activity even when an earlier system error revoked execution
permission. The canonical END owner clears only its exact failed carrier's
activity mirror under current-card, full-snapshot, thread and matching-turn CAS.
It does not restore Failed, clear the wedge or alter delivery/claims. The generic
active-only session observation writer remains unchanged.

Released known-END frames can retain that exact ended turn in the activity
mirror. Explicit recovery accepts this exact historical shape only with the
persisted failed terminal and a new native terminal/inactivity proof; a different
activity turn remains refused. A native reconnect preserves past END evidence,
while the current connection must issue and retain the recovery proof.

## Approved interface change

The core/api owner change request is to regenerate
`fe/core/api/generated/openapi.json` for the explicit recovery endpoint and typed
recovery/held declarations. Orchestrator decision: approve this scoped extension
for #3363, with REST API 44 and web compatibility 67. Transport and UI consume
the declared lifecycle contract; no provider-error text becomes policy.

## Complete transcript before retirement

Native END can close the logical execution before the harness consumes earlier
content callbacks. Recovery therefore requires a typed complete terminal receipt
from the native owner and projects it through the existing transcript owner in
the same transaction as replacement. The transaction first rechecks the original
snapshot and ingress, applies its own receipt, and then refreshes ingress for the
replacement CAS. It never synthesizes delivery, claims or an END from content.
Missing items, duplicate/invalid item identities and explicit partial views refuse
recovery. Native RPC failures return a fixed unavailable response without exposing
provider payloads.

The full-read contract is verified against Codex rust-v0.163.0: loaded legacy
threads load rollout history, and paginated threads persist and traverse every
page with `TurnItemsView::Full`. See the upstream
[full read implementation](https://github.com/openai/codex/blob/rust-v0.163.0/codex-rs/app-server/src/request_processors/thread_processor.rs#L3155)
and [full pagination](https://github.com/openai/codex/blob/rust-v0.163.0/codex-rs/app-server/src/request_processors/thread_processor.rs#L3447).
The explicit items array remains mandatory, while absence of the newer itemsView
marker follows that full-read contract; an explicit non-full marker is rejected.


## Sticky native error status

Codex retains its system-error flag after failed END and a loaded-thread resume.
The explicit terminal owner accepts a resumed `idle` or `systemError` only with
its exact failed terminal, complete receipt and unchanged ended/no-pending native
lease. Active, unloaded and unknown resume states still refuse retirement. No
business turn is sent to clear the flag, and generic idle detach, settlement and
automatic recovery keep their existing predicates. The upstream
[status owner](https://github.com/openai/codex/blob/rust-v0.163.0/codex-rs/app-server/src/thread_status.rs)
checks running/pending activity before systemError; turn completion and upsert do
not clear the error flag. The real HTTP regression preserves that resume status.


## Composition with Claude identity recovery

Abandonment selection remains in the provider factory, including its typed
UnknownInput/UnconfirmedSession modes and registered-execution quiescence.
Terminal Codex recovery composes with those modes through one protected carrier
replacement owner. Commit rechecks preserve provider-qualified eligibility as
well as terminal context and native proof. Only terminal recovery receives a
native complete transcript receipt; it never broadens Claude identity admission.
