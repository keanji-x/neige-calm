-- PR9a-ii: back the worker_sessions terminal-completion lookup (runtime_get_active_for_terminal_tx),
-- mirroring runtimes_terminal_run_idx (0028) which the read-flip would otherwise lose.
CREATE INDEX IF NOT EXISTS ws_terminal_run_idx
    ON worker_sessions(terminal_run_id) WHERE terminal_run_id IS NOT NULL;
