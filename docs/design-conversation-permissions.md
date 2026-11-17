# Shared conversation execution and approvals (#3695)

Review tier L2: execution authority and persistent session boundaries.

Assistant conversations reuse the existing Planner execution, MCP capability and
approval contract. Card roles, managed session kinds, provider bindings and actors
remain real. Only the Track's current Planner owns scheduling and root identity;
capability sharing does not repoint it or rewrite stored roles. PlainChat and
workers retain their contracts.

The MCP and Truth owners map validated conversation identities to the shared
Planner capability. Discovery consumes that declared capability rather than a
parallel Assistant catalog. The common catalog includes `neige_worker_report`
within the existing 31,262-byte aggregate budget. Shared instructions describe
the available tools; real attempt, session, plugin and Track checks still govern
execution. Retired sessions and foreign Track targets remain refused.

Never/Ask/Full choices are saved atomically with first input. Retry reuses frozen
creation and invocation facts, and Never remains the independent Assistant
default. Planner switching retains the outgoing-Never product choice for that
card's next turn; it cannot alter a frozen invocation or another conversation.
Draft migration 0222 initializes existing Assistant choices without changing any
released migration bytes.

The existing ask producer, answer route, responder table and conversation drawer
serve both roles. Every native answer requires an authenticated nonshare Human
source and the original live card/session ownership. Provider-owned connection
and request revocation is rechecked at the native writer, including delayed
queued replies. Ordinary Wake answers execute only their original conversation.
No parallel approval admission service or transported actor policy is needed.

Activity schema 5 carries explicit `card_id` and a present, nullable `session_id`
on Ask items. The Truth registry validates the complete declared question/detail
and action shapes while rejecting missing required fields and unknown fields.
Its closed action shape is intentional: the event decoder itself is
forward-readable and cannot supply the registry's unknown-field refusal.
The UI opens the original conversation rather than guessing the current Planner.

Compact creation controls retain the chosen approval value and full accessible
name. ModelPill's existing in-menu effort mode keeps Agent and Model on one line
with existing truncation; other hosts retain their wrapping behavior. Turn
budgets and Track transcript-refresh ownership still use the real role/profile,
not the shared capability role.

Verification uses actual production entry points and private fake-provider
stacks. The final gate and mutation contract is described in
[verification](verification-conversation-permissions.md). Runtime logs, revision
comparisons and test counts belong in ignored evidence and Report/PR. The
Planner arranges final independent L2 reviews; passing tests alone do not
constitute overall product acceptance.
