# Native macOS execution

Newly built binaries support registered **Private ExternalDir + Pipe** execution
for `task-verify`, `task-gate-run`, forge commands and their verdict/output probes,
and the provider rewind checks already connected to the registered executor.
These consumers use its durable journal, authorization, release, stop, and
retirement lifecycle. Following the registered Claude migration, ordinary turns
and compaction also use this executor. Their completion owner accepts either
Linux drain or Mac original-group settlement, while unconfirmed execution still
cannot certify a successful provider result. This does not establish full native
Claude feature parity or upgrade an already running Neige installation.

`neige-app` creates a private `execution-client-key.json` with mode `0600` in its
configured data directory and passes the same key path to the kernel and process
supervisor. The supervisor also receives its execution journal directory.
These paired arguments are required for registered execution. Launches use the
registered directory handle and the caller's explicit environment; they do not
inherit every variable or descriptor from the supervisor.

## Completion and recovery

The macOS supervisor owns the payload child directly. While that root remains
unreaped, its PID pins the original process group. Completion records
`OriginalGroupEmpty` only after observing root exit, observing no other member of
that original group, reaping the owned root, and completing bounded output
collection. A root that exits before same-group children triggers the existing
stop path. Stop uses TERM, then KILL after the grace period, through the same
owned handle. Nonzero exit and timeout do not become successful gate results.
Task cancellation preserves the canceled task outcome and keeps the checkout
busy until the running gate and delivery end; it does not release resources early.
Gate target sampling still runs before and after the steps.

This is an observation of the original group, **not a full descendant proof**.
Programs can deliberately leave it with `setsid`, `setpgid`, job control, or
daemonization. Such background processes can escape observation and cleanup;
their departure may be undetectable, and an original-group completion can still
be recorded. Run commands that remain in the foreground. Do not use this path
when safe handoff requires proving that every possible descendant has ended.

After a supervisor restart on the same OS boot, a formerly active execution
starts as `Unknown`. Each registered `Inspect` retries read-only native recovery
under the existing execution and resource locks. Recovery requires the original
receipt, matching slot and boot, and the supported Mac mechanism. It observes
both the disappearance of the original PID + birth identity (including zombies)
and an empty original process group. A living original identity blocks recovery
even if it has left that group. Any remaining member, permission denial, query
error, truncated identity result, or missing receipt preserves `Unknown` and its
journal, alias, and resource reservation. A timeout or refused Stop supplies no
new evidence and cannot release those reservations.

