<!-- archived review text, round 6, doc @ a807c1b2f (verbatim; no #1316 ratchet substitutions needed) -->

# Round 6 / channel B — verdict: APPROVE

## Wrong facts or false §11 dispositions (file:line → correct)
None found in the reviewed kernel scope. The round-5 kernel dispositions are incorporated into the operative rules, regression cases, and runbook; their cited code supports them.

## BLOCKER-n / MAJOR-n / MINOR-n: None
Zero BLOCKER, zero MAJOR, zero MINOR.

- **G19:** Cleared under the stated timing assumption. The original completed execution’s failed session is suppressed; a replacement minted after completion remains red. Restart repoints the card, and compensation fails the replacement session. Live permission/error evidence remains outside suppression.
- **W:** Cleared. Dispatched work counts without a session signal. Child delegation in `dispatched/running` is excluded, but retained `child_track_id` does not hide the parent’s `verifying` gate.
- **S0/live gate:** Current-attempt eligibility removes obsolete failures; terminal sessions cannot preserve FSM/thread-status attention. Declared historical-overlay and generation gaps remain explicit.
- **Feeder/reaper:** Completion becomes `idle/systemError`; the completion timestamp is monotone. After the 900-second recency gate, reaping still requires confirmed `Dead`, not merely `idle`.
- **FSM:** Non-projecting hooks preserve registration; Stop and the Notification whitelist address the documented resurrection sequence. The stale-session fence preserves fallback actors needed after `/clear`.
- **Migration/compatibility:** Proposed DDL applied after all 109 migrations in memory; singleton checks passed. WEB 29/API 9 with SYNC 20 matches the compatibility contracts. Both current lockstep scripts passed.
- **Queries/transactions:** E1/E2 use `idx_transcript_card_method_created_at (card_id=? AND method=?)`; removing it invalidated the plan assertion. Full non-archived boot/tick sweeps and autocommit reads respect the documented coverage and deferred-transaction constraints.

These were source and in-memory design checks, not Rust implementation or real-stack tests. S1’s notification-order acceptance remains required. Worktree unchanged at `a807c1b2f`.

## Open-question answers (§8, one line each)
Q1: Closed: one baseline per device/database identity, including existing devices after migration.
Q2: Closed: option (c), projection gating without rewriting historical FSM rows; retain the differentiated exit runbook.
Q3: Closed: retain the explicit Notification whitelist; other or missing subtypes do not project.
Q4: Closed: harness `starting` is not working; dispatched tasks independently count through W.
Q5: Closed: replace the primitive now; confirm dimensions during real-device visual signoff.