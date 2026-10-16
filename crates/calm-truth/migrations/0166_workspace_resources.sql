-- A resource names one logical selection, never a physical object incarnation.
-- Observations are audit facts, not launch, admission or destruction capabilities.
CREATE TABLE workspace_resources (
    id TEXT PRIMARY KEY CHECK (length(id) = 32 AND id NOT GLOB '*[^0-9a-f]*'),
    source_track_id TEXT NOT NULL,
    root_path TEXT NOT NULL,
    ownership TEXT NOT NULL CHECK (ownership IN ('owned', 'borrowed')),
    observation_version INTEGER NOT NULL CHECK (observation_version >= 0),
    observed_root TEXT NULL,
    observed_cwd TEXT NULL,
    directory_device INTEGER NULL,
    directory_inode INTEGER NULL,
    git_common_dir TEXT NULL,
    physical_owner_track_id TEXT NULL,
    created_at_ms INTEGER NOT NULL,
    CHECK (
        (observation_version = 0 AND observed_root IS NULL AND observed_cwd IS NULL
         AND directory_device IS NULL AND directory_inode IS NULL AND git_common_dir IS NULL
         AND physical_owner_track_id IS NULL)
        OR (observation_version > 0 AND observed_root IS NOT NULL AND observed_cwd IS NOT NULL
            AND directory_device IS NOT NULL AND directory_inode IS NOT NULL AND git_common_dir IS NOT NULL
            AND ((ownership = 'borrowed' AND physical_owner_track_id IS NULL)
                 OR (ownership = 'owned' AND physical_owner_track_id IS NOT NULL
                     AND physical_owner_track_id = source_track_id)))
    )
);

CREATE TABLE track_workspace_bindings (
    track_id TEXT PRIMARY KEY REFERENCES tracks(id) ON DELETE CASCADE,
    workspace_id TEXT NOT NULL UNIQUE REFERENCES workspace_resources(id),
    selection_version INTEGER NOT NULL CHECK (selection_version > 0),
    selected_path TEXT NOT NULL,
    ownership TEXT NOT NULL CHECK (ownership IN ('owned', 'borrowed')),
    frozen_at_ms INTEGER NULL
);

-- Every track starts with its own unobserved selection. No historical path or
-- inode lookup may revive an earlier object's identity, even at the same path.
INSERT INTO workspace_resources
    (id, source_track_id, root_path, ownership, observation_version, created_at_ms)
SELECT lower(hex(randomblob(16))), id, coalesce(workspace_worktree_path, workspace_path),
       CASE WHEN workspace_worktree_path IS NOT NULL OR workspace_kind = 'managed'
            THEN 'owned' ELSE 'borrowed' END,
       0, created_at
FROM tracks;

INSERT INTO track_workspace_bindings
    (track_id, workspace_id, selection_version, selected_path, ownership, frozen_at_ms)
SELECT t.id, r.id, 1, r.root_path, r.ownership, t.workspace_frozen_at
FROM tracks t JOIN workspace_resources r ON r.source_track_id = t.id;
