<!-- archived review text, round 4, doc @ cf1a6b798 (verbatim; no #1316 ratchet substitutions needed) -->

# Round 4 / channel B — verdict: APPROVE

## Wrong facts or false §11 dispositions (file:line → correct)

- [Design:74](/mnt/data2/kenji/neige-calm/.claude/worktrees/1722-activity-design/docs/architecture/1722-track-activity-indicators.md:74), repeated at :182 and :438: restarting Claude does **not** necessarily rewrite the old session to `Exited`. `claude_restart_adapter.rs:157-168` completes only an **active** session; `session_projection.rs:198` excludes `failed`. After D-signal, the old row remains `failed`; repointing `cards.session_id` removes it from S0.
- No false kernel §11 disposition requiring BLOCKER/MAJOR treatment found. Approval covers **v4 as written**, including its registered gaps; it does **not** clear the proposed G19 suppression rule.
- Verification: in-memory SQLite confirmed E1/E2 use `idx_transcript_card_method_created_at (card_id=? AND method=?)`, repeated singleton insertion preserves the first identity, and `singleton=2` fails CHECK. Current WEB 28 and SYNC 20 lockstep gates passed. No files changed.

## MINOR-1: Restart cleanup explanation names the wrong mechanism; where; construction; required change

Where: §2 F2.26, §4.2 live-session gate, §7 D-signal′.

Construction: signal-kill session s1, then restart the card as s2. The active-session lookup excludes s1, so `session_complete_tx(Exited)` never touches it. `session_start_mirror_tx` instead links the card to s2 (`session_mirror.rs:282-289`). The intended projection still clears s1 correctly.

Required change: attribute clearing to the current-session pointer; qualify the claim that restart rewrites the predecessor to `Exited`.

## Open-question answers (§8, one line each)

Q1: Closed; persistent database identity plus a once-per-device/database baseline implements the settled decision.

Q2: Closed; S0 and the live-session gate suppress exited-session overlays; eligible `failed` sessions remain red as documented, with the restart explanation corrected above.

Q3: Closed; `Option<State>`, Stop→Idle, and the Notification whitelist implement the dispositions; the documented timer and native-UUID gaps remain.

Q4: Closed; harness `starting` stays quiet, while a dispatched task independently makes its track working through W.

Q5: Closed; replacing the primitive leaves sizing to the declared browser/device acceptance.

G19: **Do not clear the broad rule.** Construction: task T finishes on Claude card C; its process exits; the user restarts C and submits new work. Restart accepts the card without changing T (`claude_restart_adapter.rs:133-175,223-240`), and browser terminal input uses `InteractiveUser` (`ws/terminal.rs:228`; `input_authority.rs:31`). A new permission prompt or `StopFailure` belongs to that new work, yet T remains `done` and still names C, so the proposed filter hides it. Suppression needs evidence distinguishing cleanup of the completed execution from subsequent user work; terminal task status plus permanent card binding is insufficient.