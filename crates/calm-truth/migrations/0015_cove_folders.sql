-- Issue #250 PR 1 — cove ↔ folder mapping.
--
-- A `cove_folder` claims an absolute filesystem path for a cove. The
-- folder automatically covers every descendant directory: given a cwd
-- the kernel does longest-prefix matching across all rows to decide
-- which cove a path belongs to. A path can be claimed by **at most
-- one** cove (UNIQUE on `path`); ancestor/descendant overlap is also
-- forbidden but enforced application-side because SQLite has no
-- straightforward way to assert non-prefix invariants in a constraint.
--
-- Paths are stored already-normalized (no trailing slash, except for
-- root `/`) so that equality comparison and `LIKE` prefix matching
-- both work without re-normalizing in every query.
--
-- ON DELETE CASCADE on `cove_id`: when a cove is removed every folder
-- it claimed disappears with it. The reverse direction — deleting a
-- folder while it still backs a live wave — gets a guard in PR 2,
-- once `Wave.cwd` lands.

CREATE TABLE cove_folders (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    cove_id     TEXT    NOT NULL REFERENCES coves(id) ON DELETE CASCADE,
    path        TEXT    NOT NULL UNIQUE,
    created_at  INTEGER NOT NULL
);

CREATE INDEX idx_cove_folders_cove ON cove_folders(cove_id);
