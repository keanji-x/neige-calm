-- Identity outlives tracks/areas. Legacy selection facts are recorded verbatim;
-- this transaction performs no filesystem adoption and invents no baseline.
CREATE TABLE workspace_resources (
    id TEXT PRIMARY KEY CHECK (length(id) = 32 AND id NOT GLOB '*[^0-9a-f]*'),
    descriptor_version INTEGER NOT NULL CHECK (descriptor_version = 1),
    selection_key TEXT NOT NULL UNIQUE,
    root_path TEXT NOT NULL,
    ownership TEXT NOT NULL CHECK (ownership IN ('owned', 'borrowed')),
    selection_version INTEGER NOT NULL CHECK (selection_version > 0),
    generation INTEGER NOT NULL CHECK (generation > 0),
    installation_epoch INTEGER NOT NULL CHECK (installation_epoch > 0),
    materialization_state TEXT NOT NULL CHECK (materialization_state IN ('pending', 'legacy_worktree')),
    adopted_workspace_id TEXT NULL REFERENCES workspace_resources(id),
    canonical_root TEXT NULL,
    directory_device INTEGER NULL,
    directory_inode INTEGER NULL,
    git_common_dir TEXT NULL,
    -- Verified marker/registration only; retained independently of the domain row.
    physical_owner_track_id TEXT NULL,
    created_at_ms INTEGER NOT NULL,
    CHECK (
        (adopted_workspace_id IS NULL AND canonical_root IS NULL AND directory_device IS NULL
         AND directory_inode IS NULL AND git_common_dir IS NULL)
        OR (adopted_workspace_id = id AND canonical_root IS NOT NULL AND directory_device IS NOT NULL
            AND directory_inode IS NOT NULL AND git_common_dir IS NOT NULL)
        OR (adopted_workspace_id <> id AND canonical_root IS NULL AND directory_device IS NULL
            AND directory_inode IS NULL AND git_common_dir IS NULL)
    )
);
CREATE UNIQUE INDEX workspace_resources_directory_identity_idx
    ON workspace_resources(directory_device, directory_inode)
    WHERE canonical_root IS NOT NULL;

CREATE TABLE track_workspace_bindings (
    track_id TEXT PRIMARY KEY REFERENCES tracks(id) ON DELETE CASCADE,
    workspace_id TEXT NOT NULL REFERENCES workspace_resources(id),
    admission_version INTEGER NOT NULL CHECK (admission_version > 0),
    selected_path TEXT NOT NULL,
    ownership TEXT NOT NULL CHECK (ownership IN ('owned', 'borrowed')),
    cwd TEXT NOT NULL,
    frozen_at_ms INTEGER NULL
);
CREATE INDEX track_workspace_bindings_resource_idx
    ON track_workspace_bindings(workspace_id, track_id);

-- A concrete owned checkout is per-track. Equal borrowed legacy selections may
-- share identity; empty historical selections are unresolved, never one directory.
INSERT INTO workspace_resources
    (id, descriptor_version, selection_key, root_path, ownership, selection_version,
     generation, installation_epoch, materialization_state, created_at_ms)
SELECT lower(hex(randomblob(16))), 1, selection_key, root_path, ownership,
       1, 1, 1, 'legacy_worktree', MIN(created_at)
FROM (
    SELECT CASE
               WHEN workspace_worktree_path IS NOT NULL OR workspace_kind = 'managed'
                   THEN json_array('owned', id, coalesce(workspace_worktree_path, workspace_path))
               WHEN workspace_path = '' THEN json_array('unresolved', id)
               ELSE json_array('borrowed', workspace_path)
           END AS selection_key,
           coalesce(workspace_worktree_path, workspace_path) AS root_path,
           CASE WHEN workspace_worktree_path IS NOT NULL OR workspace_kind = 'managed'
                THEN 'owned' ELSE 'borrowed' END AS ownership,
           created_at
    FROM tracks
)
GROUP BY selection_key, root_path, ownership;

INSERT INTO track_workspace_bindings
    (track_id, workspace_id, admission_version, selected_path, ownership, cwd, frozen_at_ms)
SELECT t.id, r.id, 1, coalesce(t.workspace_worktree_path,t.workspace_path), r.ownership, '.', t.workspace_frozen_at
FROM tracks t JOIN workspace_resources r ON r.selection_key = CASE
    WHEN t.workspace_worktree_path IS NOT NULL OR t.workspace_kind = 'managed'
        THEN json_array('owned', t.id, coalesce(t.workspace_worktree_path, t.workspace_path))
    WHEN t.workspace_path = '' THEN json_array('unresolved', t.id)
    ELSE json_array('borrowed', t.workspace_path)
END;
