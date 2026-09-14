# Planner terminal round-trip cuts (#1666)

Companion to [Planner Terminal client wiring](1548-planner-terminal-wiring.md):
the discovery-schema rationale and the four round-trip cuts (#1666) moved
here unchanged so the wiring doc stays under the file-size target. Section
titles are kept so the wiring doc's pointers link by anchor.

## Model discovery schema

The four targeted tools are flat objects: `terminal_id` and `task_id` are both
optional root properties, and exactly-one targeting is enforced server-side by
`Target::from_ids` and stated as the first sentence of every description. Until
#1666 each tool also carried two closed `anyOf` selector arms (one per target,
each a copy of the whole common schema); that duplicated every root property
three times, and with the #1666 fields the input schema would have measured
5349 bytes with the arms against 1791 bytes without them, so the arms are gone.
Root common properties remain present for MCP clients that require an object
surface. The accepted request set is unchanged.

Input action variants use `anyOf`; their distinct required `type` literals keep
text/submit, key, click and sequence mutually exclusive. Existing `const`
discriminators are preserved: the inspected local Codex sanitizer converts them
to singleton enums. That parser does not retain `oneOf`, and its TypeScript
renderer handles union arms before sibling properties. Complete arms prevent
nested action fields from turning into an uninformative object in discovery;
`sequence.steps` items are typed as objects and validated server-side. The
registered input schemas stay below the local 4000-byte compaction threshold
(`schema_tests.rs` checks the serialized INPUT SCHEMA of each tool, not its
description). This local source inspection does not attest the installed Codex
build; fresh Planner discovery remains the end-to-end acceptance check.

Planner guidance uses exact Terminal tool names once and includes a complete
nested-action example. This changes discovery metadata and guidance only; input
execution, targeting guards and MCP result envelopes retain their existing paths.

## Round-trip cuts (#1666)

After #1619/#1621/#1632 the real Planner ran every Claude TUI scenario with zero
tool errors; its remaining asks (rounds 11 and 13) were all round-trip costs:
an extra observe to see a TUI's first screen, one call per key when editing a
draft, a claim and a release call per scenario, and stale refusals caused by a
hint line refreshing below the input box. Each has one explicit, opt-in shape.

### `wait_for=text` — wait for a target screen

`wait_text: [pattern, …]` (1..=8 literal strings, each 1..=200 bytes, no
control characters; required with `wait_for=text`, refused with any other
mode) on observe and on every action readback. The wait ends when any pattern
is a substring of any row of the live viewport (`Frame.text`, trailing spaces
trimmed) and the screen has then stayed quiet for `settle_ms`, or the process
exits / the client disconnects / the projection is invalidated (`exited`), or
the budget elapses (default 15000 ms when `wait_ms` is omitted, max 20000).
"Until the screen shows X", not "until X appears anew": a screen that already
matches at the first capture returns after `settle_ms` from the wait's start
with `wait.text.already: true`, so the Planner names the target state
(`["trust this folder","❯"]` for a Claude start). `wait_for=text` with
`scroll_offset > 0` is invalid params.

The loop (`terminal_interaction/text_wait.rs`, dispatched from `wait.rs`)
keeps the change-mode subscriptions (revision, protocol events, invalidation
through `stopped()`), captures `ModelView::capture(0)` exactly once per
revision wake with the view lock dropped before the rows are tested, never
on a protocol-event or timer wake alone, and re-reads the revision on a timer
wake before settling. On every revision the rows are re-tested: a match that
disappears before the quiet window ends returns the wait to "no match" (the
quiet window only counts while a match is present); tie-breaking is the first
pattern in argument order, then the first row top-down. Report: `wait.outcome`
gains `matched` (with `settled: false` when the budget ended with a match
present but not yet quiet) and `unmatched`; `wait.text` is `{pattern, row,
revision, already}` on a match (`revision` is the projection revision of the
capture that confirmed it; `observation_revision` can be later) or null.
`wait.baseline_revision` / `changed_since_previous_observation` are reported as
in every mode. The argument contract moved from `wait.rs` to
`terminal_interaction/wait_plan.rs` so the loop file did not grow.

