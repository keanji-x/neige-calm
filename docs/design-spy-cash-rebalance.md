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

A later fresh review reproduced a legacy/shared Worker admission gap: those
Workers retain ordinary plugin access but have no frozen delegation. Host admission
now returns whether the exact tool was authorized by the current isolated attempt
and supplies required `delegated_tool` metadata to local plugins. `spy.execute`
requires literal true. A real SPY App route test rejects a forged legacy call
and admits the same Worker only after exact isolated binding. Ordinary plugin
contracts retain legacy behavior.

Delegation mutation: expected and actual red sets were exactly `test_spy_legacy_or_unproved_worker_cannot_execute[1]`, `test_spy_legacy_or_unproved_worker_cannot_execute[False]`, `test_spy_legacy_or_unproved_worker_cannot_execute[None]`, `test_spy_legacy_or_unproved_worker_cannot_execute[true]`. The production guard was restored byte-for-byte and all 51 SPY/SDK checks passed.

Role-only mutation: expected and actual red set was `test_spy_planner_cannot_execute_with_claimed_delegation`; the delegation predicate remained intact. This result was rerun in an exclusive period after all other Python readers ended. Production source was restored byte-for-byte and all 52 SPY/SDK checks passed.

## Live setup continuation

SDK OAuth was completed by relaying the matching user-provided callback to the
still-running official SDK listener. Same-login statement/account verification
succeeded. Global SDK quote connections were reset by the host network; the
official CN endpoint succeeded, so `access_region` is explicit trusted config,
with only global/CN official endpoints and no retry fallback after a write.

The original Demo runs with an in-memory database and disabled providers. Keep
its original process/data untouched while preparing a separate persistent
runtime. Seed the new store using normal production APIs, then perform a stopped,
transactional identity transplant for the original Area/Recipe/Track IDs only,
including exact JSON references and managed-path components. No broker plugin is
enabled and no agent runs during this step. Back up the new SQLite store using
the SQLite backup API, require foreign-key/integrity checks and verify the
original IDs/report bindings through normal read APIs before activation. Original
fictional report is preserved in the private snapshot, not imported as fills.
Keep the Demo URL/Track identity; account and plugin ledger survive restart.

Live SDK snapshot exposed native local-naive timestamps. Official v5.2.0
`python/src/time.rs` uses `PyDateTime::from_timestamp(epoch, None)`; conversion
therefore interprets naive SDK objects as process-local time and preserves the
epoch using `astimezone(UTC)`. User timestamps remain timezone-required. A
non-UTC regression reproduced red before this fix.


## Exact tool approval continuation

The real Planner read Longbridge and Wisburg but its `spy.refresh` call was
refused by provider approval policy before reaching the App. Preserve truthful
MCP annotations. For this explicitly authorized private Demo only, the operator
sets `approval_mode = "approve"` for the exact `spy.plan` and `spy.refresh` MCP
names in its private shared provider configuration. No global default changes.
The App still enforces the owning Track and Planner role for plan writes.

An isolated Worker already receives a frozen, validated plugin-tool grant list.
Generate the same exact per-tool approval overrides in its private provider
configuration, rather than prompting again for actions explicitly delegated by
the Planner. Do not approve tools outside that grant list or enable network or
new tools. The transport repeats live-attempt grant checks before execution.
Acceptance: the production private-home test requires exactly the delegated
approval keys; a no-grant home has no plugin approvals. Keep the message-driven
strategy Track open after each message; only an explicit user request closes it.


Approval regression was red before the change. The first implementation's
explicit child table disappeared under an inline parent; a TOML-only reproduction
confirmed this. The production configuration now uses an inline approval table,
and the test parses the saved configuration with `as_table_like()`.
All five private-home tests passed. An exclusive single-factor production
mutation added `plugin.ungranted` to the approval table: predicted and actual
red sets were exactly
`dedicated_codex::home::tests::dedicated_codex_plugin_grants_are_explicit_and_do_not_enable_network`.
Production was restored byte-for-byte and all five checks passed again.


## Live message and delegated execution evidence

The original Demo identity now runs in a separate persistent private runtime on
loopback port 4145. Longbridge CLI research, official SDK paper account and
Wisburg managed read-only connector are enabled; the original 4143 process and
production 4140 remain untouched. The user explicitly authorized reusing the
existing Wisburg credentials against its original service. SDK credentials,
connector secrets and app authentication stay outside tracked source.

A normal Planner message read actual broker state, Longbridge quotes and
Wisburg research, captured seven sources, and persisted a 1000-bps SPY target.
No real Codex E2E suite ran. Initial independent execution lacked backend config;
the next startup exposed missing explicit proxy transport. Codex 0.159.2 also
publishes a listener symlink to its namespace-private temporary socket, outside
the host's mounted control directory. The Demo now explicitly pins the installed
0.153.4 executable and matching companions. A credential-free, no-model-turn
Unix startup check confirmed a direct socket. A separate private Worker provider
config selects `gpt-5.6-sol`, actually listed by that account's authenticated
catalog; the prior inherited Planner model was refused by this client. Planner
settings remain unchanged. No automatic runtime fallback or broad permissions
were introduced.

Failed attempts and their native stop records remain intact. Same-key recovery
refusal was respected. Subsequent normal task declarations continued the same
financial decision only after the prior runtime was Closed, its original init
was absent and the broker-request ledger was empty. No financial decision ID,
ratio, validity or source was replaced.

The actual isolated Worker called status, executed the existing decision exactly
once, then called status and refresh and submitted native task completion.
Planner accepted the workflow report and explicitly kept allocation completion
false. The three live tables render in a real authenticated browser with no page
errors. The Track remains open. Market was outside the regular session: decision
stays queued, actual SPY allocation is zero, and there are no submitted broker
requests or fills. Regular-session broker acceptance and settlement still require
live verification; queued execution does not schedule a later order by itself.
