# SPY/cash paper allocation

Issues: https://github.com/keanji-x/neige-calm/issues/1906 and
https://github.com/keanji-x/neige-calm/issues/1966 (slice 3). Supersedes the
draft #1963, which depended on the isolated executor removed by #1908.

## Outcome
A Planner receives a message, reads Wisburg research and Longbridge SPY quotes, persists a target SPY/cash allocation, and declares an ordinary read-only Codex Worker task for that decision. The Worker requests execution; the App's background loop submits at most one regular-session market order per immutable decision. Reconciliation records actual executions and updates the allocation. No per-order human confirmation is needed for this explicitly configured official paper-account profile.

## Boundaries
- Preserve the existing supervised paper strategy profile and its CLI confirmation contract.
- Add an explicit SPY/cash execution profile to the paper-trading App, pinned to one account and owner Track. Only `SPY.US`, USD integer shares, cash-funded long positions, and regular-session DAY market orders are supported.
- The kernel passes the host-resolved caller `{role, card_id, session_id}` under `_meta["dev.neige/caller"]` to local stdio plugins on agent `tools/call` only. Request `_meta` and arguments are never forwarded as identity. Kernel-initiated calls (report-series reads, card routes) and remote/CLI/builtin connectors receive none. The kernel holds no SPY or App identity.
- Plan writes require the Planner role; execution requests require the Worker role; both on the owner Track. Any Worker-role caller on that Track may request execution (accepted gap: no proof the Planner created the task).
- Every SPY tool is local-only and annotated `destructiveHint: false`, `openWorldHint: false`, so Codex agents with `approval_policy = never` call them without approval overrides. Broker reads and writes belong to the App's background loop, as with `paper.refresh`.
- Use the official SDK with server-side paper-account enforcement and explicit OAuth client configuration. Never extract/decrypt CLI credentials or redeem native confirmation codes.
- Planner source citations and rationale are durable decision evidence. Research cannot alter account, execution policy or identity.

## Decision states

`queued` (planned) → `requested` (a Worker asked; journal records the caller) →
`submitting` (exact intent committed) → `working` / `unknown` / `rejected` →
`settled`, `canceled`, `expired`; or `requested` → `noop` / `expired`. A
`requested` decision blocked before any broker write (session closed, stale
quote, insufficient cash, unavailable shares) keeps its request and an `error`
explaining the wait, and is retried each poll until it expires.

## Invariants
- The exact order request and `submitting` state are committed before the SDK submission, under the cross-process operation lock.
- At most one order per decision: only a `requested` decision is ever submitted, and submission moves it out of `requested` in the same committed transaction.
- Unknown outcomes (timeout, crash after commit, lost response) reconcile only by exact remark/payload match and are never resubmitted.
- One unresolved decision per account; a new target requires the previous one to be final.
- A completed SDK preflight refusal (`not_submitted`) is distinguishable from a possibly accepted write (`unknown`).
- Requests for an unknown, expired or no longer queued decision are refused at request time and write nothing.
- Unknown active orders, external positions, conflicting execution IDs and incomplete fill totals roll back the whole observation and block execution.

## Acceptance
Exercise production stdio and broker adapter entry points with external fixtures: plan -> execution request -> background submit -> buy fills -> lower target -> sell fills -> restart and read status. Verify no-op/tolerance, whole-share rounding, cash-only sizing, expired decisions, stale prices, nonregular session, role/account/Track refusals, partial fills, rejected orders, conflicting executions, external activity and timeout-after-acceptance recovery. Mutation-verify role fences, intent durability, no-repeat-submit and the kernel's refusal to forward request `_meta`. A real-App kernel test admits a Planner plan and a Worker request with their resolved roles.

## Validation limits

SDK 5.2.0 was built from the official tag in an isolated virtual environment and
its actual signatures/types were inspected. SDK OAuth is a separate one-time
account authorization; CLI's encrypted credentials are not decoded or exported.
Fixture acceptance is not evidence of an actual broker order or a live Planner
model run. Shared-host real Codex E2E is not enabled by this change.

The live evidence recorded in #1963 (a regular-session paper order of 13 SPY
shares reaching `settled` on 2026-10-02, and the queued outside-session run) was
produced on the removed isolated-executor runtime with a pinned Codex. It is not
evidence for this design. Live paper-order acceptance through an ordinary Codex
Worker task on main must be re-proved during the #1966 deployment slice.
