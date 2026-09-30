-- #1893 S6 — `calm.task.delivery` is deleted: a failed Git delivery fails a gated task in its
-- settlement, and nothing abandons a delivery any more. The abandonment table goes with its code;
-- its immutability trigger goes with it. Nothing references it (it is a leaf child of
-- `task_git_deliveries` and `tracks`).
DROP TABLE task_git_delivery_abandonments;
