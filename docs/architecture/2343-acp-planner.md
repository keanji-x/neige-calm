# Managed ACP Planner backend

Tracking: #2343. Related: #2292, #1928.

## Outcome and scope

Add ACP v1 to the existing `provider` crate and run managed OpenCode Planner sessions through the existing Harness. Transport and pure event/configuration translation are reusable; OpenCode identity is a server registration using `opencode acp`.

The first delivery covers explicit text input, native text/tool output, declared model/effort selection, stop, reset, deletion and controlled restart. It does not connect to an independently running OpenCode server, import arbitrary native sessions, observe another client's activity, or implement steer/edit/compaction. Native permission requests follow the card's permission mode (#2348, see Permissions below). Ordinary `neige_user_ask` remains the existing MCP flow. Image input is explicitly refused before dispatch.

Review tier: **L2**. Process authority, durable session identity and submission recovery change. Two independent review channels must check abstraction ownership, duplicate logic and application assumptions, and converge after any behavioral fix.

## Ownership

- `provider::acp` owns bounded stdio JSON-RPC, native protocol types, capability negotiation, session configuration parsing and direct `PlannerEvent` translation. It depends on no server, Axum or database package. ACP updates do not pass through Codex notifications.
- The server owns process configuration/environment, spawn/stop, MCP credential issuance/revocation, thread seals, native binding and durable admission. The shared process primitive matches an exact backend marker and PID start time; Claude retains its existing marker key.
- The existing Harness owns queueing, transcripts and conversation lifecycle. Server construction facts live in `harness::wiring`; historical Claude module paths are direct type re-exports, not parallel contracts.
- OpenCode remains a closed, typed agent identity. This release permits it only for resumable Planner sessions. That boundary is enforced by the new database constraint; it is not an ACP protocol special case. MCP author identity remains the existing Planner session identity.
- Every ACP provider (OpenCode, Grok Build) runs only as a resumable Planner or Assistant session. Migration 0163 pins this in the schema for `opencode` alone; the one `worker_sessions` insert (`session_insert_tx`) refuses any other identity of an ACP provider, so a later provider needs no migration for it.

## Admission and recovery

Before dispatch, persist the original input, native session and kernel correlation under the stable input key. A pipe write is dispatch evidence, not acceptance. A lost/ambiguous response leaves an unresolved durable receipt. A JSON-RPC error answer to the prompt is the agent's definite answer: the turn settles as failed with the agent's error text, and later input is not fenced (#2451). OpenCode 2.1.3 answers that way when its model API refuses a call. A repeated exact key returns the original correlation without sending another prompt; changed input or binding is refused. Fresh input is fenced until explicit reset. An unknown outcome is never reported as success.

A settled session attaches to its original native ID after restart. The session owner selects `session/resume` when the agent advertises `sessionCapabilities.resume` (OpenCode 2.1.3); otherwise it uses declared `loadSession`. Both carry the native ID, cwd and MCP servers. New sessions are created only without a native binding; a failed load or resume never creates a replacement. History replay, command announcements and configuration updates during attachment are setup traffic; they are drained before the fresh turn's translator is installed and cannot establish input delivery, END or maintenance completion. Native IDs and current settings belong to the registered agent. The registration/cwd digest prevents silently switching the backing profile, and the backend refuses adopting a pre-existing native binding without managed ownership.

OpenCode 2.1.3 ACP starts a private `serve --stdio --port 0` child with an inherited launch environment and a stdin lease. The existing credential revoke → exact-marker process-tree stop → retained driver join fences cover this child as well. The host cancellation budget is 5s; upstream may spend 5s draining execution. A late or missing terminal response stays Unknown, even after cleanup succeeds. Resume supplies no native acceptance receipt: durable local receipts and body-free canonical associations remain authoritative, OpenCode retry remains unavailable, prompts are never automatically resent, and steer stays disabled.

The durable receipt is the settlement authority: final outcome and ordered item frames are committed before the Harness completion projection. Recovery and operation replay use one checkpoint loader after reserving the runtime and stopping its predecessor. It reads the entire current snapshot, reconciles the receipt's exact queue claims, and retains later input. Generic harvest paths apply that same retirement rule before moving queue ownership. Text and tool identities occupy disjoint namespaces, and replay retains first-seen item order.

External Harness checkpoint persistence takes the same issuance lock as queue drain; shutdown, which already owns that lock, uses the internal writer. Managed ACP sessions own their driver task and join it before recovery or replacement can continue, even when credential/process cleanup fails. Joining borrows the stored handle, so cancellation of a shutdown cannot detach a remaining receipt writer. No timed wait is treated as quiescence evidence.

The kernel registry retains the predecessor's discoverable live slot throughout shutdown. Replacement reserves only after that shutdown completes and atomically checks instance identity; removal uses the same identity check. Cancellation therefore leaves a predecessor available for the next recovery attempt to join. Removing before shutdown and immediate replacement are restricted to fixture seams.

Receipt projection is reconciled by the storage owner in one transaction. If native item rows are missing or out of order, it replaces only the receipt-owned identities in their declared order, retaining user input and other turns. The ordinary append path shares the same insertion primitive. An exact, ordered projection is left untouched, preserving row identity on repeated recovery.

Process cleanup and boot credential revocation select only sessions registered in `acp_managed_sessions`. The `NEIGE_ACP_PLANNER` namespace and canonical data-dir instance marker exclude independent native clients and other Neige instances. Existing migrations are unchanged; additions are 0156 and 0157 after main's permission-mode migration.

## Configuration

### Boot when process cleanup is unavailable

Review tier: **L2**, because boot crosses the credential and process authority
boundary. ACP boot must first revoke the old MCP credentials of every owned
session in this instance's database, including historical rows when ACP is no
longer configured. A database or revocation error still aborts `AppState::boot`
before the MCP listener opens. Only after that transaction succeeds does boot
attempt the existing session/catalog marker sweep. A sweep error is logged as
a warning and boot continues, matching Claude's existing lifecycle policy.

This restores overall service/HTTP startup on macOS; it does not add native
macOS ACP/OpenCode support. The existing scanner still cannot confirm cleanup
there, so configured ACP remains unavailable. Every readiness, catalog and turn
launch must still propagate `stop(...).await?` before spawning a process.
Track sweeping, deletion and relocation retain their existing error propagation.
No shared scanner, schema, migration, Codex or Claude behavior changes. A failed
scan is never an empty scan, and continuing boot does not establish that old
processes stopped: they may retain non-MCP capabilities despite credential
revocation. Operators must resolve that process cleanup separately.

Regression checks use isolated real boot/HTTP and ACP launch entry points to
cover configured and unconfigured startup, historical token revocation and
instance/provider isolation, revocation failure before listening, and refusal
before agent spawn. The ACP owner compiles `src/acp_planner/boot_tests.rs` only under
`cfg(all(test, unix))`. Its file-level `#![cfg(test)]` also prevents production
compilation. The production turn-call scan recognizes that Rust-enforced
file-level condition; the extractor scan still covers the test source. The `acp_boot_`
filter is unchanged; adding or renaming a test needs no central list. The worker
runs focused red/green and safety mutations; the gate owns quick/text/contract
checks and one intentional focused replay. Broad suites remain CI's job.

Pass `--acp-planner-config /absolute/path/acp-planner.json`:

```json
{
  "agents": [{
    "provider": "opencode",
    "command": "/absolute/path/opencode",
    "args": [],
    "env": {
      "HOME": "/private/opencode-profile",
      "XDG_CONFIG_HOME": "/private/opencode-profile/config",
      "XDG_DATA_HOME": "/private/opencode-profile/data",
      "XDG_CACHE_HOME": "/private/opencode-profile/cache"
    },
    "min_agent_version": "2.1.3"
  }]
}
```

An ACP registration may optionally declare `"default_permission_mode": "never"`, `"ask"`,
or `"full"`. The field uses the same strict string parser as `PlannerPermissionMode`;
invalid values (including explicit `null`) are refused when reading the file. Omission
means `never`. The shared Planner creation owner resolves the selected provider's registration
for ordinary Tracks (including `neige_track_add`), Human dormant creations, scheduler child
Tracks, and Today/daily planners. Other providers still start at `never`; Assistant turns always use
`never`. This creation policy does not change existing cards or native-session digests.
After creation, only `PUT /api/cards/{id}/planner/permission-mode` changes the stored mode.

Use an operator-owned private profile and authenticate through the native CLI outside Neige. All launch fields are required and unknown keys are refused. The child inherits no ambient environment. Kernel PATH and marker/credential keys cannot be overridden by registration.

The argument vector is the registration's `args`, then the provider profile's ACP entry with the
agent options of the turn's permission mode inside it: the entry's arguments before the options,
the options, then its arguments after them. Where an agent CLI accepts options is that CLI's own
fact, so its profile states it and the operator does not; `args` only prefix the entry, such as a
wrapper's or interpreter's own arguments, and never hold the ACP subcommand. OpenCode's entry is
`acp` with no options, so it always launches as `<command> <args> acp`; a Grok-shaped entry is
`agent <options> --no-leader stdio`.

Each provider's ACP profile (`acp_planner/profile/`) fixes where the agent's `initialize` answer
reports who it is: ACP's `agentInfo` with the profile's name (OpenCode: `OpenCode`), or only a
version at `_meta.agentVersion`. The reported version must be semver and at least
`min_agent_version`, itself semver; anything else stops the process before a session operation.
Precedence is semver's: build metadata never ranks, and a pre-release ranks below its own
release only, so with minimum `2.1.3` an agent reporting `2.1.4-beta.1` is accepted and one
reporting `2.1.3-beta.1` is not.
A file still naming `expected_agent_name` (now fixed by the profile) or `expected_agent_version`
(renamed `min_agent_version`) fails to parse, and the error adds that change and that `args` no
longer holds the ACP subcommand; the two forms are never read together.

### Grok Build

Grok Build (`provider: "grok"`, tested with 1.0.62) serves ACP as `grok agent --no-leader stdio`;
`--no-leader` starts the process's own agent rather than joining a shared leader. Its `initialize`
reports no `agentInfo`, only `_meta.agentVersion`, and every launch authenticates with
`cached_token`: the sign-in `grok login` stored under the registration's `GROK_HOME`. Point
`command` at a versioned binary, not the `~/.grok/bin` link its updater rewrites, and turn the
updater off:

```json
{
  "agents": [{
    "provider": "grok",
    "command": "/opt/neige/grok/grok-1.0.62-linux-x86_64",
    "args": [],
    "env": {
      "HOME": "/var/lib/neige/grok-home",
      "GROK_HOME": "/var/lib/neige/grok-home/.grok",
      "GROK_DISABLE_AUTOUPDATER": "1"
    },
    "min_agent_version": "1.0.62"
  }]
}
```

Sign in once with `GROK_HOME=/var/lib/neige/grok-home/.grok grok login --device-auth` (add the
same proxy variables the registration declares, if any). Neige runs Grok on that subscription
only: a registration setting `XAI_API_KEY` or `GROK_CODE_XAI_API_KEY` is refused when the file is
read.

Grok also loads MCP servers a project defines (`.grok/config.toml`, `.mcp.json`,
`.cursor/mcp.json`) and the operator's own in the Grok home's `config.toml`; Neige neither pins nor
checks that set. None of them can take the key Neige's server is registered under, which is fresh
for every launch (see [Permissions](#permissions)), so a request for one of their tools is never
one of Neige's own: it is answered as the mode answers any request.

Grok's `--allow`, `--deny` and `--permission-mode` belong to its TUI and headless mode; a stdio
agent does not apply them (1.0.62 still asked before a call they allowed or denied). It does apply
`--always-approve`, an agent option. Each turn's process is launched under that turn's mode, so a
change of mode holds from the next turn on.

- Under `never` nothing is added. Grok asks before edits, shell commands that write and MCP tool
  calls, and every request but one for Neige's own tools is answered `cancelled`. Grok then ends
  the whole turn (see [Permissions](#permissions)), so a Grok Planner under `never` ends its turn at its first such shell
  command (`neige …` included) or write, saying which request was declined. Switch the card's
  permission mode to `ask` to approve each such request, or to `full` to allow them. A sandboxed
  `never` that fails closed is follow-up #2942.
- Under `ask` nothing is added: Grok's own default. A shell command Grok's own classifier reads as
  read-only runs without asking and with no sandbox, unlike Codex's `ask`, which runs every command
  in its sandbox.
- Under `full` the process runs with `--always-approve`: no sandbox and no asks, the account's own
  access, for every MCP server Grok loaded too.

Grok's `_x.ai/*` notifications are ignored. Its `promptCapabilities.image` is false and its
`sendNow` reruns the turn rather than joining it, so it declares neither images nor steer.

Each agent process's stderr is kept in `<data-dir>/acp-agent-logs/<worker>.stderr.log`
(owner-only, its first 256 KiB, overwritten by the worker's next launch). Neige writes nothing
there; what Grok prints is its own diagnostics, and the card token the process held is revoked when
its turn ends. A turn whose outcome is unknown also logs the error that made it so.

A conversation bound to a native session is claimed under a digest of what that session depends
on: the provider, `command`, `args`, `env` and the working directory. Changing one of them makes
the conversation refuse to resume until it is reset explicitly. `default_permission_mode` is creation policy and is excluded from the digest. `min_agent_version` is admission
policy checked on every launch and is not part of the digest, so raising it resets nothing. A new
registration field is placed in or out of the digest explicitly where the digest is built.

A profile may name the `authMethods` id every launch authenticates with after `initialize` and
before any session operation. OpenCode's names none and sends no `authenticate`. When one is
named, it must be among the methods the agent reports, and `authenticate` must succeed within its
bound; otherwise the process stops and the launch fails. Readiness takes this same path, so the
provider reads unavailable with the advice to sign the agent in with its own CLI under the
environment its registration declares. Neige judges sign-in only from the agent's ACP answers,
never from its private credential files.

Operational launches explicitly receive their active Planner card's
`NEIGE_MCP_SOCKET` and `NEIGE_MCP_TOKEN`, using the same per-turn context as the
MCP shim (#2413). This enables the kernel-served `neige` CLI without inheriting
user or daemon credentials. Readiness launches receive neither. Managed ownership
and active-carrier checks precede token issuance; launch/negotiation failures
revoke it, as do setup failure, settlement and shutdown; an authentication
failure is a negotiation failure. The operator-registered
native binary is trusted to hold the same card credential already delivered in
its MCP descriptors; the advertised agent identity and version remain a compatibility
check, not cryptographic authentication of the executable, and ACP `authenticate`
signs the agent into its own service, not into Neige. CLI commands still
use the authoritative kernel parser and role checks; native permission requests
follow the card's permission mode.

Model configuration ids and opaque values come from declared ACP `configOptions`, using standardized `model` and `thought_level` categories. Model queries never ask Codex for an ACP card. Before managed setup, a catalog query discovers the registered agent's settings through an empty session. Managed session configuration takes precedence once available. A null choice keeps current native session settings. Unknown choices are judged during fresh setup before prompt dispatch, without silently substituting another model or effort.

Native MCP consumers can build model-visible output from `content` alone (#2401).
ACP sessions therefore launch the stdio shim with the explicit
`--structured-content-as-text` presentation mode. For a successful, correlated
`tools/call` reply, it adds a JSON text block containing `structuredContent` when
that exact data is not already present as text. Original summaries, warnings,
images and the structured result remain intact. Other responses, tool errors,
unsolicited frames and default shim clients retain their existing wire format.
The adaptation belongs to the MCP transport; report and terminal handlers keep
their authoritative result contract. Authentication and native permissions are
unchanged.

## Permissions

`session/request_permission` follows the permission mode the harness resolved when it issued the
turn (#2348); `provider::acp::approvals` owns the mapping. Under `never` and `full` (#2441) every
request is answered `cancelled` where it is read. Under `ask` each request is a `hold` ask: one question whose title is
the tool call's kind as a plain word (`execute` reads "Run", `edit` "Edit", `fetch` "Fetch"; `other` and an
unknown kind show no word, never the raw kind), its title and its files, and whose options are the agent's option
names in its order (#2452).
The chosen option is answered `selected` with that option's id; a request withdrawn or never asked
is answered `cancelled`. The process is the held-request connection. The turn ends its requests by
a fence, under the lock every answer takes: a stop sends `session/cancel` and then `cancelled` for
every pending request, and the end of the driver's read (the prompt settled, the process exited, a
protocol error) answers every pending request `cancelled` before any teardown and before the harness
is told the connection is lost. After the fence an answer writes `cancelled` or nothing, so no
late answer authorizes work after the turn ended. Requests during
session setup are always answered `cancelled`.

An agent may also ask before calling a tool of Neige's own MCP server. Grok Build does under
`never` and `ask` (under `full` its `--always-approve` asks nothing), with `use_tool`: kind
`other`, titled with the tool's qualified name `<server>__<tool>`, which its `rawInput.tool_name`
repeats. That server authorizes every call itself by the card's token, so under every mode such a
request is answered with the agent's `allow_once` option without asking, until the turn's fence. The agent's profile declares how it asks (kind, separator and input
field). Each launch of such an agent then registers Neige's server under a key of its own,
`neige-` and 12 random hex digits, never holding `_`, and only the exact qualified names of the
tools that server lists the session match: the MCP layer resolves the listing as the session's own
connection's `tools/list` does. A server whose key extends Neige's (`neige-…__evil`), the same
tool under another server, and a tool the session is not listed are each answered as the mode
answers any request. OpenCode, which does not ask before these tools, declares no form: its
server keeps the shared key `neige`, and its requests are answered as before.

Grok ends its turn `cancelled` on any answer but an allow, `reject_once` included. Its profile says
so, and a turn it ends that way after the turn declined one of its requests fails with the reason,
such as ``The agent ended the turn because the person declined `…`.`` or ``… because Neige declined
`…` under the `never` permission mode. Switch the permission mode to `ask` to approve such a
request, or to `full` to allow it.``, rather than showing as interrupted. A stop the person sends
still ends the turn interrupted.

The OpenCode profile supports `>=2.1.3,<3.0.0`, independently of the operator's
`min_agent_version`. Install `@opencode/cli`, not the retired 1.x package.
Each turn composes native `OPENCODE_CONFIG_CONTENT` with
`permissions: [{action, resource, effect}]`. Operator registrations setting that
variable are refused rather than overwritten. The registration's other explicit
environment entries remain intact.

`never` adds only a `question` denial and otherwise retains native policy; Neige
answers any permission request `cancelled` without asking a person. `ask` adds
`ask` rules for `shell`, `edit` (also write and patch), `webfetch`, and
`external_directory`. `full` allows the native acting, reading, searching,
subagent, skill, todo and MCP-resource actions without asking. All modes deny
native `question`, whose form/elicitation flow Neige does not implement. The
removed 1.x `bash`, `doom_loop` and `plan_*` policy assumptions are not used.

These are native defaults, not an isolation boundary against operator-authored
OpenCode configuration. [Last matching rule wins][permission-evaluate];
[global rules precede agent-specific rules][permission-agent]. Session rules
follow agent rules, and [saved approvals follow configured non-denials][permission-saved], so an
agent-specific allow or a saved approval can bypass an `ask`. A configured deny
survives saved approvals. An operator agent rule can also override the global
`question` denial. ACP additional-directory grants are session rules; Neige
passes no additional directories. Changing Neige's mode does not erase native
saved approvals. Unlike the old process-local assumption, 2.x `always` answers
can persist project approvals. A `full` turn still refuses stray ACP permission
requests, and cancellation continues to fence late answers.

The native action inventory comes from v2.1.3 `packages/core/src/tool/plugin/`:
`shell`, `edit`, `read`, `grep`, `glob`, `webfetch`, `websearch`, `subagent`,
`skill`, `todowrite`, `opencode_list_mcp_resources`,
`opencode_read_mcp_resource`, plus `external_directory` from `file-access.ts`
and `question`. Future native actions require assessment in the owning profile;
no generic ACP layer classifies native tool names.

[permission-evaluate]: https://github.com/anomalyco/opencode/blob/v2.1.3/packages/core/src/permission.ts#L74-L87
[permission-saved]: https://github.com/anomalyco/opencode/blob/v2.1.3/packages/core/src/permission.ts#L135-L165
[permission-agent]: https://github.com/anomalyco/opencode/blob/v2.1.3/packages/core/src/config/plugin/agent.ts#L82-L123


## Acceptance

### Model discovery before the first input

Review tier: **L2**, because discovery extends the managed ACP process lifecycle.
The model endpoint discovers a registered agent's catalog through an empty ACP
session before a card or first input exists. It uses the existing explicit process
environment and identity checks, supplies no MCP servers, issues no prompt, and
never binds the discovery session to a Neige card or durable submission. The ACP
owner serializes discovery and caches one workspace's result for 30 seconds,
including failures; existing real-session configuration remains authoritative.
The process is stopped on success and failure, with `session/close` first when
advertised. ACP processes use the existing owned process-group guard, which
sweeps inherited descendants if setup or cleanup is cancelled, without scanning
`/proc` on a runtime worker. Boot also sweeps the probe marker. Descendants that
deliberately leave the process group rely on the existing exact-marker sweeps.
ACP does not standardize deleting native session metadata: the agent
may retain an empty discovery session. Real setup validates all selections again.
Acceptance covers provider and pre-input card queries, cache reuse, workspace
separation, failure cleanup, and a selected model on the first production turn.

Use deterministic stdio peers through the production transport and real boot/REST/Harness entry points. Check negotiation, bounded/malformed frames, correlated requests, cancellation, two turns, text/tool output, MCP authentication, never permissions without Ask creation, held permission answers, cancel and exit withdrawal, exact-key recovery, unknown-outcome fencing, same-session restart, lifecycle cleanup and unsupported-control refusal. Tests must wait for actual production decisions, not merely sleep before asserting zero writes.

Mutation-verify the small set of unique safety assertions in an exclusive recoverable worktree, predict the complete red set, restore exact production bytes and prove green. Run focused provider/server/compatibility tests, text gates, contract gates, quick Rust preflight and frontend/browser checks. Generate real wire/OpenAPI artifacts after schema changes. Real Codex E2E is prohibited on the shared host. Live OpenCode acceptance is separate from deterministic peer coverage and must not be claimed unless run.

## Risk and rollback

ACP v1 does not provide native exactly-once execution or authoritative admission recovery. Keeping an unresolved submission fenced trades automatic recovery for avoiding repeated operational commands. Native optional capabilities are not assumed.

Disable the ACP registration to stop new managed turns. Do not roll an upgraded database back into an older binary that cannot decode OpenCode identity or knows fewer migrations; use a pre-upgrade backup after quiescing this instance if binary rollback is necessary. Restoring source alone is not a database rollback.
