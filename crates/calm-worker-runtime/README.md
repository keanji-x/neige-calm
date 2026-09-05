# Owned Worker process boundary

This crate provides a Linux PID namespace for an explicitly admitted local Worker.
It does not select tasks, validate artifacts, or implement a provider protocol.
The helper and library must be deployed from the same build.

`Runtime::prepare` starts a trusted PID 1 which waits for a positive start frame.
Persist its `BoundaryHandle` in the caller's Operation before `Runtime::start`.
The supplied program starts in `/workspace` with only the explicit environment
and mounts. Runtime metadata is kept in a separate private directory. Existing
run IDs are never relaunched, including when evidence is incomplete or lost.
Keep run directories for at least as long as any attempt/history references them;
deleting all evidence also deletes this crate's replay fence.

`start` records admission, not provider readiness. `probe` returns `Prepared`,
`Running`, `Quiesced(proof)` or `Unknown(reason)`. `stop` first closes start
admission durably, sends SIGKILL through the namespace init's pidfd and waits for
actual init disappearance/reaping. It does not infer quiescence from provider
stdio, task reports, a terminal, a lease, or the outer launcher. Identity or I/O
errors remain unknown. A timeout is never a successful stop proof.

Linux terminates and waits for the rest of a PID namespace before its init can
be reaped; session/process-group changes do not escape this ownership. The host
launcher owns bwrap; a small init reaps adopted children and exits when the
provider child exits. No systemd/cgroup manager or shared daemon is required.
The launcher survives ordinary caller exit. Launcher loss triggers bwrap parent
death cleanup; a subsequent caller must still reconcile actual init evidence.

The blocking API is intended for a blocking executor in an async server:

```text
Runtime::new(RuntimeConfig { state_root, helper, bwrap, timeout })
runtime.preflight(NetworkPolicy::Isolated) # or explicit Provider for app-server
handle = runtime.prepare(unique_run_id, launch_config)
persist_handle_with_operation(handle)
transport = runtime.connect_stdio(handle)  # optional raw stdin/stdout
recheck_attempt_and_authorization()
runtime.start(handle)
...
state = runtime.stop(handle, deadline)
require_exact_handle_and_quiesced(state)   # before sealing source
```

`LaunchConfig` requires attempt identity, network policy, workspace, program, arguments,
environment map and additional typed mounts. The caller owns workspace allocation
and must grant only the intended writable paths. The runtime denies mounts which
expose its own metadata and reserves its system/internal mount destinations. Mount
sources and the helper/bwrap executables must remain under trusted host ownership
during launch. The private state root must be caller-owned with no group/other
access. Hostile same-user host processes and host filesystem administrators are
outside this boundary.

The raw stdio relay admits one live client and applies bounded backpressure. It
does not implement JSON-RPC or durable message replay. Client disconnect preserves
the provider; callers must reconcile their protocol rather than blindly resend
non-idempotent requests. After init death, final output is drained to the attached
client; an undelivered tail is retained in `undelivered.stdout` in the run directory.
Stderr is retained separately and is untrusted provider output.

For Codex, the existing WebSocket-over-UDS client can instead connect to a private
provider-control socket exposed through an explicit writable mount. That avoids a
second protocol implementation. **The outer namespace does not separate provider
control/home from code executed inside it.** The integrating provider sandbox must
deny code access to those paths and credentials. For `NetworkPolicy::Provider`, code
execution networking, remote execution and external write-tool restrictions remain
integration requirements. This crate does not claim those inner policies or
external-effect replay safety.

`NetworkPolicy::Isolated` creates a separate network namespace for the entire
process tree, suitable for a verifier running on its own writable copy without
credentials or provider-control mounts. `Provider` explicitly retains the host
network for model requests. There is no implicit policy: the serialized field is
required, preflight verifies actual network identity for the chosen mode, and
capture rechecks it. Ignoring an unsupported isolation flag cannot fall back to
host networking. Network tests inspect namespace identity without sending requests.

Only the explicit Linux/bwrap/user+PID namespace/private-proc/pidfd/close-range
capability is supported. Preflight runs the trusted helper with the actual bwrap
recipe and checks pidfd signaling capability. Unsupported hosts fail explicitly;
there is no shared or unsandboxed fallback. Graceful provider shutdown can be
requested separately; `stop` is the hard ownership boundary.

Run the focused tests on a Linux host permitting unprivileged namespaces:

```sh
env -u NEIGE_CODEX_BIN RUSTC_WRAPPER= CARGO_BUILD_JOBS=6 \
  cargo nextest run --locked -p calm-worker-runtime --features test-support \
  --test-threads 8 --no-fail-fast
cargo clippy --locked -p calm-worker-runtime --all-targets \
  --features test-support -- -D warnings
```

Tests exercise the production launcher/init with a fake provider; they never start
real Codex. `test-support` only builds the test provider and integration target.
The fixture additionally pins namespace init for cleanup when tests deliberately
remove evidence. Restricted outer sandboxes may forbid private proc mounts; such
a test failure is explicit, not a skipped success.