### `sequence` — a bounded edit in one ordered write request

`{"type":"sequence","steps":[…]}`: 2..=8 steps, each a `text` or `key` action
with the same validation and `repeat` rules as the standalone actions. Keys
allowed inside a sequence: Left, Right, Up, Down, Home, End, Backspace, Delete,
Ctrl+U; everything else is rejected — Enter, Ctrl+J (LF), Escape, Tab,
Ctrl+C/D/L, PageUp/PageDown, click, submit, nested sequences. What the tool
guarantees: a sequence carries no CR and no LF. What it does not guarantee:
whether Up/Down/Ctrl+U/Home/End edit, recall history or do something else is
application-defined (the description says so). Total encoded size ≤ 16384
bytes. The encoding is the concatenation of the step encodings against the
live surface, sent as one ordered write request (`ClientMsg::Input`): one
barrier, one acknowledgement, one receipt (which adds `steps: n`) and one
fingerprint (the whole action; nested arrays hash deterministically). No
claim about OS-level write or read atomicity: the supervisor uses `write_all`
and the application may read the bytes in any chunking. The same fences, the
same stale result, the same readback; the recommended shape is `sequence` +
`observe: true, wait_for: change`, inspect the draft, then `submit`. Action
encoding lives in `terminal_interaction/actions.rs`.

### `claim` / `release` on input — control per scenario

