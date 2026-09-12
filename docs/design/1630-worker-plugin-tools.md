## Outcome
An isolated Codex Worker can use an explicitly delegated set of existing platform-proxied plugin tools while its own shell network stays disabled. A Planner can delegate a bounded research task without copying plugin credentials or broadening the worker sandbox.

## Contract and authority
- Add optional `plugin_tools` (bounded exact fully-qualified tool names, no wildcards) to named Dispatch and its frozen isolated execution selection. Omitted means no plugin grants, preserving historical contracts. A changed set conflicts with same-name replay. Recovery retains the frozen set.
- Planner may delegate only currently running, Track-scoped ordinary plugin tools; execution-backed ForgeAction tools are excluded. A tool's read-only annotation does not grant authority. Existing installation/Track/role admission remains authoritative.
- Pass the frozen names into the private provider's existing `calm.enabled_tools` alongside its four native tools. No new MCP endpoint, secret forwarding, outbound worker network or filesystem privileges.
- Enforce the grant again on the platform's tools/list and tools/call paths, resolving the actual Worker task by authenticated card identity and its frozen task context. Never trust model-supplied task/Track IDs. Current plugin availability and Track scope still apply, so revocation wins over a historical grant.
- Report grants in dispatch/recovery environment information and teach the Planner the explicit parameter. Unknown/unavailable/out-of-scope grants fail before named dispatch creates work; tools added later do not become available implicitly.

## Acceptance
Production-path regressions first red: authorized ordinary plugin appears in Worker discovery, traverses existing proxy, and returns actual fixture data; ungranted/stopped/out-of-scope/other-task/ForgeAction tools are refused without reaching plugin. Provider config contains only base tools plus grants and retains no-network/secret isolation. Named replay/recovery preserve grants and reject silent changes. Mutation-verify load-bearing refusal and isolation assertions.

Use a private checkout/runtime; do not restart 4140. Two independent full-diff reviews, focused tests, quick Rust gates, generated-artifact sweep where required, then PR CI and squash-only integration. Existing real-Codex Tier 2 prohibition on shared production remains; deterministic isolated provider and real MCP/HTTP fixture exercise the process boundary locally. Any real-provider acceptance requires the permitted isolated runner or a dedicated test host.

## Implementation notes
The named Dispatch parameter is `plugin_tools: ["plugin.<id>_<tool>", ...]`.
It is stored in the existing immutable `context.neige_execution` JSON, so no
migration or credentials are needed. Existing Planner/User-authored task-block
context may also explicitly declare this field: that is the existing task
contract authority, not a worker self-grant. Named Dispatch adds an up-front
availability check only for creation; receipt replay remains available after
revocation. A directly authored task may name a currently unavailable tool, but
neither the provider list nor platform call authorizes it until existing live
Track/role/plugin admission permits it.

The effective plugin set is the intersection of frozen names, current ordinary
plugin descriptors, current Track scope, and the authenticated active worker
attempt. Finishing an attempt removes its plugin access even before session
cleanup. Legacy Worker behavior remains unchanged. Model-supplied read-only
annotations confer no extra authority; remote plugins can change semantics,
so an explicit name grant is authority to call that installed tool, not a
sandbox certifying the plugin's business behavior. ForgeAction tools remain
outside isolated grants.

Recovery reuses the same names and original immutable inputs. It does not grant
newly installed tools; disabling a plugin or narrowing scope takes effect on
subsequent calls. Private-home policy digests reject preparing an existing run
with a different grant set. The worker receives the platform MCP session token
through the existing private shim configuration, never connector credentials.
