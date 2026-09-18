<!-- archived review text, round 3, doc @ 14fbcc3fe (two substitutions for the #1316 ratchet: the two migration file names quoted in the first wrong-fact row read `0032_<retired-item-term>s_fk.sql` / `0094_<retired-id-term>_to_worker_session_id.sql` instead of the words themselves) -->

# Round 3 / channel B — verdict: REVISE

## Wrong facts or false §11 dispositions (file:line → correct)

`D` = `docs/architecture/1722-track-activity-indicators.md`.

- `D:240` → E1/E2 are **not indexed short-range reads**. The transcript table retains only its `card_id` index (`0032_<retired-item-term>s_fk.sql:28`; `0094_<retired-id-term>_to_worker_session_id.sql:34`). In-memory `EXPLAIN` confirms full-table iteration. §11 B-M4 implements the enumeration fix, but its cost justification is false.
- `D:347,411` → terminating a Claude PTY does not necessarily produce `exited`: a signalled exit produces `failed` (`terminal_renderer/attach_reader.rs:127`). Under `D:179`, that remains red. The runbook correction claimed at `D:482,503` is incomplete.
- `D:371` → removing `CHECK(singleton = 1)` does **not** make the described reboot/concurrent-boot tests fail: every insertion still uses primary key `1`.

## MAJOR-1: Full reconciliation repeatedly scans global transcript history
**Where:** `D:208–213,240`; migration specification `D:272–280`.

**Construction:** With T non-archived tracks and H transcript rows, E1/E2 perform approximately **2 × T × H row visits per sweep**, including history belonging to other or archived tracks. At the document’s 1,000-track example and one million transcript rows, that is roughly two billion visits, not merely 11,000 cheap statements. Boot does the same work; the serial projector delays events and subsequent reconciliation while scanning.

**Required change:** Add suitable transcript indexes to the new migration, or batch the aggregates while retaining full-track coverage. Specify and verify query plans and representative scan latency. Keep the accepted autocommit-read/IMMEDIATE-write transaction shape.

## MINOR-1: The cleanup runbook can replace stranded amber with stranded red
**Where:** `D:179,347,411`.

**Construction:** An eligible interactive Claude session has stale `AwaitingInput`. Operations terminates its PTY with a signal. The exit writer persists `state='failed'`; the next tick suppresses the FSM amber but emits session failure. Opening the track does not clear it.

**Required change:** Specify a graceful, unsignalled exit and verify the resulting session state. Explicitly document that signalled termination remains failed and requires a different resolution. Add exited-versus-failed acceptance cases; do not promise unconditional quiet after “close the terminal.”

## MINOR-2: Singleton constraint mutation has no detecting assertion
**Where:** `D:371`; §11 B-M1 at `D:494`.

**Construction:** Without the CHECK, repeated `INSERT OR IGNORE ... VALUES(1, ...)` still preserves one identity because the primary key remains. Concurrent boots behave identically. An in-memory check confirmed this, while an insertion with `singleton=2` successfully created a second row.

**Required change:** Add an assertion that inserting a non-1 singleton fails. Keep the reboot/concurrency tests, but assign the CHECK-removal mutation to this constraint-specific test.

## Open-question answers (§8, one line each)

Q1: Closed; stable database identity and once-per-device/database baselines are coherent, subject to the corrected constraint test.
Q2: Retain (c); qualify the runbook’s exit semantics as MINOR-1 requires.
Q3: Retain the settled Notification whitelist; idle, missing, and unrecognized subtypes remain non-projecting.
Q4: Keep harness `starting` non-working; W independently makes dispatched tasks working.
Q5: Retain primitive replacement followed by device sizing sign-off; no additional kernel requirement.