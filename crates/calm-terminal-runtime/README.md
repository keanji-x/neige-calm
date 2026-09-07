# Private terminal runtime (#1548)

This package hosts pinned `rmux-server` in a separate process and connects with
`rmux-sdk`. No PTY implementation is copied into Neige. The server's web surface
is disabled. Upstream is MIT OR Apache-2.0; distribution must
carry its selected license notices when this binary is added to the release.

`RuntimeLaunch::command` is the application launch boundary: explicit executable,
socket, working directory, HOME, PATH and locale; no inherited credentials or
implicit rmux configuration. `connect` only connects to the supplied socket.
It returns a constrained `RuntimeClient`, not the raw SDK create/respawn surface.
`TerminalSpec.environment` supplies the complete invoking-client environment
through RMUX's typed request. Unix SDK creation otherwise reads the requester's
`/proc` environment, even when the daemon itself was launched with `env_clear`.
Creation timeouts have an explicit `OutcomeUnknown` result: an already queued
blocking request may still create the pane and must be reconciled by its name.
The owning application must not blindly retry or dispose of its workspace.
The socket parent must be an existing private owned directory. The host leases
a lock file and refuses existing socket paths; stale endpoint recovery requires
an explicit future ownership/recovery protocol, never an automatic unlink.

The host loads only a private temporary policy generated from fixed source:
`exit-empty=off` and `remain-on-exit=on`. It does not load system/user rmux or
tmux configuration. Connections wait for configuration readiness through the
upstream typed client; they never invoke CLI or auto-start. The upstream
FIFO helper entrypoint is wired before CLI parsing, as required for embedding.
The owning application must explicitly supervise/reap the process. Client
disconnect and runtime shutdown are different operations.

This is the first runtime integration slice. It is not yet wired into Terminal
card creation, persisted backend handles, release packaging, or Planner tools.
Those remain tracked by #1548. In particular, an SDK pane exit is not itself
permission to complete a Neige task or delete a workspace.

Acceptance exercises the built host and real shell through public SDK calls:
create, input, observe, reconnect without replacement, retained exit information,
shutdown, private endpoint ownership and launch-environment isolation. No real
model is used. Next slices must cover Neige's operation and persistence fences.
The runtime client does not yet certify persisted handles across daemon restarts;
Neige's backend identity and generation checks belong to the next slice.
