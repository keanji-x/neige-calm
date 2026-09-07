# Terminal computer use — implementation record (#1548)

## Outcome and order

The Planner using the user's astry model observes and operates the same terminal
as the human. Each slice ends with a preview loop: observe, decide, act, observe,
and verify. Issue #1548 defines S0–S6; none is complete merely because protocol
tests pass.

Baseline: `fb318f57` (2026-09-07). Model selection is now on main. PR #1535 still
owns composer attachment wiring. Terminal observations return through MCP tool
results and do not depend on the user-attachment upload path.

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

## Observation and control design

The Rust terminal interaction service owns identity, Track scope, terminal
generation, action ordering, and control handoff. Rendering is an explicit
surface interface. The initial preview binds a real browser xterm view; PNG,
text, geometry, cursor, active buffer, viewport offset, and applied output
position must be captured from a single rendered state.

Native browser region capture is the initial screenshot candidate. A spike
must establish its render barrier and its disconnect behavior before it becomes
a production service. Neither a test-only xterm dump nor the current incomplete
Rust TerminalModel is a substitute for the captured screen. No generic browser
navigation or CDP capability is given to the Planner.

Viewport scrolling changes the observation surface without PTY input.
Application scrolling sends an explicit wheel/key action. Text entry does not
implicitly press Enter. Slash-menu selection uses the actual TUI: type `/`,
observe the menu, filter/navigate, confirm, and observe the result.

Inputs are checked again at the final write boundary against the current
control generation. Human takeover cancels pending AI actions. A PTY write ACK
does not prove an application action completed. An unconfirmed write returns
an unknown outcome and is not automatically repeated. A disconnected surface
returns unavailable, never an old image disguised as a new observation.

Terminal frames remain bounded runtime/artifact data, not a stream of domain
events. Any persisted action or attachment contract gets an explicit retention
and lifecycle design before implementation. Browser-independent operation is a
later renderer decision with release/dependency and recovery acceptance, not an
implicit promise of the browser-bound preview.

## Verification and execution environment

Follow repository gates and two isolated independent reviews for every code
slice. Use deterministic real-PTY and browser tests locally. Real astry/Planner
stack acceptance requires a dedicated development/test host; shared-production
host Codex E2E remains prohibited. Preview has separate ports, configuration,
data, release paths, supervisor socket, and model runtime.

Record actual commands/results per PR, plus preview URL, build SHA, runtime and
model ID, before/after images, and action results for each completed slice.
