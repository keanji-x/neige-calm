# Dedicated Codex controller for one task

This module owns one provider endpoint for a caller-authorized task attempt.
The current scope is a single-task execution loop; artifact publication, dependency
materialization and multi-task delivery are separate follow-ups.
It reuses `CodexAppServer` WebSocket-over-UDS framing and `calm-worker-runtime`.
There is no task database writer, transcript recorder, artifact implementation or
interactive resume viewer here. Runtime helper and server must come from the same
build; build the helper explicitly before this module's integration tests.

`ControllerConfig` requires a private provider root, a separate runtime state root,
the installed Codex executable, its required matching `code_mode_host_binary` companion,
and REQUIRED `sandbox_bwrap` executable (inner Codex sandbox),
existing MCP shim executable, explicit provider
transport environment, and bounded connect/request timeouts. The Codex executable
is mounted as `/provider-bin/codex` plus a fixed set of same-binary helper aliases.
The companion is mounted read-only at `/provider-bin/codex-code-mode-host`; real model tool
execution requires it even when direct MCP calls work without it. Only these explicit executable
files, the inner bwrap and MCP shim are mounted; no host
binary directory is exposed. `sandbox_bwrap` must support `--argv0`, `--perms` and the
required namespace/read-bind flags; a bounded env-cleared `--help` check rejects unsupported
helpers before credentials are published. Use the existing Codex package's
`codex-resources/bwrap` when its flags are supported. The runtime's outer `bwrap` remains
a separate required path. Fixed command PATH selects `/provider-bin/bwrap` first. The provider gets only its private home, private control directory,
the particular native MCP socket, prepared workspace, DNS/TLS files and any existing
system requirements files. Caller must keep these sources under trusted ownership. Canonical private-root overlap
with the provider binary directory, fixed toolchain source, or other readonly mounts
is refused before publishing credentials. Host control paths can be long: connect holds
a directory descriptor through the WebSocket handshake and uses its short proc-fd alias.

`HomeSeed::read(configuration, authentication)` reads bounded regular files only.
It imports selected model/provider values and the existing authentication JSON,
never a recursive home, sessions, skills, hooks or foreign MCP entries. Legacy
home-managed configuration is explicitly unsupported pending a requirements import;
it is not silently dropped. Provider transport env permits only HTTP_PROXY,
HTTPS_PROXY, ALL_PROXY, NO_PROXY, SSL_CERT_FILE and SSL_CERT_DIR. Custom CA paths
must be reachable in the explicit `/etc/ssl/certs` mount; do not assume arbitrary
host paths or environment credential keys are forwarded.

The kernel-owned policy selects `neige-delivery-v1` with no sandbox fallback:
workspace write, minimal toolchain read, private provider home/control/MCP and proc
aliases denied, command network disabled, project config untrusted, and optional
external/code-mode/agent routes disabled. Native MCP uses the existing per-card
token in the MCP child configuration only; it is not injected into command shells.
The installed protocol must expose the profile and report `allowed=true`. Endpoint
version 2 records this bootstrap layout. Version 1 may still be probed/stopped with its
original handle, but activation is refused; no existing physical run is replaced.

These are generated-policy and protocol checks, not proof that a particular Codex
build enforces every inner-sandbox boundary. A coordinated real-provider probe must
verify intended writes and denied private paths, aliases, UDS and code networking
before integration enables this capability. No such live probe is part of these tests.

For the installed 0.153.4 CLI, the parent also verified the offline entry syntax:
`codex sandbox -P neige-delivery-v1 -C /workspace -- <command...>`; there is no
`linux` subcommand in that invocation. Use the production-rendered private config
and the SAME namespace path mappings, with fake authentication sentinels for an
offline check. A different host cwd does not match this profile's `/workspace`
rules. `--include-managed-config` can include managed policy when checking parity
with provider execution. An outer sandbox mount/setup rejection is not evidence
of an inner-profile permission denial. Those earlier offline probes are historical evidence; this port runs only fake-provider
tests and does not start an installed provider or model.

## Existing Operation checkpoint contract

1. Caller chooses and prepares the isolated workspace seed. Before declaring that
   input ready, it must create or verify a real nonsymlink `.codex` directory.
   `WorkspaceRequirement::ProtectedConfigDirectory` is a required caller precondition:
   this controller never changes the workspace to satisfy it. A file/symlink is
   rejected unchanged. All trusted executable sources must be outside the writable
   workspace and private provider state. The controller does not select an empty
   versus committed seed or implement artifact baseline capture.
2. Persist a `DedicatedRequest` in the existing Operation before preparation.
3. `Controller::prepare` creates private home and a dormant runtime init, returning
   `PreparedEndpoint`. Caller stores `SessionRecord::prepared(endpoint)`.
4. `Controller::connect` checkpoints ProviderStarting before runtime start. It uses
   the private socket and existing client, verifies profile availability, and
   checkpoints Connected. It does not issue a model turn.
5. `Session::create_thread` checkpoints CreatingThread before RPC; `begin_turn`
   checkpoints IssuingTurn before RPC. It then calls REQUIRED `TurnAdmission` with
   a non-cloneable owned `TurnLaunch`. The parent wraps only `launch.issue()` in its
   final TaskLaunch writer/source/owner fence. `issue` bounds the existing client
   send and acknowledgement and has no checkpoint/DB handle. After admission returns
   and the guard exits, the response is checkpointed. No default admission is provided.
6. The required `Checkpoint::save(expected,next)` callback must atomically compare
   the expected private Operation state AND current owner/lease, recheck current
   authorization/reference/delivery fences, and then store next. Stale state must
   reject before sending; a repeated intent does not grant another RPC.

