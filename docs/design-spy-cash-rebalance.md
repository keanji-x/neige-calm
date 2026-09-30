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
- One unresolved order/decision per account; inspect active orders and positions before execution. Cash buffer, maximum order fraction (default 10% of current cash plus SPY value) and quote freshness are typed configuration. Reject unexpected positions/activity rather than attributing it silently.
- Planner source citations and rationale are durable decision evidence. Research cannot alter account, execution policy or identity.

## Acceptance
Exercise production stdio and broker adapter entry points with external fixtures: plan -> delegated execution -> buy fills -> lower target -> sell fills -> restart and read status. Verify no-op/tolerance, whole-share rounding, cash-only sizing, expired decisions, stale prices, nonregular session, role/account/Track refusals, partial fills, rejected orders, conflicting executions, external activity and timeout-after-acceptance recovery. Mutation-verify account/role fences and no-repeat-submit assertions. Run relevant focused tests, text ratchets, two independent complete-diff reviews, and Rust preflight for the local caller metadata change. Actual paper-account order acceptance is reported separately from fixture checks.

## Invariant verification

The complete SPY/SDK corpus was run for each single-factor production mutation,
then restored byte-for-byte and rerun green. Expected and actual red sets matched:

- account: `test_spy_paper_account_fence[change0]`, `test_spy_paper_account_fence[change2]`.
- worker: `test_spy_production_stdio_entrypoint_and_overlays`, `test_spy_role_fence[spy.execute-role1-args1]`.
- repeat: `test_spy_no_repeat_submit_after_lost_response`, `test_spy_partial_fill_and_conflicting_execution_rollback`, `test_spy_production_stdio_entrypoint_and_overlays`, `test_spy_proved_not_submitted_resolves_without_unknown`, `test_spy_terminal_retry_preserves_decision`, `test_spy_unknown_without_broker_match_stays_blocked`.
- durable: `test_spy_intent_is_durable_before_broker_write`.
- server-paper: `test_sdk_context_hard_codes_server_paper_enforcement`.
- not-submitted: `test_spy_proved_not_submitted_resolves_without_unknown`.
- step: `test_spy_default_step_is_ten_percent_of_portfolio`, `test_spy_insufficient_cash_and_maximum_order`.

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

## Review fixes

Independent reviews found and reproduced half-day calendar membership and
definite pre-submit failures being mistaken for unknown writes. A separate
review also found a freshness clock captured before quote network reads.
The calendar uses both disjoint day lists; an exact not-submitted protocol is
reserved for completed SDK preflight; the broker call remains outside that
catch; clocks are read after observation and immediately before the write.
Regression coverage includes half-only mornings and closure, fresh quotes
created during reads, no-write refusals and ambiguous failures after write start.

The user selected the current account's 10% value as the basic execution unit
and asked to reuse the existing Demo Track. The default cap is now 1000 basis
points, computed from current SPY plus cash, with at most one step per message.
The Demo can retain its identity while fictional report records remain separate
from the broker ledger. No new Track is required.

## Existing Demo

The requested Demo was located in the PR #1770 preview, not the 4140
production instance: `http://127.0.0.1:4143/next/track/ad8da32d19fa4752bcfc3c33e9364a07`.
Its Track is `ad8da32d19fa4752bcfc3c33e9364a07`, recipe
`f24352b966db4fe7add5d63f8b849a62`. The current preview has no plugins,
uses an in-memory DB and `/bin/false` for both Agent providers. This change
does not claim that installing the App alone upgrades that preview into a
working Planner runtime. Keep a snapshot before replacing the preview process;
the in-memory state cannot survive a restart. Report/Track/Recipe/Area snapshots
were saved privately for subsequent setup. The 4140 production service was
only inspected; no production service restart or configuration write was done.
