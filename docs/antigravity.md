# Antigravity ACP

Neige supports Antigravity as a Planner and Assistant through the official
[`agy acp`](https://antigravity.google/docs/cli/acp/) entry point. The supported
release line is AGY 1.x, tested from 1.5.0.

## Configure once

Install the official CLI and run `agy acp --login` as the account that runs
Neige. The CLI reuses that account's cached sign-in after restart.

Pass `--acp-planner-config /absolute/path/acp.json` when starting Neige. The
registration supplies the executable and an explicit child environment; Neige
adds the `acp` argument. For example:

```json
{
  "agents": [{
    "provider": "antigravity",
    "command": "/absolute/path/agy",
    "args": [],
    "env": {"HOME": "/home/neige"},
    "min_agent_version": "1.5.0"
  }]
}
```

Use the service account's actual home directory. Add proxy settings explicitly
if the service needs them. Sign-in and network readiness are checked before
new conversations start. Once ready, select Antigravity in the existing model
picker. Upgrading Neige does not install or sign in to AGY automatically.

For sandboxed commands to proceed without an approval request, configure the
service account's [AGY CLI settings](https://antigravity.google/docs/sandbox?tab=cli)
in `~/.gemini/antigravity-cli/settings.json`. Merge these values into any
existing settings; this file also affects other AGY CLI uses of that account:

```json
{
  "agentMode": "",
  "enableTerminalSandbox": true,
  "toolPermission": "proceed-in-sandbox",
  "allowNonWorkspaceAccess": false
}
```

Neige launches its resident ACP server with `--sandbox`; the settings above
also select how AGY handles sandboxed commands and access outside a workspace.
Review the account's AGY permission rules as well: allow rules can change which
operations require approval. Restart the resident server after changing its
settings; that ends any background commands owned by the old server.

## Available capabilities

| Capability | Behavior |
| --- | --- |
| Planner and Assistant | Available through the existing model picker |
| Concurrent cards | One resident ACP process, separate native sessions and working directories |
| Cancel | Targets one card's native session |
| Neige MCP tools | Card-session authorization; ending the session or archiving the Assistant revokes its credential |
| Track soft-close | Pauses the Track and retains the card session |
| Images | Existing upload/attachment controls, in managed workspaces; only this card's bound images are sent |
| Read/write approval requests | Routed through Neige's existing approval flow when AGY requests approval |
| Running-turn steer | Not enabled |
| Manual compaction | Existing `/compact` command for canonical Planner and Assistant conversations |
| Context usage | Existing context ring, with optional supplementary readings from AGY 1.5.3 onward |
| Canonical input | New Planner and Assistant conversations use canonical messages, original submission receipts and retained execution recovery |
| History replacement | Not enabled |
| Terminal sandbox | AGY's shared terminal-sandbox policy; availability depends on the host |

Images use standard ACP image blocks and require the agent to negotiate image
support. The existing upload limit is 8 MiB per file, but the complete ACP request
must be **less than 8 MiB after base64 encoding**, including instructions, text,
and all images. Use smaller images or fewer attachments when a message exceeds
that limit. A local capability or size refusal leaves the input editable and
creates no ACP send receipt; canonical input remains editable so the person can remove it and send a smaller message.

The terminal sandbox applies to AGY's commands, not every file or MCP operation.
The shared process runs with the service account's access, and AGY's own policy
can allow an action without emitting an approval request. This integration does
not provide a Neige-enforced per-card OS isolation boundary. If the host cannot
provide AGY's sandbox, AGY documents that `proceed-in-sandbox` behaves like
`request-review`: in Never mode those requests are declined. Verify sandbox
availability on the host rather than inferring it from the launch flag.

## Permission modes and early stops

`full` selects the agent's single-call `allow_once` option for each permission
request in that card's current turn. `ask` shows the request in Neige's existing
approval flow and returns the option the person chooses. `never` declines requests.
The resident AGY process does not enable a process-wide permission bypass. A
stopped or settled turn cannot approve a
later request. Full requests without an unambiguous `allow_once` option are refused.

If a denied tool fails and AGY ends without replying or recovering through a
successful tool, Neige shows the denial as the turn's failure reason, even when
earlier tools succeeded. An explicit user interruption remains interrupted.
These modes govern requests AGY emits; they do not change Neige MCP authorization
or grant ordinary cards cross-workspace report access. AGY may execute sandboxed
commands and workspace edits without issuing a request even in Ask mode; Full
can approve a sandbox bypass only when AGY requests one.

## Context usage

When the native ACP stream reports context occupancy, Neige uses it directly.
AGY 1.5.3 reports per-turn token consumption but does not send that context
notification in the tested sessions. For normally completed model turns, Neige
can read the same CLI conversation through the documented
[`statusLine` callback](https://antigravity.google/docs/cli/statusline/).
This uses the confirmed model and effort, adds no prompt, and feeds the existing
context ring and its percentage policy. It does not sum billing tokens or guess
model capacities.

The reader is enabled from AGY 1.5.3 in the supported 1.x line; older agents keep
their existing conversation support. No separate server, account or session
migration is required. The original CLI native session identifier, registration
fingerprint and MCP authorization stay unchanged.

Each normal model turn triggers at most one bounded read, with one query at a
time per host. Its six-second collection budget includes waiting for that slot.
An unconfirmed spawn or exit retains its turn fence until cleanup is confirmed.
The query uses temporary settings, disables its own automatic updater and logs,
and receives no Neige MCP grant. Query failure does not fail a completed model
turn; without a new reading, the previous valid reading remains unchanged.

Compaction does not trigger a supplementary read: AGY can still report the old
occupancy immediately after compacting. Neige keeps its existing usage-not-updated
state until a later model turn supplies a valid reading.

## Compact a conversation

Use `/compact` in the existing command menu after a conversation has history and
its current turn has finished. Neige uses canonical maintenance admission and
retains the native session; it adds no Human message, input receipt or assistant
reply. Other cards can continue using the same resident ACP server.

The AGY profile uses the advertised native `compact` command. AGY documents that
[bare `/compact` summarizes the conversation and ends the turn](https://antigravity.google/docs/changelog/),
and its [ACP entry preserves CLI behavior](https://antigravity.google/docs/ide/extensions/acp).
AGY 1.5.0 returns the matching `session/prompt` result with `stopReason: end_turn`
but does not publish standard ACP compaction lifecycle events. That correlated
native command result confirms completion; Neige does not invent a checkpoint,
summary or token-reduction measurement. Standard ACP agents retain their existing
lifecycle-event requirement.

Errors, cancellation, missing results and overflowing session traffic cannot
confirm compaction. An uncertain operation is retained and never automatically
resent. Cleanup closes and revokes only the affected native attachment. Successful
compaction preserves its session authorization.

Running-turn steer remains unavailable: AGY 1.5.0 returns method-not-found for
`session/steer`, and overlapping `session/prompt` calls do not provide Neige's
required acknowledgement bound to the running turn. Cancellation followed by a
new prompt is not exposed as steer.
