-- Old facts remain untracked: migration cannot prove historical process absence.
ALTER TABLE codex_backend_daemons ADD COLUMN recovery_phase TEXT NOT NULL DEFAULT 'legacy'
    CHECK (recovery_phase IN ('legacy', 'clear', 'pending', 'recorded'));
ALTER TABLE codex_backend_daemons ADD COLUMN recovery_id TEXT
    CHECK (
        (recovery_phase IN ('legacy', 'clear') AND recovery_id IS NULL)
        OR (recovery_phase IN ('pending', 'recorded') AND recovery_id IS NOT NULL)
    )
    CHECK (
        recovery_phase NOT IN ('clear', 'pending')
        OR (pid IS NULL AND pgid IS NULL AND process_start_time IS NULL AND boot_id IS NULL)
    )
    CHECK (
        recovery_phase != 'recorded'
        OR pid IS NOT NULL OR pgid IS NOT NULL OR process_start_time IS NOT NULL OR boot_id IS NOT NULL
    );