The parent driver must preserve these writes when it later replaces its outer
TxOutput. Use a consistent outgoing in-memory value or a persist/reload boundary;
never overwrite the physical journal with the driver's stale clone. Do not nest
repository writes while holding another launch transaction.

Thread reply loss leaves CreatingThread. Reconciliation can adopt exactly one
empty thread from this endpoint, checking its identity and cwd. Turn reply loss
leaves IssuingTurn and returns explicit Unknown; it does not replay the prompt or
pick a latest turn. Post-thread/turn acknowledgement checkpoint failure is also Unknown on the first
call; pre-send veto remains distinguishable. Known turn replay returns the saved identity
without another RPC. Incomplete send cancellation/error synchronously shuts down the
same socket through a separately owned descriptor, before releasing the shared sink.
It does not flush a WebSocket close or rely on asynchronous reader abort; later RPCs
and automatic Pong cannot transmit the retained remainder. A fully flushed request's
ordinary reply timeout keeps the healthy transport. Uncertain phases reject before
incidental profile queries. Transport shutdown is not runtime quiescence: the original
provider and descendants remain owned until the runtime stop proof. A connected session returns its single notification stream to the caller's
existing liveness/recording path. Use `HomeReceipt.home` for the existing rollout
source; never start a parallel recorder or route this endpoint through shared-daemon
discovery/resume.

`Controller::stop` first checkpoints Requested, then stops the exact runtime.
Only its exact Quiesced proof is returned as stop evidence. It never releases the
workspace or publishes acceptance. The single-task caller owns final
task outcome and resource retention. Artifact capture, verifier execution and repair
eligibility are not implemented by this module.

`PreparedEndpoint::launch_request()` preserves the ORIGINAL submitted runtime request
for exact ownership/recovery checks; do not rebuild it from today's filesystem or
transport settings. Serialized launch data belongs only in private Operation state
because transport environment may contain credentials. Debug hides it; public/UI
responses and logs must not expose the nested request.

Private-home creation/reopen syncs directory ancestry. First publication, exact replay
and concurrent-winner adoption all complete the same directory publication barriers;
a visible receipt does not bypass a failed fsync. Replay preserves refreshed auth.

The native MCP socket is a read-only file bind. Replacing the host socket changes
its epoch, so reconnect/request checks fail explicitly instead of using a stale
mount. Transparent kernel-MCP listener replacement needs an integration-owned
stable endpoint/relay; this controller does not claim it is already solved.

## Production call sequence for a parent-owned probe

Use existing configured credentials; never print authentication contents or tokens.
The parent supplies an already-minted card token/native MCP socket and an Operation
checkpoint implementation. It should run a bounded instance and always stop it.

```rust,ignore
let controller = Controller::new(ControllerConfig {
    private_root: probe_root.join("provider"),
    runtime: RuntimeConfig {
        state_root: probe_root.join("runtime"),
        helper: installed_worker_boundary,
        bwrap: "/usr/bin/bwrap".into(),
        timeout: Duration::from_secs(30),
    },
    codex_binary: installed_codex,
    code_mode_host_binary: installed_codex_code_mode_host,
    sandbox_bwrap: installed_codex_package_bwrap,
    mcp_shim: installed_neige_mcp_stdio_shim,
    provider_environment: explicitly_selected_proxy_and_ca_values,
    connect_timeout: Duration::from_secs(30),
    request_timeout: Duration::from_secs(30),
})?;
let seed = HomeSeed::read(&configured_config_toml, &configured_auth_json)?;
let native = NativeMcp {
    socket: isolated_kernel_mcp_socket,
    card_token,
    plugin_tools: Vec::new(), // No delegated plugins for this standalone probe.
};
let endpoint = controller.prepare(frozen_request, &seed, &native).await?;
let initial = SessionRecord::prepared(endpoint);
// Parent writes initial into its already-existing Operation transaction.
let mut session = controller.connect(initial, operation_checkpoint).await?;
session.create_thread(operation_checkpoint).await?;
session.begin_turn(probe_request_key, bounded_probe_prompt,
    operation_checkpoint, task_launch_admission).await?;
let notifications = session.take_notifications()?; // use existing observer
// On every success/error/timeout, load the latest checkpointed record and:
controller.stop(&mut latest_record, operation_checkpoint, Duration::from_secs(10)).await?;
```

Build and run fake tests in a dedicated target (no installed model calls):

```sh
env -u NEIGE_CODEX_BIN RUSTC_WRAPPER= CARGO_BUILD_JOBS=6 \
  CARGO_TARGET_DIR=/tmp/neige1501-dedicated-codex-target \
  cargo build --locked -p calm-worker-runtime --bin calm-worker-boundary
env -u NEIGE_CODEX_BIN RUSTC_WRAPPER= CARGO_BUILD_JOBS=6 \
  CARGO_TARGET_DIR=/tmp/neige1501-dedicated-codex-target \
  cargo nextest run --locked -p calm-server --lib --test dedicated_codex \
  dedicated_codex --test-threads 8 --no-fail-fast
```

Platform-owned task execution supplies `NativeMcp.plugin_tools` from the frozen task
contract, not from an unfiltered live plugin catalog. The platform rechecks current
scope, ordinary tool kind, plugin availability and execution identity on every call.

The ignored fake-provider entry is invoked only as a child of production runtime
tests; no additional fake-provider production binary is shipped. The parent must
provide the namespace-capable Linux CI setup for this integration target. Default
namespace-restricted runners must fail preflight rather than skip this coverage.

The explicit `fixtures` feature also exposes a bounded zero-model `command/exec` probe
through the same client and named profile, only while Connected before any thread. It
ships no production command path around TurnAdmission. Manual installed-provider probes
are not model E2E and do not enable real Codex tests in the ordinary suite.
