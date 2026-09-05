-- Issue #1456 — terminal task completion must carry enough durable evidence
-- to validate command output after the live renderer and supervisor entry are
-- gone. Terminal workers run under one PTY, so stdout/stderr are irreversibly
-- multiplexed before the kernel observes bytes; name the stored stream
-- honestly instead of manufacturing separate stdout/stderr values.
ALTER TABLE terminals ADD COLUMN pty_output TEXT NOT NULL DEFAULT '';
ALTER TABLE terminals ADD COLUMN pty_output_truncated INTEGER NOT NULL DEFAULT 0
  CHECK (pty_output_truncated IN (0, 1));

-- An already-exited row predates output capture. Empty text means "not
-- available", not "the child produced no output".
UPDATE terminals
   SET pty_output_truncated = 1
 WHERE exit_code IS NOT NULL OR signal_killed = 1;