`input claim: true` (default false). Order under the serial guard:
observation/binding/age/availability/pending checks → the checks that need no
live screen (live-viewport fence, the action encoded against the saved
surface, which the surface fence later proves equal to the live one), so a
claim is never granted on a request that errors anyway → claim → pre-write
capture and the remaining fences (availability and age again, control, surface,
the action against the live surface, revision) → write. Cases: this connection holds
control → no claim, `claim: {status: "held"}`, the ordinary fences apply
unchanged; the observation was taken as observer (`saved.control == None`) and
the connection holds no control → the same atomic `claim_if_unowned` as `open
claim:true` (pump verdict under the owner-registry lock, never displaces a
human), then wait for the grant delivery (`grants` counter) and re-read: the
None → Some(new control id) transition is authorized explicitly for this
observation (the equality fence is not re-run against the observer
observation); `owner != me` after delivery (a grant folded with a takeover) is
a refusal; then availability, pending, observation age, surface and revision
are re-checked on a fresh capture and the write proceeds with `control_id` and
`claim: {status: "claimed", control_id}` on the receipt. The control fence
authorizes exactly the granted lease: a takeover applied between the
post-grant re-read and the fence (the cached `control` no longer equals the
granted id) fails closed with the same `control_unavailable` result as a
folded takeover. Anything else (`saved.control` set but
no longer held, or another client owns the terminal) → no write, nothing
cached, `outcome: "control_unavailable"` with `reason`, `claim: {status:
"unavailable"}` and a fresh observation (same envelope as `stale_observation`,
`application_result: "unverified"`); a claim or delivery timeout (7 s) reports
`claim: {status: "unconfirmed"}`; a cancelled call may leave the claim granted
and the next `claim: true` on the connection reports `held`. A granted claim
followed by the stale fence carries `claim: {status: "claimed", control_id}` on
the stale result (its `next` says to resend with `observation_id` omitted:
the fresh observation is the connection's latest); every RPC error after a
granted claim ends with `; control claimed (control_id <id>)`, since an error
carries no receipt.

`input release: true` (default false). Order: write → ack/refusal/unknown (the
unknown/written/refused receipts already carry `release: {status:
"requested"}`, so the cached unknown receipt has the release fact) → release
(a helper that assumes the serial guard is held; the public `control()`
re-takes the guard and is never called) → the cached receipt is updated to
`release: {status: "released"}` (this connection held control — cache and
registry — before the release was sent and held none after it; a takeover
applied in between is reported as released too, since the outcome for the
caller is the same), `"not_held"` (the cache or the owner registry said this
connection did not hold control when the release ran, e.g. after a takeover)
or `"unconfirmed"` (send failure or 7 s timeout) → readback with the
pre-write baseline. A release never clears `pending`, never rewrites the write
outcome, and the readback shows the state after the release (`role: observer`
when released; after `unconfirmed` or `requested` the role may still be owner:
read `role`; text always carried: the release `text_omitted` economy is not
extended). A call cancelled between the write and the release update leaves
`"requested"` in the cached receipt: a replay returns it unchanged with a fresh
readback whose `role` says whether control is still held, and never releases.
`claim`, `release`, `allow_output_below_cursor` and
`allow_output_since_observation` enter the request fingerprint; a replayed
`request_id` never claims, releases or writes again. `control(action=claim|
release)` is unchanged. Helpers live in `terminal_interaction/input_control.rs`.

### `allow_output_below_cursor` — status-line refreshes are not stale

Each registered observation additionally stores the cursor `{row, column,
visible}` and one 64-bit hash per rendered row computed from the row's
`Frame.cells` (glyphs AND presentation — width, attributes, colours — so a
highlight change counts), computed from the capture already taken, outside the
registry lock (`terminal_interaction/screen_diff.rs`). `input
allow_output_below_cursor: true` (default false): when only the revision fence
fails, the live frame is compared with the observation and the write proceeds
iff the cursor is visible, within `0..rows` and identical, the surface fence
passed (already required; a hidden cursor flips an input mode and is refused
there first), `scroll_offset == 0`, the row count is unchanged and every row
with index ≤ cursor.row hashes identically — only rows strictly below the
cursor differ (possibly none: a revision can move without a textual or
presentational change). Anything else stays `stale_observation`.
`allow_output_since_observation: true` remains the wider opt-in and wins when
both are set. The tolerance is accepted only for draft edits — `text`,
`sequence` and a `key` from the sequence vocabulary — and refused (invalid
params at the MCP layer, the same refusal in `TerminalInteraction::input`)
for `submit`, `click`, Enter, Tab, Escape, control keys and PageUp/PageDown:
Claude Code's slash-command menu renders below the input row and re-sorts
while it loads, so an Enter admitted by the tolerance could pick a different
item than the one observed; a submission in a field whose status text moves
keeps using `allow_output_since_observation` after inspecting the fresh
state.

Receipt: when the tolerance admitted the write, `observation_drift` gains
`tolerance: "below_cursor"`, `rows_changed_below_cursor: [indices]` (first 16),
`rows_changed_total` and `truncated`. Every stale result gains `screen_diff:
{compared: {observed_revision, current_revision}, cursor: {moved, visible},
rows_changed_total, rows_changed_at_or_above_cursor,
rows_changed_below_cursor}` (counts; `rows_changed_total: 0` means the revision
moved without a textual/presentational change) and `next` names both flags and
says neither bypasses the control, surface, viewport or pending fences. This is
an opt-in text-and-presentation drift heuristic, not target equality: a
completion menu opening below a shell prompt or an editor popup below the
cursor leaves rows ≤ cursor intact and is not benign, invisible application
state is not seen, and hash collisions are theoretically possible. The Planner
opts in per request for input fields whose hint/status line refreshes (Claude
Code's draft box), never for menus or clicks. The check runs at admission like
the other fences (documented residual window unchanged).

### Measured one-write edits with Claude Code 2.1.259 (2026-09-13)

Real `claude` in a PTY (`pexpect` + `pyte`, 80×24), each edit sent as one
write request:

| Write | Draft afterwards |
|---|---|
| `7200 + 19` + Left×5 + Backspace + `9` | `7209 + 19` |
| `world` + Home + `hello ` | `hello world` |
| `old draft` + Ctrl+U + `new draft` | `new draft` |
| Left/Right mixes, CJK edits | as expected |

So a bounded edit can be one ordered write request, like `submit` (text + CR)
and `repeat` (Left×5) already are. The focused suite pins the same facts against the real
PTY with `stty raw -echo; exec cat -v` (the bytes arrive concatenated, in
order, as `7200 + 19^[[D^[[D^[[D^[[D^[[D^?9`) and against bash's readline
(the draft reads `echo 7209 + 19`).
