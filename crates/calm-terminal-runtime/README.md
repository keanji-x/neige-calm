# Private terminal runtime (#1548)

This package hosts pinned `rmux-server` in a separate process and connects with
`rmux-sdk`. No PTY implementation is copied into Neige. The server's web surface
is disabled. Upstream is MIT OR Apache-2.0; distribution must
carry its selected license notices when this binary is added to the release.

`RuntimeLaunch::command` is the application launch boundary: explicit executable,
socket, working directory, HOME, PATH and locale; no inherited credentials or
implicit rmux configuration. `connect` only connects to the supplied socket.
It returns a constrained `RuntimeClient`, not the raw SDK create/respawn surface.
`TerminalLaunchConfig.environment` supplies the complete invoking-client environment
through RMUX's typed request. Unix SDK creation otherwise reads the requester's
`/proc` environment, even when the daemon itself was launched with `env_clear`.
Creation timeouts have an explicit `OutcomeUnknown` result: an already queued
blocking request may still create the pane and must be reconciled by its name.
The owning application must not blindly retry or dispose of its workspace.

For a Neige terminal's process ownership, use `isolated_command` with an explicit
absolute `unshare` executable. It creates a user/PID namespace, runs the RMUX
host as namespace init, and ties that child to its launcher. Graceful SDK shutdown followed by
successful launcher exit is the current namespace stop receipt. Abnormal launcher
death triggers containment but reaping that launcher alone does not synchronously
prove quiescence; integration must also observe namespace init termination. This is
process containment only, not a filesystem/network sandbox. There is no fallback
when namespaces are unavailable. Plain `command` remains useful for daemon/SDK
probes, but an RMUX pane-close response alone is not a process-tree stop receipt.

The reason for one runtime per owned Terminal is measurable: upstream pane
removal starts termination in the background, and an ignored HUP can outlive the
pane/leader. The namespace shutdown regression checks a real descendant's socket
closes after runtime shutdown and successful launcher exit. A separate test kills
the launcher and requires descendant EOF, verifying the parent-death fence without
treating launcher exit alone as proof. Neige's supervisor should
own this outer process; RMUX remains the sole owner of PTYs inside it.
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

## Coherent observation and developer preview

`TerminalSession::observe` returns one bounded RMUX recovery capture with a
mandatory typed viewport, cursor, process generation, next output sequence,
keyframe and history coverage. Grid and keyframe come from the same upstream
capture. Missing/revoked panes are errors, never a blank successful observation.
This read result is not an input compare-and-swap token or a Neige ownership lease.

The developer-only `preview-driver` example launches a contained, temporary host
and accepts observation/text/key requests over its parent's private stdin. It
loads no model or application configuration. It is not an application endpoint
and must not be exposed as a user-facing or model-facing service.

A browser acceptance probe runs actual fzf through that driver, reconstructs
immutable captures in xterm 6, and compares all viewport cell text, widths, bold,
reverse-video, foreground/background encodings and cursor positions. It exercises
application PageDown/PageUp, real mouse selection through xterm's terminal input,
slash filtering, arrow navigation, Enter confirmation and viewport-only history
scroll. PNG/JSON artifacts distinguish the source capture from the scrolled view.
This uses the xterm dependency used by Neige's frontend; it does not claim to test
Neige Terminal-card creation, its production WebSocket bridge, MCP authorization,
or actual astry image receipt.

On a Linux developer host with working user/PID namespaces, `/usr/bin/fzf` and
Playwright Chromium installed:

```bash
(cd fe && npm ci --ignore-scripts && npx playwright install chromium)
env -u NEIGE_CODEX_BIN RUSTC_WRAPPER= CARGO_BUILD_JOBS=4 \
  cargo build --locked -p calm-terminal-runtime --bin neige-terminal-runtime \
  --example preview-driver
node scripts/spike/terminal-runtime-preview-check.mjs \
  --driver /absolute/target/debug/examples/preview-driver \
  --runtime /absolute/target/debug/neige-terminal-runtime \
  --output /absolute/new-evidence-directory
```

The probe binds an ephemeral loopback port, creates its own runtime and fresh
artifact directory, and closes its browser/runtime on completion. It never
connects to an existing user's terminal or invokes a real model.
