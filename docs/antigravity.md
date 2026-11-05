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

## Available capabilities

| Capability | Behavior |
| --- | --- |
| Planner and Assistant | Available through the existing model picker |
| Concurrent cards | One resident ACP process, separate native sessions and working directories |
| Cancel | Targets one card's native session |
| Neige MCP tools | Card-session authorization; reset or Assistant archive revokes its credential |
| Track soft-close | Pauses the Track and retains the card session |
| Images | Existing upload/attachment controls, in managed workspaces; only this card's bound images are sent |
| Read/write approval requests | Routed through Neige's existing approval flow when AGY requests approval |
| Running-turn steer | Not enabled |
| Manual compaction | Not enabled |
| Canonical input and history replacement | Not enabled |
| Per-card OS sandbox | Not provided by this integration |

Images use standard ACP image blocks and require the agent to negotiate image
support. The existing upload limit is 8 MiB per file, but the complete ACP request
must be **less than 8 MiB after base64 encoding**, including instructions, text,
and all images. Use smaller images or fewer attachments when a message exceeds
that limit. A local capability or size refusal leaves the input editable and
creates no send receipt; remove the queued input and send a smaller message.

Approval is not an OS sandbox. The shared process runs with the service account's
access; AGY's own policy can allow an action without emitting an approval request.
