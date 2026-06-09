UPDATE events
SET kind = 'codex.worker_requested'
WHERE kind = 'codex.job_requested';

UPDATE events
SET kind = 'terminal.worker_requested'
WHERE kind = 'terminal.job_requested';
