-- GC proof epochs permit tree-coverage-preserving history extensions.
-- The epoch certifies a conservative superset of current reachable trees and valid objects.
-- Positive same-tree head prefixes preserve this proof; all other mutations invalidate it.
CREATE TABLE track_vcs_gc_state (
    singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
    revision INTEGER NOT NULL CHECK (typeof(revision) = 'integer' AND revision >= 0)
);
INSERT INTO track_vcs_gc_state(singleton, revision) VALUES (1, 0);

CREATE TRIGGER track_vcs_gc_state_no_delete
BEFORE DELETE ON track_vcs_gc_state
BEGIN
    SELECT RAISE(ABORT, 'track-vcs: required GC revision cannot be deleted');
END;

-- BEFORE INSERT also runs for INSERT OR REPLACE before its implicit conflict deletion.
CREATE TRIGGER track_vcs_gc_state_no_replace
BEFORE INSERT ON track_vcs_gc_state
WHEN EXISTS (SELECT 1 FROM track_vcs_gc_state)
BEGIN
    SELECT RAISE(ABORT, 'track-vcs: required GC revision cannot be replaced');
END;

CREATE TRIGGER track_vcs_gc_state_monotonic
BEFORE UPDATE ON track_vcs_gc_state
WHEN NEW.singleton IS NOT OLD.singleton
  OR typeof(NEW.revision) IS NOT 'integer'
  OR OLD.revision = 9223372036854775807
  OR NEW.revision IS NOT OLD.revision + 1
BEGIN
    SELECT RAISE(ABORT, 'track-vcs: GC revision must advance exactly once');
END;

-- A positive local witness. Both old HEAD and parent are the same existing commit.
-- The self-alias guard rejects replacing that HEAD with a row that proves itself.
CREATE VIEW track_vcs_gc_inert_head_extension AS
SELECT c.hash AS child_hash, c.track_id AS track_id
FROM track_vcs_commits AS c
JOIN track_vcs_refs AS r
  ON r.track_id = c.track_id AND r.head_hash = c.parent_hash
JOIN track_vcs_commits AS p
  ON p.hash = r.head_hash AND p.track_id = c.track_id
WHERE c.hash <> p.hash AND c.tree_hash = p.tree_hash;

CREATE TRIGGER track_vcs_gc_revision_refs_insert
AFTER INSERT ON track_vcs_refs
BEGIN
    UPDATE track_vcs_gc_state SET revision = revision + 1 WHERE singleton = 1;
    SELECT RAISE(ABORT, 'track-vcs: required GC revision is missing')
    WHERE changes() <> 1;
END;

-- BEFORE is required: the shared witness must see the current OLD head.
CREATE TRIGGER track_vcs_gc_revision_refs_update
BEFORE UPDATE ON track_vcs_refs
BEGIN
    SELECT RAISE(ABORT, 'track-vcs: required GC revision is missing')
    WHERE NOT EXISTS (SELECT 1 FROM track_vcs_gc_state WHERE singleton = 1);
    UPDATE track_vcs_gc_state SET revision = revision + 1
    WHERE singleton = 1 AND NOT (
        NEW.track_id IS OLD.track_id
        AND (
            NEW.head_hash IS OLD.head_hash
            OR EXISTS (
                SELECT 1 FROM track_vcs_gc_inert_head_extension AS extension
                WHERE extension.child_hash = NEW.head_hash
                  AND extension.track_id = OLD.track_id
            )
        )
    );
END;

CREATE TRIGGER track_vcs_gc_revision_refs_delete
AFTER DELETE ON track_vcs_refs
BEGIN
    UPDATE track_vcs_gc_state SET revision = revision + 1 WHERE singleton = 1;
    SELECT RAISE(ABORT, 'track-vcs: required GC revision is missing')
    WHERE changes() <> 1;
END;

CREATE TRIGGER track_vcs_gc_revision_commits_insert
AFTER INSERT ON track_vcs_commits
BEGIN
    SELECT RAISE(ABORT, 'track-vcs: required GC revision is missing')
    WHERE NOT EXISTS (SELECT 1 FROM track_vcs_gc_state WHERE singleton = 1);
    UPDATE track_vcs_gc_state SET revision = revision + 1
    WHERE singleton = 1 AND NOT EXISTS (
        SELECT 1 FROM track_vcs_gc_inert_head_extension AS extension
        WHERE extension.child_hash = NEW.hash AND extension.track_id = NEW.track_id
    );
END;

CREATE TRIGGER track_vcs_gc_revision_commits_update
AFTER UPDATE ON track_vcs_commits
BEGIN
    UPDATE track_vcs_gc_state SET revision = revision + 1 WHERE singleton = 1;
    SELECT RAISE(ABORT, 'track-vcs: required GC revision is missing')
    WHERE changes() <> 1;
END;

CREATE TRIGGER track_vcs_gc_revision_commits_delete
AFTER DELETE ON track_vcs_commits
BEGIN
    UPDATE track_vcs_gc_state SET revision = revision + 1 WHERE singleton = 1;
    SELECT RAISE(ABORT, 'track-vcs: required GC revision is missing')
    WHERE changes() <> 1;
END;

CREATE TRIGGER track_vcs_gc_revision_objects_insert
AFTER INSERT ON track_vcs_objects
BEGIN
    UPDATE track_vcs_gc_state SET revision = revision + 1 WHERE singleton = 1;
    SELECT RAISE(ABORT, 'track-vcs: required GC revision is missing')
    WHERE changes() <> 1;
END;

CREATE TRIGGER track_vcs_gc_revision_objects_update
AFTER UPDATE ON track_vcs_objects
BEGIN
    UPDATE track_vcs_gc_state SET revision = revision + 1 WHERE singleton = 1;
    SELECT RAISE(ABORT, 'track-vcs: required GC revision is missing')
    WHERE changes() <> 1;
END;

CREATE TRIGGER track_vcs_gc_revision_objects_delete
AFTER DELETE ON track_vcs_objects
BEGIN
    UPDATE track_vcs_gc_state SET revision = revision + 1 WHERE singleton = 1;
    SELECT RAISE(ABORT, 'track-vcs: required GC revision is missing')
    WHERE changes() <> 1;
END;
