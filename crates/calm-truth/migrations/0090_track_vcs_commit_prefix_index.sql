-- Resolve abbreviated commit hashes within one track without scanning the
-- track's entire retained history or sorting matches in a temporary B-tree.
CREATE INDEX idx_track_vcs_commits_track_hash
    ON track_vcs_commits(track_id, hash);
