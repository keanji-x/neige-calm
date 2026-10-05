-- #2139 R1 — the commit message a worker supplied with neige_task_done; NULL = the kernel's own
-- one-line text (every non-completed attempt, a done report without one, every row before this
-- migration). Written once by the release that ends the attempt. `IS`, not `=`: a NULL outcome
-- must refuse a message, and `NULL = 'completed'` would make the CHECK pass.
ALTER TABLE task_git_deliveries ADD COLUMN commit_message TEXT NULL
  CHECK (commit_message IS NULL OR (outcome IS 'completed'
    AND length(CAST(commit_message AS BLOB)) BETWEEN 1 AND 16384));

-- The 0113 immutability trigger names its columns, so it does not cover this one.
CREATE TRIGGER task_git_delivery_commit_message_immutable
BEFORE UPDATE OF commit_message ON task_git_deliveries
BEGIN SELECT RAISE(ABORT, 'git delivery commit message is immutable'); END;