The native calls are `proc_pidinfo(PROC_PIDTBSDINFO, arg=1)` and a non-null
one-PID `proc_listpids(PROC_PGRP_ONLY)` query. The latter proves emptiness only
when it returns zero bytes with a freshly cleared `errno` still zero; libproc
also returns zero on syscall errors. This follows Apple's
[libproc wrapper](https://github.com/apple-oss-distributions/xnu/blob/main/libsyscall/wrappers/libproc/libproc.c)
and [XNU process queries](https://github.com/apple-oss-distributions/xnu/blob/main/bsd/kern/proc_info.c).

Only after the journal durability barrier succeeds does recovery publish
`InterruptedQuiescent` / `ExecutorInterrupted` with `MacOriginalGroupEmpty`.
Incomplete output remains incomplete. This allows retirement and explicit alias
handoff, but supplies no successful command result, exit status, session-wide
proof, or escaped-descendant proof. Existing gate, forge, and provider consumers
retain infrastructure-failure semantics. Recovery never signals a PID from
stored metadata, reconstructs a live owner, or reruns the command. A genuine OS
boot change remains separate evidence. Failure to read the real boot UUID
refuses startup; no synthetic boot identity is used.

For an operational recovery, preserve the execution ID, journal, output, host
boot, and permission/query diagnostics. Retry through the normal registered
Inspect path for a bounded observation window (for example once per second for
30 seconds); a caller deadline ends waiting, not the reservation. If a known
workload is still running, let its owner finish it through its own verified
control channel. If native evidence remains unavailable, quarantine the affected
checkout and alias from reuse and escalate the collected evidence to the host
operator. Restore appropriate observation access or arrange an isolated host
maintenance procedure with independently verified workload ownership, then retry
Inspect. Do not kill stored bare PIDs, delete journals, or release resources
because a deadline elapsed.

Linux retains its helper/subreaper lifecycle and full descendant meaning for
`Drained` / `NoChildrenAfterForkEntryClosed`. Mac original-group evidence must
never be presented as that Linux proof. The native implementation uses journal
version 5 (version 7 for interruption evidence) and retains receipt contract 4. Valid Linux version 4/5 journals remain
readable without filling missing facts; version 4 is normalized to version 5 on
open. Registered wire envelope 12 requires a matching peer. Legacy terminal
negotiation remains separate.

## Shared Codex daemon recovery

The shared owner has a separate recovery protocol from registered execution.
An acknowledged reservation creates a new scope before connecting to a peer or
spawning a child. The unentered reservation can be canceled only by that same
attempt. Entering either operation consumes that permission synchronously;
connection and spawn errors leave a durable Pending obligation. Reconstructing
an owner from Pending never reconstructs cancellation permission. Actual process
facts are retained before their first database publication, including ordinary
publication failures. Settlement requires the same Recorded scope and a native
empty-group observation; it grants no permission over a later listener.

Endpoint cleanup holds the verified parent directory descriptor, rejects
symlinks and non-socket leaves, and uses public `unlinkat(AT_NODELETEBUSY)`.
Automatic cleanup requires that capability on the host filesystem. Unsupported
operations refuse cleanup without ordinary-unlink fallback; an ordinary error
before process entry cancels the current reservation. Cancellation errors,
uncertain commit responses and aborted attempts retain the conservative fence.
Healthy takeover and socket-free spawn do not require conditional deletion.
No minimum macOS release or filesystem whitelist is implied. Leaf checks plus
conditional deletion do not establish an arbitrary-object atomic CAS against a
malicious same-UID replacement with a non-socket object.

For an unresolved obligation, identify the backend, phase and scope in the
error, investigate that scope's producers and stop them safely before explicitly
repairing the same backend's obligation and endpoint. Clearing identity columns
alone is insufficient for Pending and invalid for Recorded. Do not clear another
backend or assume an absent leader proves the original group empty. Recovery
cannot certify descendants that deliberately leave their original group.

## Limits and macOS permissions

Registered strong Worktree isolation and registered PTY are unsupported on macOS.
The legacy terminal remains available. Secure file operations requiring Linux
`openat2` keep their existing refusal behavior. There is no container backend or
OS application automation in this support scope.

Commands run with the launching host's OS identity. Access to protected folders,
desktop data, and other TCC-controlled resources depends on how that host was
started and the permissions macOS grants it. Native execution does not bypass
TCC, grant Full Disk Access, or change a provider's sandbox permissions. No desktop
capture or permission prompt is needed to verify this implementation.

## Isolated verification

Use a disposable checkout and private temporary data, journal, key, database,
socket, and workspace directories. Do not point tests at a running installation's
resources. Tests must retain ownership of every child, arm bounded cleanup before
fallible assertions, and finish cleanup before starting more children. Unknown
PIDs are diagnostic information only.

From the repository root, with the pinned Rust toolchain:

```sh
env -u NEIGE_CODEX_BIN RUSTC_WRAPPER= CARGO_BUILD_JOBS=6 \
  cargo nextest run --locked -p calm-proc-supervisor --lib \
  -E 'test(/native_macos|recovery_3450/)' --no-tests fail --test-threads 1
env -u NEIGE_CODEX_BIN RUSTC_WRAPPER= CARGO_BUILD_JOBS=6 \
  cargo test --locked -p calm-session --lib execution_ancillary \
  -- --test-threads=1 --nocapture
env -u NEIGE_CODEX_BIN RUSTC_WRAPPER= CARGO_BUILD_JOBS=6 \
  cargo test --locked -p calm-proc-supervisor --test integration_suite native_macos \
  -- --test-threads=1 --nocapture
env -u NEIGE_CODEX_BIN RUSTC_WRAPPER= CARGO_BUILD_JOBS=6 \
  cargo test --locked -p calm-server --test mcp_integration_suite native_macos \
  -- --test-threads=1 --nocapture
env -u NEIGE_CODEX_BIN RUSTC_WRAPPER= CARGO_BUILD_JOBS=6 \
  cargo test --locked -p calm-server --test planner_harness_suite native_macos \
  -- --test-threads=1 --nocapture
env -u NEIGE_CODEX_BIN RUSTC_WRAPPER= CARGO_BUILD_JOBS=6 \
  cargo test --locked -p calm-server --test forge_action_adapter native_macos \
  -- --test-threads=1 --nocapture
```

Require a nonzero selected test count for each target/filter and preserve complete logs,
exit codes, and child/resource cleanup evidence. Follow the repository's text,
contract, and quick Rust gates as well; use real cargo-nextest for contract gates.
On macOS those shell gates require GNU grep and coreutils on the command's PATH.
The full contract suite currently requires a case-sensitive temporary filesystem
for plugin-name fixtures; see [#3110](https://github.com/keanji-x/neige-calm/issues/3110).
Set `TMPDIR` for that command to a private directory on a case-sensitive volume,
retain the full suite and its assertions, and clean up only resources owned by
that verification run. For a local run, prepare a private sparse image and short
mount path in the same terminal. Stop if any preparation command fails; preserve
the variables and diagnostics for cleanup rather than running the suite:

```sh
contract_root=$(mktemp -d /private/tmp/ncase.XXXXXX) &&
contract_image="$contract_root/case.sparseimage" &&
contract_mount="$contract_root/m" &&
mkdir "$contract_mount" &&
hdiutil create -size 16g -fs 'Case-sensitive APFS' -type SPARSE \
  -nospotlight "$contract_image" &&
hdiutil attach -nobrowse -owners on -mountpoint "$contract_mount" \
  "$contract_image" &&
mkdir -m 700 "$contract_mount/t"
```

From the checkout root, use the existing Cargo cache and scope `TMPDIR` to the
contract command; leave other native checks on their own short temporary paths:

```sh
env -u NEIGE_CODEX_BIN TMPDIR="$contract_mount/t" \
  PATH="$HOME/.cargo/bin:/opt/homebrew/opt/grep/libexec/gnubin:/opt/homebrew/opt/coreutils/libexec/gnubin:/opt/homebrew/bin:$PATH" \
  CARGO_BUILD_JOBS=6 scripts/local-contract-gates.sh
```

After the command has exited and its test processes have finished, retain the
exit status and full log, then clean up in that same terminal. Unmount only this
run's volume, and remove its image only after detachment succeeds. If detachment
fails, preserve the image and diagnostics; do not force it or detach other volumes.

```sh
hdiutil detach "$contract_mount" &&
rm "$contract_image" &&
rmdir "$contract_mount" "$contract_root" &&
unset contract_mount contract_image contract_root
```

For a kernel gate, its owner prepares the volume and performs cleanup after the
gate ends; the worker uses the supplied `TMPDIR` without creating or detaching it.

The native gate tests also cover a failed task followed
by a successful task on the same isolated instance, track, and checkout through
worker report, candidate delivery, and `task-verify`.
An execution sandbox may deny `kern.bootsessionuuid` even when a native terminal
can read it. Record that denial and rerun the exact check in an authorized native
terminal; it is neither a pass nor a reason to weaken the identity check. A running
older kernel may also refuse a Mac gate before executing any step. Verify the
fixed saved revision independently without replacing the running service.
