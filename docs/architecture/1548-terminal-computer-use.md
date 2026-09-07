# Terminal computer use — implementation record (#1548)

## Outcome and order

The Planner using the user's astry model observes and operates the same terminal
as the human. Each slice ends with a preview loop: observe, decide, act, observe,
and verify. Issue #1548 defines S0–S6; none is complete merely because protocol
tests pass.

Implementation status (2026-09-07): #1551, #1562 and #1563 are merged;
#1566 supplies the coherent observation and real-TUI developer preview. Composer
attachments from #1535 are also merged. Actual astry receipt and production
Terminal-card/Planner wiring remain open acceptance work in #1548.

The chosen runtime uses published RMUX libraries through a private host process.
Neige retains card/operation identity, authorization, durable launch intent and
workspace disposal. RMUX owns the PTY, terminal state and recovery stream. The
application integration will connect both human and Planner views to that same
backend; the developer probe currently owns an independent temporary terminal.

## First implementation boundary

The first change makes the kernel tool-result contract explicitly multimodal.
Every registered handler returns a typed result. Existing JSON-only tools use an
explicit structured-result constructor, preserving their wire representation.
Transport serializes that result once; it never guesses whether a JSON object
with `content` keys was intended as an MCP envelope. Images are native MCP image
content blocks, with their bytes separate from structured screen metadata.

Acceptance: authenticated calls through the actual UDS MCP server preserve a
PNG image block and its metadata. JSON-only results, tool errors, role checks,
and thread identity retain their behavior. An image-shaped JSON payload remains
ordinary JSON unless its handler explicitly constructs an image result.

This is an S0 prerequisite, not an assertion that astry has received an image.
The actual model ID, input modality, deployment runtime, and dedicated preview
host still need to be verified before the real-model acceptance run.

## Implemented observation boundary

`calm-terminal-runtime` hosts pinned `rmux-server` 0.10.0 and connects through
`rmux-sdk` plus typed `rmux-client` requests. Creation explicitly supplies the
complete invoking-client environment: the SDK convenience path otherwise reads
the requester's `/proc` environment. Unknown creation outcomes require
reconciliation by unique session name, without blind retry or workspace disposal.

`TerminalSession::observe` obtains one upstream recovery capture with a required
`TerminalObservation.snapshot`. Its grid, cursor, keyframe, process generation,
next output sequence and history coverage share that capture boundary. Missing
or revoked panes produce an error. The timeout bounds the read, and a captured
output sequence does not serve as an action authorization token.

The current developer renderer reconstructs that immutable keyframe in xterm 6.
The browser check compares all viewport cells and cursor, including tagged
palette/RGB color modes, before saving PNG and JSON. It records the local view's
history offset separately from the source terminal viewport. This establishes
capture/render parity for the exercised fixtures; the earlier live-DOM screenshot
probe below retains its weaker `pixels_only_unverified` contract.

The real fzf probe exercises application PageDown/PageUp, mouse selection through
xterm input, `/s` filtering, visible Down/Up selection changes and Enter results.
Viewport-only history scrolling is checked against actual text/key requests,
rather than an output sequence that could stay unchanged after silent input.
The driver handles parent TERM/INT, and browser and driver cleanup run independently.
See `crates/calm-terminal-runtime/README.md` for reproducible commands and failure
checks. These probes do not invoke a real model or Neige's production WS route.

## Process ownership and future application integration

One runtime per owned Terminal is the selected containment topology. Upstream
pane removal begins termination asynchronously; an HUP-ignoring descendant can
outlive its pane. The Linux user/PID namespace launch supplies process containment.
Graceful SDK shutdown followed by successful launcher exit is the tested stop
receipt. Abnormal launcher death alone is insufficient: the application must also
observe actual namespace-init termination before disposing of a workspace. This
mechanism makes no filesystem or network sandbox guarantee.

The next integration slice must freeze the backend, unique runtime endpoint and
launch identity in persisted creation state, and carry them through reconnect,
exit handling, compensation and disposal. Reads must never start a replacement
runtime or silently fall back to another backend. RMUX recovery should feed the
human renderer directly, with explicit history and theme capabilities.

Local call-path inspection found two supervisor adapter requirements: its Pipe
launch currently overlays inherited environment, and its ready FD expects
`ready\n`, whereas upstream RMUX writes a different startup token. The integration
must preserve the explicit environment and readiness contracts. No existing
Worker/provider launch path is switched by the runtime or preview prerequisites.

The future interaction service owns Track scope, runtime/pane generations,
action ordering and human/Planner handoff. #1563 now validates connection leases
when protocol frames are admitted and cleans up only that exact lease. Already
admitted/queued writes are not cancelled by that fix. The final write fence and
pending-AI cancellation remain requirements for the model interaction service.

Text entry must not imply Enter. Slash commands use the actual TUI's filtering,
navigation and confirmation. An acknowledged PTY write does not prove that the
application action completed; each action needs an observed result, and uncertain
writes must not be automatically repeated. A lost observation surface is
unavailable, never an old image represented as fresh state.

Terminal frames remain bounded runtime/artifact data. Any persisted action or
attachment contract needs explicit retention and lifecycle rules. Production
Terminal-card/WS integration, model tool authorization and actual astry image
receipt are still open; the developer preview does not satisfy those gates.

## Verification and execution environment

Follow repository gates and two isolated independent reviews for every code
slice. Use deterministic real-PTY and browser tests locally. Real astry/Planner
stack acceptance requires a dedicated development/test host; shared-production
host Codex E2E remains prohibited. Preview has separate ports, configuration,
data, release paths, supervisor socket, and model runtime.

Record actual commands/results per PR, plus preview URL, build SHA, runtime and
model ID, before/after images, and action results for each completed slice.

## Reproducible S0 screenshot probe

`scripts/spike/terminal-capture.mjs` is developer tooling, not a Planner tool or
the production observation service. It captures a terminal rectangle in an
already open managed browser without navigation, resizing, scrolling or input.
Its manifest deliberately says `pixels_only_unverified`: it has no atomic
text/state snapshot, output cursor, scope enforcement or model receipt proof.
Overlays within that rectangle are pixels too; this is not a production
redaction or content-isolation boundary.

Prepare `fe` dependencies with `npm ci` and install Playwright Chromium with
`npx playwright install chromium` from `fe`. Start this checkout's Vite server
on an unused port, for example:

```bash
cd fe
npm run dev -- --host 127.0.0.1 --port 5198 --strictPort
```

From the repository root, exercise the real `TerminalCardView` and xterm with
controlled WS output (no PTY or model is launched):

```bash
node scripts/spike/terminal-capture-check.mjs \
  --base-url http://127.0.0.1:5198 \
  --output-dir /tmp/terminal-capture-check-unique
```

The check saves a PNG with Chinese text and reverse-video menu highlighting,
rejects disconnects before and during capture, and asserts no terminal input.
It covers the actual nested card/surface DOM, not a hand-written replacement.
Inspect the PNG as part of the probe; these checks do not prove visual fidelity
for arbitrary TUI programs or genuine astry image input.

For a deliberately configured preview browser with a private CDP endpoint:

```bash
node scripts/spike/terminal-capture.mjs \
  --browser-url http://127.0.0.1:9222 \
  --page-url http://127.0.0.1:5198/next/track/TRACK_ID \
  --terminal-id TERMINAL_ID \
  --output-dir /tmp/terminal-capture-live-unique
```

The page URL must match exactly, the actual surface must be unique and fully
visible, and the output directory must not exist. A disconnected or moved
surface is rejected. The developer retains browser ownership; no CDP endpoint
is exposed to the Planner. This example does not set up a preview deployment.
