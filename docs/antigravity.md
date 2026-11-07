# Antigravity ACP

Neige supports Antigravity as a Planner and Assistant through the official
[`agy acp`](https://antigravity.google/docs/cli/acp/) entry point. The supported
release line is AGY 1.x from 1.5.1; native integration checks use 1.5.3.
AGY 1.5.0 is refused because 1.5.1 fixes native sandbox credential exposure,
cancellation across prompts, and updates arriving after prompt completion. See the
[1.5.1 release notes](https://antigravity.google/docs/changelog/#v1-5-1).

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
    "min_agent_version": "1.5.1"
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
| Concurrent cards | Each turn owns its process, native session, working directory and credential |
| Cancel | Targets one card's native session |
| Neige MCP tools | Card authorization with a fresh credential per execution; settlement or archive revokes it |
| Track soft-close | Pauses the Track and retains the card session |
| Images | Existing upload/attachment controls, in managed workspaces; only this card's bound images are sent |
| Read/write approval requests | Sandbox work proceeds normally; Planner `ask` asks for additional authority, Assistant `never` declines it |
| Running-turn steer | Not enabled |
| Manual compaction | Existing `/compact` command for canonical Planner and Assistant conversations |
| Canonical input | New Planner and Assistant conversations use canonical messages, original submission receipts and retained execution recovery |
| History replacement | Not enabled |
| Terminal sandbox | AGY native sandbox for `never`/`ask`; disabled for `full` |

Images use standard ACP image blocks and require the agent to negotiate image
support. The existing upload limit is 8 MiB per file, but the complete ACP request
must be **less than 8 MiB after base64 encoding**, including instructions, text,
and all images. Use smaller images or fewer attachments when a message exceeds
that limit. A local capability or size refusal leaves the input editable and
creates no ACP send receipt; canonical input remains editable so the person can remove it and send a smaller message.

## Permission modes and early stops

Planner cards expose permission selection. Assistant conversations keep `never`.
The AGY profile bridges three separate native controls: the terminal sandbox,
`toolPermission`, and the advertised ACP `mode` option. Neige selects and checks
`accept-edits` through `session/set_config_option`, so workspace edits do not
need additional authority; this mode alone does not authorize shell commands.

| Neige mode | Native terminal policy | Additional permission requests |
| --- | --- | --- |
| `never` | Sandbox enabled, `proceed-in-sandbox` | Declined |
| `ask` | Sandbox enabled, `proceed-in-sandbox` | Existing Neige approval flow |
| `full` | Sandbox disabled, `request-review` | Current turn's unambiguous `allow_once` |

The terminal sandbox must be available on the host. AGY documents that
`proceed-in-sandbox` falls back to review when it is unavailable; Neige does not
report an unverified sandbox status or automatically bypass the resulting request.
Full never uses a process-wide skip-permissions flag. Stopped or settled turns
cannot approve later requests.

Native settings are prepared under the registration's explicit absolute `HOME`,
in documented CLI settings subdirectories `neige-sandboxed-v1` and
`neige-unrestricted-v1`. Authentication and native history stay with the account;
no private credential or history files are copied. Neige leaves the original
`settings.json` unchanged. Derived settings pin the native execution policy. Nonempty custom permission
rules in the operator CLI settings are refused before a user prompt: real ACP
probes of 1.5.0 and 1.5.3 did not enforce a copied `deny command(*)` in an isolated
CLI profile. This bridge does not claim support for those native custom rules or report that
Project/Shared native permission scopes have been inspected.
Registration arguments may not override profile-owned
settings paths or permission flags.

Each turn uses Neige's existing managed execution owner. Before continuation,
previous execution and credential cleanup must be confirmed. A new process
resumes the same native session with fresh MCP configuration. Permission changes
never change the durable registration identity, resend an original input, or
bypass an uncertain prior execution.

If a denied tool fails and AGY ends without replying or recovering through a
successful tool, Neige shows the denial in the current response status
independently of transcript pagination, even when earlier tools succeeded. An explicit user interruption remains interrupted.
These modes govern requests AGY emits; they do not change Neige MCP authorization
or grant ordinary cards cross-workspace report access.

## Compact a conversation

Use `/compact` in the existing command menu after a conversation has history and
its current turn has finished. Neige uses canonical maintenance admission and
retains the native session; it adds no Human message, input receipt or assistant
reply. Other cards continue in their own executions.

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
resent. Cleanup revokes the maintenance credential and stops its exact execution. Successful
compaction preserves the native session identity; the next turn receives fresh authorization.

Running-turn steer remains unavailable: AGY 1.5.0 returns method-not-found for
`session/steer`, and overlapping `session/prompt` calls do not provide Neige's
required acknowledgement bound to the running turn. Cancellation followed by a
new prompt is not exposed as steer.
