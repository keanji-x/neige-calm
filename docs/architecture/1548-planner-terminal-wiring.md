# Planner Terminal client wiring (#1548)

The application entry point is a Planner-only MCP tool set:

| Tool | Behavior |
|---|---|
| `calm.terminal.open` | Idempotent visible Terminal-card creation through `terminal-create` OperationRuntime, attributed to the authenticated Planner session. Returns text by default; `format=image` requests a PNG. Presentation does not change creation idempotency. |
| `calm.terminal.resolve` | Resolve an exact current task attempt or Terminal ID to its real Worker card, worker session and view availability. |
| `calm.terminal.observe` | Text/cursor/mode state and observation/connection IDs by default. Explicit `format=image` includes a PNG from that same captured RMUX frame. Reads never create or restart a process. |
| `calm.terminal.control` | Claim/release control, optionally returning fresh text with `observe=true`, or detach the model client while retaining the card/program. |
| `calm.terminal.input` | One text/key/cell-click action, bound to a recent live observation and current control; navigation/editing keys support bounded `repeat`. Optional `observe=true` returns fresh text after the action. A matching request ID replays its receipt without another write. |

## Model discovery schema

The four targeted tools expose complete, closed `anyOf` object arms for
`terminal_id` and `task_id`. Each arm derives from the same common schema, retains
all common properties and required fields, requires its selected target, and
excludes the other target property. Root common properties remain present for
MCP clients that require an object surface. The accepted request set is unchanged.

Input action variants also use `anyOf`; their distinct required `type` literals
keep text, key and click mutually exclusive. Existing `const` discriminators are
preserved: the inspected local Codex sanitizer converts them to singleton enums.
That parser does not retain `oneOf`, and its TypeScript renderer handles union
arms before sibling properties. Complete arms prevent nested action fields from
turning into an uninformative object in discovery. The registered schemas stay
below the local 4000-byte compaction threshold. This local source inspection does
not attest the installed Codex build; fresh Planner discovery remains the end-to-end
acceptance check.

Planner guidance uses exact Terminal tool names once and includes a complete
nested-action example. This changes discovery metadata and guidance only; input
execution, targeting guards and MCP result envelopes retain their existing paths.

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

Open and observe accept only `format=text` (the default) or `format=image`.
Normal observations return one MCP text block and structured metadata, including
a valid observation ID for input. They never initialize system fonts or rasterize
an image. Input still requires control, a fresh live observation, and the same
session, revision and authority checks in either format. Observing after each
action does not require taking a screenshot.

Use `format=image` for color, reverse-video selection or layout-dependent TUI
decisions; plain text does not preserve these visual cues. Image replies include
a native MCP PNG block and `image_source`; text replies omit both. For `observe`,
an explicit image error is returned as the call's error, never as a text
observation. `open` is the exception: once the create (and the claim) succeeded,
an image that cannot be rendered never fails the open; the text observation is
returned with `image: {status: unavailable, reason}` and no PNG (see the
composite-actions section). Both formats use one immutable captured frame for
their metadata and any image.

