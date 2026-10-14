-- #2599 — a codex or claude task may declare `result_contract`, the name of a kernel-registered
-- worker result contract (calm-types `task_result`). The kernel refuses a `neige_task_done` whose
-- result does not match it. Frozen with the row like `start`. NULL is an untyped result, which
-- every existing row keeps. No CHECK: the registry, not the schema, names the contracts.
ALTER TABLE tasks ADD COLUMN result_contract TEXT;
