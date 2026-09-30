# SPY/cash paper allocation

Issue: https://github.com/keanji-x/neige-calm/issues/1906

## Outcome
A Planner receives a message, reads Wisburg research and Longbridge SPY quotes, persists a target SPY/cash allocation, and dispatches a Worker with the execution tool. The Worker submits at most one regular-session market order per immutable decision. Reconciliation records actual executions and updates the allocation. No per-order human confirmation is needed for this explicitly configured official paper-account profile.

## Boundaries
- Preserve the existing supervised paper strategy profile and its CLI confirmation contract.
- Add an explicit SPY/cash execution profile to the paper-trading App, pinned to one account and owner Track. Only `SPY.US`, USD integer shares, cash-funded long positions, and regular-session DAY market orders are supported.
- Pass resolved caller role/card/session metadata only to local plugins. Plan writes require Planner identity; execution requires Worker identity and existing delegated plugin grants. Arguments cannot supply identity.
- Use the official SDK with server-side paper-account enforcement and explicit OAuth client configuration. Never extract/decrypt CLI credentials or redeem native confirmation codes.
- Persist intent and exact quantity before submission. Recovery reconciles unknown results and never resubmits. Broker idempotency has a limited lifetime and does not replace the local journal.
- One unresolved order/decision per account; inspect active orders and positions before execution. Cash buffer, maximum order value and quote freshness are typed configuration. Reject unexpected positions/activity rather than attributing it silently.
- Planner source citations and rationale are durable decision evidence. Research cannot alter account, execution policy or identity.

## Acceptance
Exercise production stdio and broker adapter entry points with external fixtures: plan -> delegated execution -> buy fills -> lower target -> sell fills -> restart and read status. Verify no-op/tolerance, whole-share rounding, cash-only sizing, expired decisions, stale prices, nonregular session, role/account/Track refusals, partial fills, rejected orders, conflicting executions, external activity and timeout-after-acceptance recovery. Mutation-verify account/role fences and no-repeat-submit assertions. Run relevant focused tests, text ratchets, two independent complete-diff reviews, and Rust preflight for the local caller metadata change. Actual paper-account order acceptance is reported separately from fixture checks.

## Invariant verification

The complete SPY/SDK corpus was run for each single-factor production mutation,
then restored byte-for-byte and rerun green. Expected and actual red sets matched:

- account: `test_spy_paper_account_fence[change0]`, `test_spy_paper_account_fence[change2]`.
- worker: `test_spy_production_stdio_entrypoint_and_overlays`, `test_spy_role_fence[spy.execute-role1-args1]`.
- repeat: `test_spy_no_repeat_submit_after_lost_response`, `test_spy_partial_fill_and_conflicting_execution_rollback`, `test_spy_production_stdio_entrypoint_and_overlays`, `test_spy_terminal_retry_preserves_decision`, `test_spy_unknown_without_broker_match_stays_blocked`.
- durable: `test_spy_intent_is_durable_before_broker_write`.
- server-paper: `test_sdk_context_hard_codes_server_paper_enforcement`.

An initial replay mutation check was invalidated because its stdio state assertion
could be repaired by background reconciliation before observation. The test now
also checks the immutable no-op audit event, and a separate direct terminal-replay
assertion pins the state. Fresh mutation runs above matched their complete
predictions; no mutation residue remains.

## Validation limits

SDK 5.2.0 was built from the official tag in an isolated virtual environment and
its actual signatures/types were inspected. CLI paper-account identity, real
quote/account read APIs and Wisburg MCP handshake/tool discovery were checked in
the requesting session. SDK OAuth is a separate one-time account authorization;
CLI's encrypted credentials are not decoded or exported. Fixture acceptance is
not evidence of an actual broker order or a live Planner model run. Shared-host
real Codex E2E is not enabled by this change.