For explicit image observations, the frame is immutable before rasterization. System-font-only `resvg` renders
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
the physical PTY write. Queued stale writes are refused. The input-surface
fence (size, modes, alternate screen) is evaluated once, at admission of the
tool call before the write is queued. A surface change between that check and
the physical PTY write is not re-checked; the queued write still revalidates
scope, ownership lease and task binding. If a sent write loses its
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
Text excludes control characters and never implicitly submits. Ctrl+J explicitly
sends LF (0x0a), distinct from Enter's CR (0x0d). Claude Code documents Ctrl+J
as [draft newline](https://code.claude.com/docs/en/keybindings); other applications define their own behavior, so LF is not a
generic no-submit guarantee. Enter, Escape,
arrows and other supported keys are explicit actions. Application mouse input
requires reported SGR mouse mode and in-range cell coordinates. Local history
scrolling uses `observe.scroll_offset`; application paging uses explicit keys.

## Optional action observation and repeated navigation

Control (claim/release) and input accept `observe=true` with optional `wait_ms`
(0..20000; see the #1618 section for `wait_for`/`settle_ms`). They retain the original receipt fields and add exactly one of:

```json
{"observation":{"status":"available","state":{"observation_id":"...","text":["..."],"control_id":"...","role":"owner"}}}
{"observation":{"status":"unavailable","reason":"..."}}
```

`state` is the full existing text observation metadata, not just the abbreviated
example. These responses never include PNG. The default remains receipt-only;
`wait_ms` without `observe=true`, or detach with observation, is rejected before
any action. Fixed waiting does not certify application completion. A readback
failure never erases or changes the action receipt, including written/unknown
input. An unavailable observation must not cause a new input request.

Readback uses the action's existing client, renderer generation and execution
binding. It cannot attach a client or silently follow a replacement session.
It rechecks current state after waiting; a human takeover does not claim control
again. Presentation options do not enter the physical action fingerprint/cache.
Repeating an identical request may obtain a fresh observation without another
physical write; receipt-only replay still returns the original receipt.

A Planner can claim with observation, type with observation, inspect the text,
and send Enter against that fresh observation in three calls. Input accepts the
returned `observation_id`; `control_id` is informational, not an input argument.

Key actions accept optional integer `repeat` (1..32, default 1). Counts above one
are restricted to Left/Right/Up/Down/Backspace/Delete. Enter/Escape/Tab/control keys
cannot repeat; null, noninteger and out-of-range counts fail before input. The
repeated encoded bytes travel in one existing input request under one ownership
barrier. Repeat belongs to the physical action fingerprint. This does not add
mixed-action batches or automatic Enter; the only relaxed revision check is the
explicit `allow_output_since_observation` fence below (#1618).

## Change waiting, drift-tolerant input and implicit observation (#1618)

`calm.terminal.observe` and the `observe=true` readbacks of control and input
accept `wait_for` (`elapsed`, default, or `change`), `wait_ms` (0..20000, the
budget for either mode; when omitted it is 0 for `elapsed` and 2000 for
`change`, so the prompt's recommended `observe=true, wait_for=change` readback
actually waits) and `settle_ms` (0..2000, default 150; rejected unless
`wait_for=change`). `change` returns once the model projection's revision differs
from the baseline and no further revision arrived for `settle_ms`, or at the
budget, or when the process exited, the client became unavailable or the
projection was invalidated (`ModelView::invalidate` wakes subscribers without
a revision; the wait stops there and the capture after it fails explicitly
instead of idling to the budget). Only a new revision starts or extends the
quiet window; protocol events (acks, ownership)
do not, and when the quiet timer completes the revision and exit state are read
again before `settled:true` is reported, because `select!` may pick the timer
while a newer revision notification is already ready. Baselines:
observe uses this connection's previous observation revision (the revision at
call start when there is none); an input readback uses the revision read
immediately before the physical write; a control readback uses the revision at
call start. The wait selects over `ModelView`'s `watch<u64>` revision channel
(published under the same lock as the revision) and the client's protocol
watch; there is no sleep-poll loop. Every observation, in either mode, carries:

```json
"wait":{"mode":"change","outcome":"changed|unchanged|exited|elapsed","waited_ms":812,"settled":true,"baseline_revision":"41"},
"changed_since_previous_observation":true,
"previous_observation_revision":"41"
```

`elapsed` mode reports `outcome:"elapsed"`, `settled:false`, `waited_ms` equal to
the budget. `unchanged`/settled screens are not completion evidence; the prompt
says so. `changed_since_previous_observation` is false when the connection had
no previous observation.

Baseline transparency (rounds 07/08): `wait.baseline_revision` is the revision
the wait compared against, as a string like `observation_revision`; it is
reported in elapsed mode too, where it is the same baseline a change wait
would have used (the previous observation for observe, the pre-write revision
for an input readback, the call-start revision for a control readback).
`previous_observation_revision` is the revision of this connection's previous
observation, or `null` on a fresh connection, and is what
`changed_since_previous_observation` is computed from. The two differ after an
action: an input readback's wait baseline is the pre-write read, while
`changed_since_previous_observation` still compares with the last observation
the Planner saw.

Input accepts `allow_output_since_observation` (default false; part of the
request fingerprint). When false the exact-revision fence is unchanged. When
true the fence becomes: same binding and connection, observation younger than
120 s, `control` unchanged and present, client available, not exited, no pending
unknown write, `scroll_offset == 0`, and the observation's input surface (cols,
rows, modes, alternate) equal to the live frame's; bytes are encoded against the
live surface. The receipt reports `output_since_observation` and, when true,
`observation_drift: {observed_revision, input_revision}` (numbers). A resize, an
input-mode change (for example application cursor keys) or an alternate-screen
switch in either direction is refused with a surface-changed error even with
the flag. The alternate screen is compared as the projection's `alternate`
flag: rmux tracks it through the saved grid, not through a mode bit, so a
modes-only comparison would let a menu that appeared over the shell pass.

Under the same fence the action is validated (encoded against the live
surface) before stale is decided, so an invalid action — Enter with `repeat`,
a click without mouse mode — is an RPC error whatever the revision did; only
the exact-revision fence is relaxed by the stale result. Write authority
(`check_binding(write)`: task running, worker session active) is decided
under the connection's serial lock, after any readback in progress, so an
input queued behind a long readback sees the task state as it is when its
turn comes; a task that finished during the queue refuses the input rather
than answering `stale_observation`.

`observation_id` is optional on input. When omitted the server uses the latest
observation captured on this client connection (any format, including action
readbacks and the fresh observation of a stale result); the receipt reports
`observation_id_used`. All fences still apply. A connection with no
observation is refused ("observe first"). The fingerprint hashes
`observation_id` as given (null when omitted), so a replayed `request_id`
returns the same receipt. The prompt's input example omits `observation_id`
and says to pass it only after an image observation or to act deliberately on
an older observation.

### Structured stale refusal (rounds 07/08)

In round 07 three inputs were refused with the RPC error "terminal changed
since observation; observe again" because only Claude's bottom status line had
changed between a settled readback and the next input; each refusal cost a
separate observe round trip. Now, when every other fence passes (binding,
connection, age, availability, no pending unknown write, control, live
viewport and input surface) and only the exact revision differs, input returns
a successful tool result instead of an error:

```json
{"terminal_id":"..","request_id":"enter-3","outcome":"stale_observation","application_result":"unverified",
 "observation_id_used":"<old>","observed_revision":41,"current_revision":42,
 "next":"inspect observation.state; if only status text changed, resend the same request_id with allow_output_since_observation=true, else act on the new state",
 "observation":{"status":"available","state":{"observation_id":"<fresh>","text":["..."],"previous_observation_revision":"41", ...}}}
```

No physical write happens and nothing is cached under the `request_id`, so a
later resend with a different flag or observation id does not conflict. The
fresh observation is a text capture taken at once and registered as this
connection's latest, so the advised resend may omit `observation_id`; when the
capture fails the receipt says `observation:{"status":"unavailable","reason"}`.
`observed_revision`/`current_revision` are numbers like `observation_drift`.
Every other refusal — including a revision change combined with a control or
surface change — stays an RPC error; without the flag the surface fence is
now checked too, so a resize plus output reports the surface error rather than
inviting a flagged resend. The summary line reads `input stale_observation
readback available`.

### Release readback economy (rounds 07/08)

A release rarely changes the screen, and its readback repeated the unchanged
text. For `control` action `release` with `observe=true`, when the captured
revision equals this connection's previous observation revision (read before
the readback registers itself) and both captures are live viewports
(`scroll_offset` 0; the connection's latest observation stores its offset),
`observation.state` omits the `text` array and carries
`"text_omitted":"unchanged since previous observation <id>"`; every other
field (ids, revision, geometry, cursor, wait, task status) stays. A history
view shares the live revision but not its text, so after one the release
readback includes the text. After output the text is included as before. Claim readbacks and observe always
include text. The tool descriptions and prompt also say that claim/release
readbacks should use `wait_for=elapsed` or a change budget of at most 500 ms,
keeping long change budgets for program output after Enter, and that
`allow_output_since_observation=true` is for Escape/Ctrl+C while a program
streams and for typing or submitting in an input field whose surrounding
status text keeps changing (after inspecting the fresh state), never for menu
selection or clicks.

Receipts: input drops `application_completed` and reports
`application_result:"unverified"` on every outcome — written, refused and
unknown (enqueue failure, acknowledgement timeout, cancelled replay) — plus
`next` on acknowledged ones. Detach returns
`{"detached":true,"had_client":bool,"terminal_id":..,"connection_id":<closed or null>,"terminal_session_id":<closed client's or null>}`;
without a client the terminal id comes from a read-only target resolution when
possible.

Text results of the five terminal tools keep the complete state only in
`structuredContent`; `content[0].text` is a one-line summary (ids, revision,
role, geometry, cursor, wait outcome, receipt facts, or the operation id and
outcome of an open that did not succeed) and never contains screen text.
Collectors that read terminal results must read `structuredContent`; the
summary is not parseable metadata. The UX collector counts a completed input
whose receipt outcome is `stale_observation` as an observation refusal (next
to the error-string refusals) and accepts a release readback whose state has
`text_omitted` instead of `text` without adding an observation entry for it.

Readback ordering: an action readback re-resolves the target after its wait,
not only before it. The wait (up to 20 s) can span a task completion or an
authority change, so `task_status` and `controllable` in the returned state
are the post-wait values; if the execution binding changed during the wait the
readback is `unavailable` with the reason and the action receipt stands.

Serialization: one connection runs one action at a time, and the readback wait
is part of the action. A second input or control call from the same Planner on
the same terminal queues behind a readback in progress (bounded by the wait
budget) instead of writing into the screen the first call is still waiting to
read back. Releasing the serial before the readback would keep the fences
sound (the pending reservation is cleared by the acknowledgement and the
revision fence still applies) but would let the second write end the first
wait with output that is not the first action's reply, so the readback stays
inside the serialized section. Connections of other Planners or humans are not
serialized by it. Image results keep their metadata text block and native PNG block. A probe
of Codex 0.153.4 showed the model receives both `content` and
`structuredContent` verbatim, so the duplicate state was real.

## Provider approval entry point (#1578)

Terminal writes retain truthful `readOnlyHint: false`, `destructiveHint: true`
and `openWorldHint: true` annotations. The Planner's `approvalPolicy: never`
otherwise rejects them before the MCP server sees the call. The application
therefore explicitly sets `mcp_servers.calm.tools.<tool>.approval_mode = "approve"`
for exactly `calm.terminal.open`, `calm.terminal.control` and
`calm.terminal.input` on Planner threads. This uses the provider's
[per-tool configuration](https://learn.chatgpt.com/docs/config-file/config-reference),
not a server-wide approval default or an annotation shortcut.

This delegates the provider prompt decision to the kernel's existing authenticated
Planner authority, which already includes same-Track terminal task execution.
The live role, Track, task/session, observation and human-control checks still
apply, including at the queued write boundary. It does not authorize actions
outside that contract, or certify that a TUI completed an operation.

The required card role is carried through typed thread configuration. Fresh
Planner starts and valid cold resumes use the same producer; cold resume reads
the current persisted card role. Assistant, Worker and plain-chat threads gain
no tool approval override. Unknown card roles do not receive a policy. Global
approval/sandbox settings and daemon-wide MCP configuration remain unchanged.
A hot takeover retains its already-loaded provider thread configuration, so a
new Planner thread or a proper cold daemon restart is needed to adopt the policy.

A model-free probe with the actual Codex 0.153.4 app-server reproduced the exact
`MCP tool call requires approval, but approval policy is never` error, then
confirmed the named tool enters MCP with this override while an unrelated write
still gets refused. A local synthetic Responses endpoint emitted the tool calls;
this is provider-policy evidence, not an actual astry autonomous acceptance run.

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

## Hook signals and composite actions (#1620)

After #1618 the Planner still inferred "is Claude done / waiting for me?" from
pixels. #1620 adds explicit application state for terminals the Planner opens,
reusing the Claude Worker hook transport, plus two bounded composite actions.

### Transport

`calm.terminal.open` submits the terminal-create operation with
`planner_hooks: true`. The adapter allocates the card id in `prepare_tx`, derives
the env from it and persists identical values in the terminal row and the spawn
output: `NEIGE_CLAUDE_SETTINGS=<data_dir>/terminal-hooks/<card_id>.json`,
`NEIGE_CARD_ID`, `NEIGE_CALM_BASE_URL`, `NEIGE_HOOK_PROVIDER=claude`,
`NEIGE_HOOK_URL` (the bridge's existing env contract). The spawn side effect
writes the hooks-only settings file (mkdir → write → spawn, so operation recovery
re-creates it) before the child starts. The file registers exactly seven events —
SessionStart, UserPromptSubmit, Stop, Notification, PermissionRequest,
SessionEnd, SubagentStop — each running `neige-codex-bridge --provider claude`,
which POSTs the payload to `/internal/claude/hook?card_id=` (loopback,
`X-Calm-Actor: ai:claude`). Nothing is installed into `~/.claude` or the project;
the Planner starts Claude with `claude --settings "$NEIGE_CLAUDE_SETTINGS"`.
Human-created terminals (`POST /api/tracks/:id/terminal-cards`) keep
`planner_hooks: false` and get exactly the env they asked for.

Idempotency: the open's `stable_payload_hash` covers the request as sent plus
`planner_hooks`; the generated keys never enter it (they are derived after
hashing, from the allocated card id), so a replayed `request_id` returns the
existing terminal. The settings file is deleted by the shared terminal reap
(`reap_terminal_artifacts_with_renderer`: card delete, sweeper, create
compensation) and, for track and area deletion (which quiesce terminals
through `quiesce_terminal_artifacts_for_deletion` and never reach the reap
helper), on the committed arm of the delete only — the card ids are captured
before the transaction and the files stay in place on rollback. Only the path
derived from the server-owned directory and the card id is ever deleted, never
a path read from env.

Occurrence identity: Claude's `Stop` and idle `Notification` bodies are
byte-identical every turn, and the ingest key is
`provider|card|session|event|sha256(body)`. The bridge therefore stamps
`neige_hook_occurrence: "<pid>-<captured_ms>-<random>"` into every body it
posts, generated once per bridge process and kept across its retries and the
replayable fallback file (whose stem hashes the stamped body): distinct
invocations are distinct events, a redelivery of one invocation is still a
duplicate. Server keys are unchanged; the field travels with the payload. The
stamp is not terminal-specific: two invocations of a worker (codex or claude)
hook with byte-identical bodies now persist as two hook events, where the
worker dedupe cache previously collapsed the second into the first.

### Ingest → ring

`ingest_provider_hook` resolves the card first: for a `kind == "terminal"` card it
parses the body into a bounded signal (`{seq, event (snake_case), notification_type?,
message (≤200 chars, control characters stripped; taken from `message`, else
`prompt`, else `reason`), claude_session_id?, received_at_ms}`), appends it to the
CURRENT renderer entry's ring (`RendererEntry.signals`, capacity 64, under the
registry lock so a superseded generation never receives it) and returns the
same 2xx — BEFORE the worker dedupe cache and the persist / FSM projection
path, so a terminal hook can never move a card FSM and never occupies a slot
of the bounded (4096-key) worker `hook_ingest_cache`: a flood of terminal
hooks, malformed ones included, cannot evict a worker key. Dedupe for
terminals is the ring's own recent-key set (128 keys per terminal, keyed by
the same ingest key, which covers the bridge's occurrence id), so a
redelivery never appends twice while every accepted event gets a fresh `seq`.
Malformed or unknown payloads are logged and acknowledged; codex hooks for
terminal cards are acknowledged and ignored. A `watch<u64>` seq is published
under the ring lock (`send_modify`, retained with zero subscribers).

### Observation and waiting

Every observation carries `signals: {hooks_seen, last_seq,
since_previous_observation (≤20, newest last), dropped_since_previous_observation}`,
read atomically with `last_seq` under the ring lock; the per-connection
`LatestObservation` records `last_seq` so the baseline is per connection like
revisions, and advancing it cannot skip an unlisted signal.

`wait_for: "signal"` (budget default 15000 ms, max 20000, `settle_ms` refused,
optional `signal_events` from the seven-event vocabulary, default
`stop, notification, permission_request, session_end`) ends when a signal with
`seq > wait.baseline_signal_seq` and a matching event exists, on process exit,
disconnect or projection invalidation (the attach stream failing — the same
`stopped()` / `projection_unavailable` treatment as change mode, so the wait
and the connection's input serial are never parked to the budget), or at the
budget (`outcome: unchanged`). `wait.outcome` adds `signal` and `wait.signal`
carries the matching signal (null otherwise). The loop subscribes to the seq
channel, the projection revision channel and the protocol channel, marks the
versions seen, inspects the ring before every select and again on timeout,
and re-reads `stopped` on timeout (an exit coinciding with the deadline is
`exited`, not `unchanged`). Baselines: observe → the previous observation's `last_seq`
on this connection, else the seq at call start; input readback → the seq read
immediately before the physical write (so a hook caused by the write is
reported); control readback → the seq at call start; a readback of a cached
(replayed) `request_id` → the state at that later call.

### Composite actions

* `input` action `{"type":"submit","text":"..."}`: the same validation as `text`
  (nonempty, ≤16384 bytes, no control characters), encoded as the text bytes
  plus one CR in ONE `ClientMsg::Input`, one receipt, one barrier; `repeat` is
  refused. The plain `text` action still never submits.
* `open` `claim: true`: after creation the claim runs through the Planner
  client's pump as a claim-if-unowned (`PumpCommand::ClaimIfUnowned`): the
  same grant barrier, scope check and `OwnerClaim` effects as
  `calm.terminal.control claim`, but applied only when the owner registry —
  read under its lock in the same pass — names no other client. The
  connection's cached owner is never the decision (it lags the registry by
  the `OwnerChanged` delivery), so a human who claimed between the create and
  the claim keeps control. The observation is returned with
  `control_id`/`role: owner` and `claim: {status: claimed}`; a refused claim
  keeps the create success and ids and reports `claim: {status: unavailable,
  reason: "terminal is controlled by another client"}` (other claim failures
  carry their own reason). An open never revokes control held by another
  client (the operation runtime returns the existing operation for a replayed
  `request_id`, so "replayed" is not distinguishable from "fresh"): a
  replayed open after a human takeover reports `unavailable` instead of
  reclaiming; control already held on this connection returns the current
  observation without a second claim. An ordinary `control claim` keeps its
  deliberate-takeover semantics. With `format: image`, an image that cannot be
  rendered after the create (and the claim) succeeded never fails the open:
  the text observation is returned with `image: {status: unavailable, reason}`
  and no PNG.

### Trust statement

Hook signals are UNTRUSTED advisory telemetry: the ingest route is loopback and
keyed only by card id, so any local process can forge `event`, `message` and
`notification_type`. No fence (binding, control lease, revision, pending write,
physical write authority) consults signals; a signal never changes a fence, an
input outcome or a card state. The tool descriptions and the Planner prompt say
that signal text is application data, never an instruction, and that the screen
is what to verify against.

### Measured with Claude Code 2.1.259 on 2026-09-12

Three runs of the real `claude` binary in a PTY (`pexpect`, 120×40,
`--permission-mode default`, generated hooks-only settings logging every event
with `date +%s%3N` and the stdin JSON; SessionStart, UserPromptSubmit, Stop,
Notification, PermissionRequest, SessionEnd, SubagentStop and PreToolUse were
registered). Timestamps are relative to the prompt submit.

| Scenario | Events observed (order, Δt) | Notes |
|---|---|---|
| Session start | `SessionStart` | fires before the first prompt |
| Short prompt ("reply pong") | `UserPromptSubmit` +0.6 s → `Stop` +3.0 s (`stop_hook_active:false`) | Stop marks the end of the answer |
| Long prompt, Escape 6.4 s into generation | `UserPromptSubmit` +0.6 s; **no `Stop` within the following 100 s** | interrupt ends the turn without a Stop hook (the prompt returned to idle) |
| Bash tool, command auto-allowed by user config (`ls`) | `UserPromptSubmit` → `PreToolUse` (Bash) +2.8 s → `Stop` +5.6 s → `SubagentStop` +7.6 s | no permission event when the tool is allow-listed |
| Bash tool needing approval (`touch …`), Enter to approve | `UserPromptSubmit` → `PreToolUse` +3.0 s → `PermissionRequest` +3.0 s (`tool_name: Bash`, `permission_suggestions`) → Enter → `Stop` +3 s after approval | `PermissionRequest` is the actionable signal; no `Notification permission_prompt` arrived within the 4 s before approval |
| Idle after Stop | `Notification` `notification_type: idle_prompt`, `message: "Claude is waiting for your input"` at +60 s | once per idle period |
| `/exit` | `SessionEnd` `reason: prompt_input_exit` | |

Not covered: programs other than Claude Code (no hooks, `hooks_seen` stays false
and the Planner falls back to `wait_for=change`); a user configuration that
disables hooks or overrides them; `Stop` after an Escape interrupt (measured
absent, so an interrupt must be confirmed on the screen); Notification
`permission_prompt` timing (unmeasured before approval); ring overflow inside
a wait — 64 or more signals arriving between two inspections of the ring can
evict a matching one before `first_matching` sees it, so the wait may run to
its budget although the event happened; only
`signals.dropped_since_previous_observation` on the next observation reveals
it.

## Acceptance evidence and limits

The focused suite uses the actual authenticated MCP UDS server, real operation
runtime and renderer, and a real shell. It verifies visible card identity, native
default text-only envelopes, explicit native PNG delivery, format-independent open
idempotency, exact application output, physical Enter counts for duplicate
requests, same-database foreign-Track refusal, human takeover and reconnect IDs.
The writer tests hold real protocol work and physical acknowledgement separately.
Projection tests traverse actual RenderPlane output and resize paths.

A developer driver reuses only this test setup and calls the actual tools; it is
not a Planner implementation. It accepts private NDJSON stdin and starts a
disposable loopback browser preview. It forwards observation formats unchanged:

```json
{"name":"calm.terminal.observe","arguments":{"terminal_id":"<returned-terminal-id>"}}
{"name":"calm.terminal.observe","arguments":{"terminal_id":"<returned-terminal-id>","format":"image"}}
```

The first reads text without a screenshot; the second explicitly asks for one.
Build with:

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
