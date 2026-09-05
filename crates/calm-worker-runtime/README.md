# Owned Worker process boundary

This crate provides a Linux PID namespace for an explicitly admitted local Worker.
It does not select tasks, validate artifacts, or implement a provider protocol.
The helper and library must be deployed from the same build.

`Runtime::prepare` starts a trusted PID 1 which waits for a positive start frame.
Persist its `BoundaryHandle` in the caller's Operation before `Runtime::start`.
The supplied program starts in `/workspace` with only the explicit environment
and mounts. Runtime metadata is kept in a separate private directory. Existing
run IDs are never relaunched, including when evidence is incomplete or lost.
Prepare compares the original submitted-request fingerprint before checking paths
needed only by a new launch. An identical replay returns its receipt even after
the old workspace or mount sources have been removed. Changed arguments, mounts
or network policy still conflict under the same run ID. Private record version 2
stores that fingerprint; earlier development version 1 records lack this evidence
and are refused, never reinterpreted or silently relaunched.
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
The init polls its specific provider PID and the direct-child inventory from its
private proc mount using nonblocking per-PID waits. Adopted zombies are collected
while the provider runs; the provider's exit code is preserved. No process-wide
wait or supervisor wait-status stealing is used.
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

Client `shutdown(Write)` ends only that client's request direction; it keeps
ownership of the response direction, including delayed provider output. **Provider
stdin intentionally stays open across client write-half closure and full disconnect**
so a replacement client can continue the same process. This reconnectable endpoint
is not a transparent forwarding of client EOF to provider stdin. Send the provider's
own termination request or call `stop` when execution should end. A fully disconnected
client releases the attachment after its already-received input is drained; a second
live client is rejected. Buffered stdout not yet sent is retained for the active or
next client. Bytes already sent to a client's kernel socket are not durable replay.

When every provider stdout writer closes, the relay drains its output and shuts
down only its write direction to the client. The client observes EOF even if the
provider remains alive and accepts stdin. Neither stream EOF changes process state
nor provides a quiescence proof. The trusted init and bwrap monitor do not retain
provider stdout write ends.

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

## Required CI integration

The ordinary workspace command with `--features calm-server/codex-e2e` does **not**
enable this crate's `test-support`. It therefore does not execute the runtime
integration target or establish the 22-test result recorded during development.
The CI workflow runs the targeted **worker process isolation** job on an ephemeral
GitHub-hosted Linux runner. The required `rust (test)` aggregate depends on both
the ordinary Rust shards and this job, so a failed or skipped isolation check
cannot produce a successful Rust verdict for an affected change.

The Ubuntu hosted image restricts unprivileged user namespaces. The job temporarily
permits them on its disposable runner and restores the original value in an always
step, following [Ubuntu's documented namespace setting](https://discourse.ubuntu.com/t/ubuntu-24-04-lts-noble-numbat-release-notes/39890).
This setup is guarded by `RUNNER_ENVIRONMENT=github-hosted`; neither the library
nor shared production-host tests change host namespace policy.

Use a Linux runner with bubblewrap and Python 3 installed, a kernel supporting
user/PID/network namespaces and pidfd/close-range operations, and a runner policy
permitting unprivileged bubblewrap namespaces and private proc mounts. Python is
used only by the controlled stale-identity/ignored-flag test executables. For a
Debian/Ubuntu runner, the package setup and targeted command can be:

```sh
sudo apt-get update
sudo apt-get install -y bubblewrap python3
env -u NEIGE_CODEX_BIN RUSTC_WRAPPER= CARGO_BUILD_JOBS=6 \
  cargo nextest run --locked -p calm-worker-runtime --features test-support \
  --test-threads 8 --no-fail-fast
```

Reuse the repository's existing Rust/nextest installation steps. Make this job or
step a required gate in the assembled change. A runner whose sandbox/AppArmor or
container configuration denies the requested namespaces needs explicit runner
setup or a suitable dedicated runner; do not convert the failing preflight into
an ignored test, successful skip, or host-network fallback. Keep the fake provider
behind `test-support`, rather than shipping it by default just to make CI run it.
