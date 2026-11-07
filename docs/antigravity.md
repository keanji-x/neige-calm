# Antigravity ACP

Neige supports Antigravity Planner and Assistant conversations through Google's
standalone `antigravity-acp` service from the
[official ACP registry](https://cdn.agentclientprotocol.com/registry/v1/latest/registry.json).
The supported release line is 1.x, tested from 1.4.5. This is a separate program
from the Antigravity CLI's `agy acp` command.

## Configure once

Install the standalone service for your platform using the official ACP registry.
Use an ACP client to authenticate that executable as the operating-system account
that runs Neige. The service owns its authentication settings and credentials;
Neige does not copy the CLI's credentials or select an OAuth/API-key method.
An unconfigured service reports `Authentication required`. If OAuth is configured
but its cached credentials are unusable, the service may initiate its own sign-in
flow during readiness checks. Complete sign-in before enabling it on a headless host.

Pass `--acp-planner-config /absolute/path/acp.json` when starting Neige. The
registration supplies the executable and an explicit child environment. Neige
starts the standalone executable directly, without an `acp` argument:

```json
{
  "agents": [{
    "provider": "antigravity",
    "command": "/absolute/path/agy_acp_server.par",
    "args": [],
    "env": {"HOME": "/home/neige"},
    "min_agent_version": "1.4.5"
  }]
}
```

Use the service account's actual home directory. Add proxy or authentication
settings explicitly when required by the chosen native authentication method.
Neige checks the reported `antigravity-acp` identity, supported release line and
readiness before starting new conversations. The existing model picker selects
Antigravity; no separate conversation UI is needed.

## Upgrading from the CLI backend

Replace the registration's CLI executable and minimum version with the standalone
service configuration above, authenticate the standalone service, then create new
Antigravity conversations. Existing CLI conversations and history are retained,
but their native sessions and credentials are not migrated. A changed registration
cannot silently resume or replace an existing native session. The old CLI identity
is refused; Neige does not fall back to launching `agy acp`.

## Available capabilities

| Capability | Behavior |
| --- | --- |
| Planner and Assistant | Existing canonical input, submission receipts and retained recovery |
| Context usage | Standard ACP `usage_update.used/size` feeds the existing context ring |
| Concurrent cards | Separate managed processes; each turn resumes its card's native session |
| Cancel | Stops the selected turn without interrupting another card |
| Neige MCP tools | Credentials belong to the current turn and are revoked when it settles |
| Images | Existing bound uploads in managed workspaces; negotiated ACP image blocks |
| Model and thinking choices | Native configuration options, confirmed by the service |
| Read/write approval requests | Existing Neige approval flow when the service asks |
| Running-turn steer | Not enabled |
| Manual compaction | Not enabled: standalone 1.4.5 does not declare the CLI compact command |
| History replacement | Not enabled |
| Per-card OS sandbox | Not provided by this integration |

The context ring uses the service's current occupancy and model capacity. It does
not use cumulative token consumption or a Neige model-capacity lookup table.
Readings may decrease after native context management; an absent reading remains
unknown. Usage visibility after a context replacement follows the existing
invalidation rules, and persisted readings survive Neige restart.

The existing upload limit is 8 MiB per file, while the complete encoded ACP
request must be less than 8 MiB, including instructions, text and base64 images.
A capability or size refusal leaves canonical input editable and creates no ACP
send receipt. Use smaller images or fewer attachments if the complete request is
too large.

## Permission modes

`full` selects the native single-call `allow_once` option for requests in the
current turn. `ask` routes requests through Neige's existing approval flow;
`never` declines them. No process-wide permission bypass is enabled. A stopped or
settled turn cannot approve a later request, and ambiguous allow-once options are
refused. Native policy may allow actions without issuing a permission request;
these modes do not add an OS sandbox or expand Neige MCP authorization.

A denied tool followed by a silent stop is reported as a failure. A later
successful recovery or explanatory reply retains its existing interpretation.
Explicit user cancellation remains interrupted. Unknown outcomes stay held and
are not automatically resent, including after a restart.
