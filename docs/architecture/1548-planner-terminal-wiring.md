# Planner Terminal client wiring (#1548)

The application entry point is a Planner-only MCP tool set:

| Tool | Behavior |
|---|---|
| `calm.terminal.open` | Idempotent visible Terminal-card creation through `terminal-create` OperationRuntime, attributed to the authenticated Planner session. |
| `calm.terminal.resolve` | Resolve an exact current task attempt or Terminal ID to its real Worker card, worker session and view availability. |
| `calm.terminal.observe` | PNG and text/cursor/mode state from the same captured RMUX projection, with observation and connection IDs. Reads never create or restart a process. |
| `calm.terminal.control` | Claim/release control, or detach the model client while retaining the card/program. |
| `calm.terminal.input` | One text/key/cell-click action, bound to a recent live observation and current control. A matching request ID replays its receipt without another write. |

## Ownership and observation

This slice retains Neige's existing supervisor PTY and terminal-create lifecycle.
The Planner and human use the same Terminal card, renderer entry and process.
The separate `calm-terminal-runtime` daemon component remains available for a
later explicit backend migration; these tools do not silently switch backends.

The existing RenderPlane installs a read-only observer before ingesting its first
output. RMUX core receives every original byte and resize in order and maintains
the model-facing grid and history. It emits no replies: the current server render
plane remains the query responder. The observer is shared by model clients, so
attaching or reconnecting an observation client never reconstructs state from the
legacy ANSI snapshot (which loses CJK widths, cursor modes and resize history).
A lost supervisor output stream or history gap makes the observation unavailable.
After server reattachment with a persisted process ID, geometry history is not
proven complete, so model observation fails explicitly; human reconnect retains
its existing behavior. Open a new Terminal for the model instead of silently
claiming complete recovery. No model read launches a replacement process.

The frame is immutable before rasterization. System-font-only `resvg` renders
escaped terminal text into a bounded PNG with a fixed cell geometry. No terminal
text is interpreted as SVG markup, file paths or external image URLs. The image
uses the model projection's font/viewport, not a screenshot of browser chrome.
The browser continues using xterm with its existing font settings. Sixel and
inline image protocols are not rendered. Containers install DejaVu and Noto CJK;
missing system fonts return an explicit image error.

Scope comes from live MCP session/card/Track identity and is checked at tool
admission and again by the queued write's scope callback. A connection owns a
server-issued lease. The final writer rechecks that lease under a barrier shared
with ownership grants, then retains the barrier until the supervisor acknowledges
the physical PTY write. Queued stale writes are refused. If a sent write loses its
acknowledgement, the barrier becomes uncertain and refuses new ownership grants;
cancelling a Rust future does not prove the supervisor's blocking write stopped.
Existing kernel-originated input remains a distinct trusted capability.

Request receipts belong to the connection, with bounded hashed request identity.
A dropped/detached connection drops its receipts; old observation IDs cannot
address the new connection. Client watchdogs stop idle or no-longer-authorized
clients, and admission prunes their handles. No timer automatically retries input.
The global observation registry expires captures after 120 seconds and has a
fixed limit; it retains only input geometry/modes and identities, not screen
cells, text or image bytes. Each capture contains the exact control identity it observed; a
human takeover invalidates it. Output changes also require a new observation.

A successful input reply says `written`, not that the TUI completed an action.
Text excludes control characters and never implicitly submits. Enter, Escape,
arrows and other supported keys are explicit actions. Application mouse input
requires reported SGR mouse mode and in-range cell coordinates. Local history
scrolling uses `observe.scroll_offset`; application paging uses explicit keys.

## Task and Worker targeting

Resolve, observe, control and input accept exactly one of `terminal_id` or
`task_id`. The latter is the exact current `attempt_id` returned by
`calm.plan.list`, not the logical task key. It resolves the task's actual Worker
card and current worker session in the authenticated Planner's Track. Terminal,
Codex and Claude Worker cards are supported; Planner and Assistant cards are
excluded. Manual Codex/Claude Worker cards may be addressed by Terminal ID.

Both selectors validate task ownership, including historical card membership,
spawn-operation identity and the current execution allocation. A recovered task
must be selected explicitly; an old Terminal ID cannot bypass this rule. Client
and observation identities bind the exact task, card, worker session and Terminal.
Queued input rechecks that binding immediately before the supervisor write.
A finished task can retain a readable view, but cannot claim control or receive
input. Release and detach remain available for cleanup.

Some isolated Codex workers have a terminal record but no live PTY viewer.
`resolve` returns `available: false` and `controllable: false` in that case;
observing never starts a substitute session. Images cover only the selected
Terminal's RMUX viewport, with independent per-terminal control and scroll offset.

## Child environment

The supervisor clears inherited child environments and supplies an explicit OS
and developer-tool allowlist plus the caller's typed `EnsureProcRequest.envs`.
It includes HOME/PATH/locale, desktop/SSH-agent endpoints, proxy and CA settings;
application credentials such as MCP tokens are not inherited implicitly. Provider
or worker configuration must be supplied explicitly by its existing launch
request. This applies to both Pipe and PTY branches. User shell startup files keep
their ordinary shell contract. `ccode` on the development host is a zsh alias that
sets HTTP_PROXY and HTTPS_PROXY to `http://127.0.0.1:2080` before starting Claude.

## Acceptance evidence and limits

The focused suite uses the actual authenticated MCP UDS server, real operation
runtime and renderer, and a real shell. It verifies visible card identity, native
PNG delivery, exact application output, physical Enter counts for duplicate
requests, same-database foreign-Track refusal, human takeover and reconnect IDs.
The writer tests hold real protocol work and physical acknowledgement separately.
Projection tests traverse actual RenderPlane output and resize paths.

A developer driver reuses only this test setup and calls the actual tools; it is
not a Planner implementation. It accepts private NDJSON stdin and starts a
disposable loopback browser preview. Build with:

```sh
env -u NEIGE_CODEX_BIN RUSTC_WRAPPER= CARGO_BUILD_JOBS=4 \
  cargo build --locked -p calm-server --example planner-terminal-driver
```

On 2026-09-07 this driver exercised actual Claude Code 2.1.259 through `ccode`:
open/claim/observe, send baseline and target prompts, `/rewind`, select the target,
choose Restore conversation, observe the baseline retained and target prompt
restored, then resubmit and observe the target reply again. All steps used the
same Terminal ID and renderer session; the actual process's HTTP/HTTPS proxy was
verified as 127.0.0.1:2080 without reading credentials into logs. A real browser
TerminalCardView connected over the production WebSocket to the same session.
The temporary terminal and driver were closed after evidence capture.

This verifies the real Claude interaction through the Planner-facing tools. It
is **not evidence that the actual astry Planner autonomously chose those calls**.
That final model-level acceptance still requires the dedicated Tier 2 environment;
real Codex E2E remains prohibited on the shared production host.
