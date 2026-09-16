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
control characters) on observe, open and every action readback. Since #1677
r16 the rule is: `wait_for=text` requires at least one of `wait_text` /
`wait_text_absent`, `wait_for=signal` accepts either as conditions on its
repaint phase, and change / elapsed mode refuse both (see "Text conditions on
waits"). In text mode the wait ends when the conditions hold — for
`wait_text`, any pattern is a substring of any row of the live viewport
(`Frame.text`, trailing spaces trimmed) — and the screen has then stayed quiet
for `settle_ms`, or the process exits / the client disconnects / the
projection is invalidated (`exited`), or the budget elapses (default 15000 ms
when `wait_ms` is omitted, max 20000).
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
claim is never granted on an invalid action or a history-view observation (a
surface change since the observation, e.g. a resize, needs the live screen:
it still claims and then errors with the claim note) → claim → pre-write
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
or `"unconfirmed"` (send failure or 7 s timeout; on an exited terminal the
registry-confirmed step of #1697 below, with its `reason`) → readback with the
pre-write baseline. A release never clears `pending`, never rewrites the write
outcome, and the readback shows the state after the release (`role: observer`
when released; after `unconfirmed` or `requested` the role may still be owner:
read `role`; text always carried: the release `text_omitted` economy is not
extended). A call cancelled between the write and the release update leaves
`"requested"` in the cached receipt: a replay returns it unchanged with a fresh
readback whose `role` says whether control is still held, and never releases.
`claim`, `release`, `allow_output_below_cursor` and
`allow_output_since_observation` enter the request fingerprint; a replayed
`request_id` never claims, releases or writes again. `control(action=claim)`
is unchanged; `control(action=release)` runs the same release step since
#1697. Helpers live in `terminal_interaction/input_control.rs`.

#### Release on an exited terminal (#1697)

Round 20: `control release` on a terminal whose program had exited failed
after 7 s with tokio's `deadline has elapsed`. The client pump's downstream
task stops forwarding after `TerminalExited` (the WS client closes there), so
the `OwnerChanged(None)` the release produces never reaches this connection's
mirror (`ScreenState`), while the pump still applied the `OwnerRelease` to
the owner registry. Now one step (`ReleaseStep` in `input_control.rs`)
serves `input release:true` and `control release`: `not_held` when the
mirror shows no lease or the registry does not name this connection;
`OwnerRelease` sent (a send failure is `unconfirmed` with the reason); when
the mirror says `exited`, the registry — the truth; the mirror is a cache
the pump stopped feeding at exit — is polled every 20 ms for up to 1 s until
it no longer names this connection, the mirror's `owner`/`control` are set
from it and the status is `released` (otherwise `unconfirmed`, reason
`terminal exited; release not confirmed`); on a live terminal the mirror's
`OwnerChanged` is awaited for 7 s as before. The control receipt carries `release: {status,
reason?}` like the input path (so `summary.release` fills for control
releases too) and the call no longer fails on an unconfirmed release; the
readback still runs. A `claim` on an exited terminal is the binding refusal
(`controllable: false`), not a wait. Observations also carry `exit_code`
from the `TerminalExited` frame (null until the exit or when unknown).

#### An exited terminal survives the orphan sweeper (#1701)

Round 21: 76 s after a one-shot program exited (release confirmed, `exit_code:
0` read), the orphan sweeper reaped the terminal, and a later `observe` /
`control release` on it failed with `target has no terminal view`. The
ephemeral terminal session completes on PTY exit, so the row matched
`terminals_orphaned` (no active session, older than the 60 s grace) although
the attach reader had recorded the exit and the Terminal card still existed.
The query now skips a `terminal`-kind card's row with a recorded exit
(`exit_code IS NOT NULL OR signal_killed = 1`): the row and renderer entry
follow the card and go with it on card / track / area delete, so the final
screen, scrollback and `exit_code` stay observable and the release above
keeps answering through the registry. A row without a recorded exit is still
residue and reaped; other card kinds are unchanged. The refusal for a row
that is gone reads `terminal not found: deleted with its card or reaped as
residue`.

### `allow_output_below_cursor` — status-line refreshes are not stale

Each registered observation additionally stores the cursor `{row, column,
visible}` and one 64-bit hash per rendered row computed from the row's
`Frame.cells` (glyphs AND presentation — width, attributes, colours — so a
highlight change counts), computed from the capture already taken, outside the
registry lock (`terminal_interaction/screen_diff.rs`). `input
allow_output_below_cursor: true` (default false): when only the revision fence
fails, the live frame is compared with the observation and the write proceeds
iff the cursor is within `0..rows` and identical (position and visibility;
#1677: it may be hidden in both captures — a visibility CHANGE flips an input
mode and is refused by the surface fence first, which is already required),
`scroll_offset == 0`, the row count is unchanged and every row with index ≤
cursor.row hashes identically — only rows strictly below the cursor differ
(possibly none: a revision can move without a textual or presentational
change). Anything else stays `stale_observation`.
`allow_output_since_observation: true` remains the wider opt-in and wins when
both are set. The tolerance is accepted only for draft edits — `text`,
`sequence` and a `key` from the sequence vocabulary — and refused (invalid
params at the MCP layer, the same refusal in `TerminalInteraction::input`)
for `submit`, `click`, Enter, Tab, Escape, other control keys and PageUp/PageDown:
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

#1684 (Planner ask, round 17): a write that `allow_output_since_observation`
admitted after the revision moved runs the same comparison on the frame the
fences captured and reports it, so the Planner sees what its wide opt-in let
through without observing again. `observation_drift` then carries
`tolerance: "output_since_observation"`, `cursor: {moved, visible}`,
`rows_changed_total`, `rows_changed_at_or_above_cursor` and
`rows_changed_below_cursor` (both counts, as on a stale result),
`rows_changed: [indices]` (the first 16 across both classes, at-or-above
first) and `truncated` (judged on the total). The below-cursor shape above is
unchanged (its `rows_changed_below_cursor` stays the index list); when both
flags are set the wide one admits and its shape is reported. No fence moved:
the wide flag admits whatever the comparison says, and an input on the exact
revision still carries no `observation_drift` at all. Every write receipt of
the request, the cached unknown one included, carries the same rows, so a
replay returns them without recomputing.

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

## Open with a wait, replace in the draft, receipt summary (#1677)

Round 14 (#1666) ran the Claude TUI scenarios in 16 terminal tool calls
with zero errors; the interview left three asks, all round trips or reading
cost: starting a program and waiting for its first screen took an open plus
a submit with `wait_for=text`; fixing one number in a draft meant counting
characters (CJK width included) to build a `sequence`; and the facts that
matter on a receipt (screen change, hook signal, repaint, control state) sat
several levels down. Each has one explicit shape.

### `open` waits like a readback

`calm.terminal.open` accepts `wait_for`, `wait_ms`, `settle_ms`,
`signal_events`, `repaint_ms`, `wait_text` and (r16) `wait_text_absent` with observe's semantics and
validation (`WaitPlan::new`, checked before the create operation is
submitted: an invalid wait creates no card). Order inside the handler:
create (or the idempotent replay, `SucceededViaCollision` included) →
immediate text observation (establishes the client, as before) → when
`claim:true`, `claim_after_open` with an immediate readback → the final
observation with the wait plan and the requested format, which is what the
open returns with `claim {status}` and the ids attached. The wait therefore
runs after the claim, never inside `claim_after_open`'s serial guard, and a
failed claim still returns the waited state with `claim {status:
unavailable, reason}`. Its baseline is the connection's previous observation
(the immediate read, or the claim readback), as `capture` already does: a
change wait waits for a change after the open/claim, a text wait ignores the
baseline and matches a screen that is already present (`already: true`).
Without wait arguments an open returns as before (the immediate read, or the
claim readback). `format=image` with a wait is one capture (wait → frame →
render); when the render fails the open keeps its #1620 F6 behaviour and
falls back to an immediate text observation plus `image {status:
unavailable, reason}`: the screen shown is the post-wait one, but that
fallback's `wait` block is the immediate read's (a known limitation; no round
has used an image). The operation runtime's deadline covers only the create;
the wait (≤ 20 s) and the claim (≤ 7 s) run after it inside the MCP call, as
observe's 20 s already does. The wait arguments never enter
`open_payload_hash` (like `format` and `claim`): a replayed request_id
returns the same terminal and runs the wait as asked.

`program` is a `/bin/sh -c` command line run with the terminal's env
(`routes/terminal.rs`: `args: ["-c", program]`, hook env merged by the
Planner create adapter), so `$NEIGE_CLAUDE_SETTINGS` expands and there is no
persistent shell once the command line exits. The recommended Claude start is
one call: `{"request_id":"…","program":"claude --settings
\"$NEIGE_CLAUDE_SETTINGS\"","claim":true,"wait_for":"text","wait_text":["trust
this folder","❯"]}` → the trust dialog (or the prompt) with control held.

### `replace` — the server derives the edit from the screen

`{"type":"replace","from":"11","to":"19"}`: `from` nonempty printable text
(≤ 200 bytes), `to` printable (may be empty = delete), neither with control
characters (so no CR/LF), no other fields. The shape is checked where every
action's shape is checked (`actions.rs`, before the claim; `encode` returns
`Encoded::Replace` instead of bytes). The plan is derived from the live frame
captured at the pre-write fences, only after the revision/tolerance admission
(a stale observation is reported as `stale_observation` before any lookup),
in `terminal_interaction/replace_plan.rs`:

* the cursor row is `cells[row*cols..(row+1)*cols]`; a continuation cell of
  a wide glyph (`width == 0`, text `" "`, measured against rmux-core 0.10.0)
  is skipped; blank cells are kept (one character each: the plan assumes the
  row is the application's line buffer with one character per non-padding
  cell); the cursor column is a zero-based cell column and a cursor inside a
  wide cell (`start < column < start+width`) or off the row is refused; the
  cursor may be hidden — round 15 on Claude Code 2.1.259 observed `cursor
  {column: 29, row: 12, visible: false}` on the draft `❯ 请只回答 7200 + 11
  的结果。`: Claude Code keeps DECTCEM off in its draft box while positioning
  the cursor at the edit point, so the plan uses the position whether or not
  the cursor is shown and the receipt reports `cursor_visible` for audit;
* the character index at a boundary is Σ `cell.text.chars().count()` over
  the non-padding cells before it; `from` is searched in the row string built
  from those cells in the same scalar convention, counting overlapping
  occurrences (`aaa` holds two `aa`); exactly one occurrence is required;
  review r1 A: the application moves and erases per CELL while the plan
  counts scalars, so a cell holding several scalars (a combining sequence,
  an emoji) inside the match or between the match and the cursor is refused
  (`> 11 café` with `é` = e+U+0301 gave `Left×6/Backspace×2` → `191 café`
  on readline; this also covers a match cutting through such a cell);
  review r1 B: a cursor in the last column over a non-blank cell is refused
  — after a write into the last column the terminal parks the cursor there
  with a pending wrap, one cell left of the application's position, and
  `Cursor` does not expose the flag; the refusal is conservative: a cursor
  genuinely placed before the final character of a row-filling draft (no
  pending wrap) is refused too, and a `sequence` still edits it; review r1
  D: a `from` whose end lies
  past both the cursor and the last non-blank cell (it reaches into the
  trailing blank run) is refused, since the moves would run past the
  buffer's end;
* moves = `end_index(from) − cursor_index` (negative → `Left`, positive →
  `Right`, zero → none), bounded by the row width and `ACTION_BYTES_MAX`, not
  by the public `repeat ≤ 32`; the bytes are `Left×n` or `Right×n`, then
  `Backspace×chars(from)`, then `to`, with the `sequence` key encoding, in one
  ordered write (one barrier, one ack, one receipt).

Refusals (cursor row outside the viewport, cursor inside a wide cell, off
the row or parked in the last column, absent, N occurrences, another row,
past the draft, a multi-scalar cell in the span, control characters, size)
follow the invalid-action convention: an RPC error through
`failure` (−32403), nothing written, nothing cached; after a granted
`claim:true` the error carries the `note_claim` disclosure. The Planner then
falls back to a `sequence`. The plan is stamped on all three `WriteReceipts`
(unknown/written/refused) before the unknown receipt is cached, as `replace:
{row, cursor_index, cursor_visible, moves: {key, repeat} | null, erased,
inserted}`; a replay returns the cached plan and never recomputes it. `allow_output_below_cursor`
admits `replace` (an editing action: it joins the edits-only allowlist and
its reason string, not `SEQUENCE_KEYS`); the fingerprint covers the action
as given. `application_result` stays `unverified`; the recommended shape is
`replace` + `observe: true, wait_for: "change"` (the readback IS the
preview), then `submit`. What the tool guarantees: the bytes correspond to
that plan against the row as captured. What it does not: that the
application moves one character per arrow key and erases one per Backspace
(true for Claude Code — measured with CJK in rounds 13/14 — readline and most
line editors), that the text belongs to an unsubmitted draft (the screen
cannot prove it), or wrapped drafts (a `from` on another row is refused).
The focused suite drives Python's `input()` with GNU readline under
`LANG=C.UTF-8`: `11 松果` → `19 松果` moves Left 3 (characters, not the 5
columns), Home then `松果` → `苹果` moves Right 5, a separate Enter prints
the edited line, and the same edit under `printf '\033[?25l'` (cursor hidden,
as in Claude Code) is written with `cursor_visible: false`. The same fact
reaches `allow_output_below_cursor`: `ScreenDiff::only_below_cursor` compares
the cursor's position (a cursor hidden in both captures is admitted; the
suite's hint box under `\033[?25l` reports `screen_diff.cursor {moved: false,
visible: false}` and writes with `tolerance: below_cursor`), while a
visibility change between the observation and the live frame stays refused
by the surface fence through the mode bit, as before. Known gap (review r1
L): with visibility no longer required, an application that hides its cursor
and parks it on row 0 leaves only row 0 in the at-or-above set, so
`allow_output_below_cursor` would admit an edit onto a largely repainted
screen; the edits-only allowlist still excludes Enter, submit and clicks, and
the Planner opts in only for a draft box whose cursor sits on the draft.

### `summary` on receipts

Every `calm.terminal.input` receipt and every `calm.terminal.control`
claim/release receipt (not detach: it has no readback) gains `summary`, a
flat object derived in the MCP layer's `receipt_result`
(`terminal_interaction/receipt_summary.rs`) once the readback and release
facts are final — no new facts, no fence reads it: `{"action":
written|refused|unknown|stale_observation|control_unavailable|claim|release,
"readback": available|unavailable|none, "screen": changed|unchanged|null
(`changed_since_previous_observation` in words; null on a connection's first
observation, where `previous_observation_revision` is null and nothing was
compared — #1692), "wait": wait.outcome,
"settled": wait.settled, "signal": wait.signal.event, "repaint": wait.repaint.outcome,
"matched": wait.text.pattern, "role": state.role, "control_id":
state.control_id, "exited": state.exited, "claim": claim.status, "release":
release.status}`. Every field is nullable; mode-dependent wait fields are null
outside their mode; `control_id` is the readback's (explicit null included),
never the receipt's own lease — after `release:true` the receipt still names
the granted lease while the summary says `null`; `release` fills for a
control release too since #1697. `application_result:
"unverified"` stays where it is: the summary is a digest of evidence, not a
verdict and not proof of a current screen change. The text block
(`content[0].text`) says the same in words — `terminal <id> input written;
screen changed settled; wait signal; signal stop, repaint settled; role
observer; details in structuredContent` — so a client that shows only text
gets the digest too.

#1692 (round 19): `summary.screen` used to be `wait.outcome`, so a signal
readback whose budget ran out on a screen that had moved (revision 509 →
645) said `screen: "unchanged"` — the outcome name `WaitOutcome::Unchanged`
was reused for "no signal within the budget", a fact about the ring, and the
Planner read it as "Claude did not move". Now the screen fact and the wait
outcome are two fields (`screen` from `changed_since_previous_observation`,
`wait` the outcome, `no_signal` for that case), and the budget-end capture
lands on a frame boundary whenever one occurs within the grace (a mitigation,
not an atomic frame guarantee): `wait_for=signal` without a signal returns
once the projection has been quiet for `min(settle_ms, 30 ms)` (one Ink frame
is a burst of PTY chunks a few ms apart; 30 ms separates frames, also under a
~100 ms spinner) or at the latest `settle_ms` past the budget, `settled`
only when quiet for the full `settle_ms` — an idle screen returns at the
deadline settled, a spinner within a few tens of ms unsettled, a streaming
log at the grace end unsettled. Constants: `wait::FRAME_GAP` 30 ms; the
grace is `settle_ms` (default 150).
The summary adds no input-schema bytes; the open wait properties and the
`replace` arm do (after r16's `wait_text_absent`: open 942 bytes, input
2126, observe 847, control 833 per `schema.to_string().len()` on the golden
registration, against the strict `< 4000` per-tool schema test).

### Text conditions on waits (#1677 r16)

Rounds 15 and 16 on the real Claude Code repeated one friction on the
rewind scenario: `stop` arrived while the screen still showed the busy
spinner (`· Noodling…` / `⏸ manual mode on · esc to interrupt`), the repaint
phase reported `settled` 7 ms after the signal (the last spinner frame had
been quiet ~143 ms) and the answer painted later, costing one observe. The
Planner's ask — "完成等待同时核验忙碌提示消失" — is a text condition on the
wait.

`wait_text_absent` (array, 1..8 literal patterns, the same validation as
`wait_text`) on observe, input, control and open; `wait_text` is now also
accepted in signal mode. Both are refused in change and elapsed mode
(`terminal_interaction/wait_plan.rs`, mode coupling in both directions).
The conditions (`terminal_interaction/text_conditions.rs`) are: PRESENT —
some `wait_text` pattern is a substring of some live viewport row — and
ABSENT — no `wait_text_absent` pattern is on any row; a list without
patterns is vacuously true and reported as `null`.

* Text mode: at least one of the two lists is required; the same loop
  (`text_wait.rs`) with the predicate generalised: the wait ends when the
  conditions hold and the screen has then been quiet for `settle_ms`; a
  screen on which they stop holding returns the wait to "not held".
  `wait.text` still names the present match (first pattern in argument
  order, first row) or is null when only an absence condition was asked;
  `wait.outcome` is `matched` iff the conditions held on the screen the
  wait ended on; the new `wait.conditions {present, absent}` says which side
  held (`true|false|null`).
* Signal mode: the repaint phase (`terminal_interaction/repaint.rs`, moved
  out of `wait.rs`) ends with `already` / `settled` only when the screen is
  quiet for `settle_ms` AND the conditions hold on the current screen,
  re-tested on every revision wake with one capture per revision (never on
  a timer or protocol wake); while they do not hold the phase keeps waiting
  for further revisions until the budget → `unsettled`. `none` stays: no
  revision at all within `repaint_ms`. Without conditions the phase is
  exactly #1628's (the paused-clock tests pin it, and no capture is taken).
  `wait.conditions` is reported in signal mode too (both null without
  conditions, or when no signal arrived).
* A wait that tests text needs the live viewport: `scroll_offset` must be 0
  for `wait_for=text` and for a signal wait with conditions (MCP layer and
  service).

Recommended Claude prompt readback: `submit` + `observe: true, wait_for:
"signal", wait_text_absent: ["esc to interrupt"]` — the readback returns once
the busy hint is gone and the screen is quiet; the Planner reads the answer
from the state (the tool does not know it is an answer).
`wait.conditions.absent` reports whether the absent patterns were gone on the
LAST tested screen and `repaint.outcome` reports the quiet window separately:
`unsettled` means the budget ended before quiet-with-conditions (the hint may
have vanished just before the budget), so observe again. `wait.conditions` is
`{present: null, absent: null}` when no signal arrived within the budget (the
phase never ran), and `repaint_ms: 0` together with text conditions is
invalid params (`WaitPlan::new`: the phase that tests them would be skipped).
The focused suite drives a fake Claude that posts `Stop` while a busy row is
painted and replaces it with the answer 800 ms later: without conditions the
readback returns at the spinner (#1628), with `wait_text_absent` it returns
with the answer. The phase tests the rows the capture renders, which can be
a NEWER revision than the channel value it woke on (review r1 C: the hint
cleared between the channel read and the rows read); that captured revision
is recorded as a change at the capture time, so no quiet window is credited
to a screen the conditions were not read from — `Repaint::observe` treats
revisions monotonically and `Tested` never re-captures a revision it has
already rendered.

The second repeated friction is guidance only (input.md, planner.md, no
fence change): the Planner pressed Enter right after an edit readback and
Claude Code's status line below the input refreshed in between (`● high ·
/effort` appended) → `stale_observation` → resend. `allow_output_below_cursor`
stays edits-only by design (the slash-menu hazard), so the guidance is: when
submitting a draft that the previous action's readback already showed
(text/replace/sequence), pass `allow_output_since_observation=true` on that
Enter/submit; keep the plain fence when a command menu may be open below
the draft (a draft starting with `/`) or after a long pause; `screen_diff` on
a stale result still says which flag applies.

### Collector (#1677)

`open_with_wait` (open calls with any wait argument), `open_wait_outcomes`
(tally of those opens' returned `wait.outcome`), `replace_actions`
(requested, failed ones included), `replace_written` (non-failed replace
receipts with outcome `written`), `summary_present` (non-failed input/control
receipts carrying `summary`); the wait accounting no longer excludes open
results, so an open with `wait_for=change` or `text` counts as a change or
text wait request with its outcome; the edit scenario's `corrects` rule
accepts a `replace`. r16: `text_condition_requests` (observation-requesting
calls in signal mode whose arguments carry `wait_text` or
`wait_text_absent`) and `signal_condition_outcomes` (tally of those calls'
returned `wait.repaint.outcome` joined with whether every asked condition
held, e.g. `settled/held`, `unsettled/not_held`).
