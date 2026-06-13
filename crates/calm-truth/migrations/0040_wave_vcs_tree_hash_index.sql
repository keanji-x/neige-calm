-- Wave VCS sweeper: live-tree lookups should not scan commits once per object.

CREATE INDEX idx_wave_vcs_commits_tree_hash
    ON wave_vcs_commits(tree_hash);
