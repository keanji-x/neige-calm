-- #1893 S4 — the isolated-codex-v1 path and `calm.task.dispatch` are deleted with their code. The
-- dispatch receipt table goes; nothing references it.
DROP TABLE planner_dispatch_receipts;
