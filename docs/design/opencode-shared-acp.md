# OpenCode shared ACP Beta

Use one Neige-owned OpenCode ACP process per configured OpenCode backend.
Use the same ACP process and session across turns, including when a user changes
their approval mode. Replace the per-turn OpenCode launch for Planner and Assistant.
Retain existing native session IDs, working directories, and input receipts.
Review tier L2 covers tool authority, provider execution, and recovery.

## Verified upstream behavior

A local OpenCode 2.1.4 probe opened two ACP sessions on one connection and
resumed the second after closing the first. A separate authenticated native
API probe created two sessions in the same directory, set A's permissions
with session.update, observed that B's permissions were unchanged, and cleared
A's rules. These probes covered session lifecycle and rule storage.

A third probe used a controlled local model to issue real shell tool calls.
With a global shell ask rule, one ACP process accepted allow_once in A
and cancelled in B. A subsequent refusal in A left its test file absent.
Selecting reject_once likewise left the file absent. When A selected
allow_always for a command, B ran the same command in the same project
without an ACP permission request. A separate project agent's shell allow
rule also bypassed the global ask rule. Therefore client-side answers can
control requests that arrive, while a fixed global ask rule alone cannot
guarantee mediation. A concurrent probe held both sessions' requests at once,
then approved A and rejected B. Only A created its file. OpenCode 2.3.0
still evaluates configured denials,
saved approvals and permission hooks before publishing an ask.

- [OpenCode 2.3.0 permission evaluation](https://github.com/anomalyco/opencode/blob/v2.3.0/packages/core/src/permission.ts)
- [ACP tool permission outcomes](https://agentclientprotocol.com/protocol/v1/tool-calls#requesting-permission)

A fourth probe used OpenCode Code Mode and a temporary stdio MCP server.
Two same-directory sessions invoked the first session's server, which
received the actual caller in `params._meta["ai.opencode/sessionID"]`.
Manual compact on the same connection produced matching in-progress
and completed notifications for the requested session.

Run OpenCode under one fixed launch policy that asks for the same actions as
the previous Ask mode. Neige applies Never, Ask or Full to the standard ACP
permission requests OpenCode sends for that turn. Full selects allow_once.
Projects and remembered approvals retain their native OpenCode scope and
precedence. Switching the Neige approval mode leaves the process and session
identity intact. OpenCode Beta uses Neige's MCP tools. The shared process and
its shells carry no card credentials for direct use of the Neige CLI.

- [OpenCode 2.3.0 ACP contract](https://github.com/anomalyco/opencode/blob/v2.3.0/services/site/src/docs/content/cli/acp.mdx)
- [OpenCode 2.3.0 session protocol](https://github.com/anomalyco/opencode/blob/v2.3.0/packages/protocol/src/groups/session.ts)
- [OpenCode server plugin session API](https://opencode.ai/docs/build/plugins/server/#permissions)
- [Related idle and wake design](https://github.com/keanji-x/neige-calm/issues/3600)

## Implementation boundaries

- Reuse the resident ACP owner for startup, validated attachment, routed
  updates, exact detach, and boot recovery. Key the owner by registered
  backend. Keep approval modes in Neige's existing per-turn admission state.
  Apply the current mode only to requests routed to that turn.
- OpenCode registers MCP servers at directory scope. Keep authorization tied
  to the invoking native session with a trusted OpenCode-provided origin.
  OpenCode includes that origin in MCP tool-call metadata for direct and
  Code Mode calls. Treat the origin as correlation and require authentication
  and an exact current Neige binding as well. Calls from native child sessions
  lack a corresponding Neige binding and are refused. Closing an attachment
  revokes its token. A separate attachment of the same Neige worker uses the
  same server name and a new token, replacing the old directory registration.
  See [upstream tool-call metadata](https://github.com/anomalyco/opencode/blob/v2.3.0/packages/core/src/mcp/client.ts).
- Negotiate ACP compaction on the resident connection. Confirm that the
  original session emitted the matching compaction lifecycle events and that
  its compact prompt ended successfully. Retain the compact operation's
  separation from chat input and do not replay an uncertain submission.

## Acceptance

Use a real supported OpenCode release and a controlled model endpoint. Open
two cards in the same directory with different approval settings. Verify the
same process serves both cards, an approval change applies only to the intended
session and its subsequent work, and both cards can use their own MCP tools.
Prove mismatched calls are refused and that detach, token replacement, and
child tool calls respect owner identity. Exercise
compact while the other card is active, session cancellation, process loss,
Neige restart, and continuing conversations created by the per-turn launcher.
Preserve unknown-outcome fencing and prove no original prompt is resubmitted.
Mark the replacement Beta at the OpenCode choice.
