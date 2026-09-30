-- #1893 S3 — Codex semantic recovery (the `Recover` dynamic tool and its turn bindings) is deleted
-- with its code. Its four tables go, children before parents. Nothing references them.
DROP TABLE planner_recovery_calls;
DROP TABLE planner_recovery_turns;
DROP TABLE planner_recovery_issuances;
DROP TABLE planner_recovery_threads;
