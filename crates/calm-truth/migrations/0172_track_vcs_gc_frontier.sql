-- A second, stricter epoch permits bounded prefix proofs without weakening released171.
-- Fresh witnesses exist only between a distinct commit INSERT and its ref publication.
-- All HEAD changes require freshness, including same-tree changes (otherwise HEAD ABA
-- could hide restoration of an old missing ancestor). Pending is never itself a GC root.
-- Source NOT NULL admission mirrors0039/0081/0123; review it with future constraints.
-- Existing-hash attempts invalidate even when IGNORE skips the source row. REPLACE
-- safety does not depend on recursive_triggers or implicit deletion trigger execution.

CREATE TABLE track_vcs_gc_frontier_state(
 singleton INTEGER PRIMARY KEY CHECK(singleton=1),
 revision INTEGER NOT NULL CHECK(typeof(revision)='integer' AND revision>=0));
INSERT INTO track_vcs_gc_frontier_state VALUES(1,0);
CREATE TRIGGER frontier_state_no_delete BEFORE DELETE ON track_vcs_gc_frontier_state
BEGIN SELECT RAISE(ABORT,'required unknown epoch cannot be deleted');

END;
CREATE TRIGGER frontier_state_no_replace BEFORE INSERT ON track_vcs_gc_frontier_state
WHEN EXISTS(SELECT 1 FROM track_vcs_gc_frontier_state)
BEGIN SELECT RAISE(ABORT,'required unknown epoch cannot be replaced');

END;
CREATE TRIGGER frontier_state_monotonic BEFORE UPDATE ON track_vcs_gc_frontier_state
WHEN NEW.singleton IS NOT OLD.singleton OR typeof(NEW.revision)<>'integer'
 OR OLD.revision=9223372036854775807 OR NEW.revision IS NOT OLD.revision+1
BEGIN SELECT RAISE(ABORT,'unknown epoch must advance exactly once');

END;
CREATE TABLE track_vcs_gc_pending(
 singleton INTEGER PRIMARY KEY CHECK(singleton=1),
 hash TEXT NOT NULL REFERENCES track_vcs_commits(hash) DEFERRABLE INITIALLY DEFERRED,
 track_id TEXT NOT NULL);

CREATE TRIGGER pending_insert BEFORE INSERT ON track_vcs_gc_pending BEGIN
 SELECT RAISE(ABORT,'required unknown epoch invalid') WHERE NOT EXISTS (SELECT 1 FROM track_vcs_gc_frontier_state WHERE singleton=1 AND typeof(revision)='integer' AND revision>=0);
 SELECT RAISE(ABORT,'pending slot replacement forbidden') WHERE EXISTS(SELECT 1 FROM track_vcs_gc_pending);
 SELECT RAISE(ABORT,'pending target must be absent') WHERE EXISTS(SELECT 1 FROM track_vcs_commits WHERE hash=NEW.hash);
END;
CREATE TRIGGER pending_update BEFORE UPDATE ON track_vcs_gc_pending BEGIN
 SELECT RAISE(ABORT,'pending update forbidden');

END;
CREATE TRIGGER pending_delete BEFORE DELETE ON track_vcs_gc_pending BEGIN
 SELECT RAISE(ABORT,'required unknown epoch invalid') WHERE NOT EXISTS (SELECT 1 FROM track_vcs_gc_frontier_state WHERE singleton=1 AND typeof(revision)='integer' AND revision>=0);
 UPDATE track_vcs_gc_frontier_state SET revision=revision+1 WHERE singleton=1
 AND NOT EXISTS(SELECT 1 FROM track_vcs_refs r JOIN track_vcs_commits c ON c.hash=r.head_hash
 WHERE r.track_id=OLD.track_id AND r.head_hash=OLD.hash AND c.track_id=OLD.track_id);
END;
CREATE TRIGGER frontier_commits_insert BEFORE INSERT ON track_vcs_commits BEGIN
 SELECT RAISE(ABORT,'required unknown epoch invalid') WHERE NOT EXISTS (SELECT 1 FROM track_vcs_gc_frontier_state WHERE singleton=1 AND typeof(revision)='integer' AND revision>=0);
 UPDATE track_vcs_gc_frontier_state SET revision=revision+1 WHERE singleton=1 AND NOT (NOT EXISTS(SELECT 1 FROM track_vcs_commits WHERE hash=NEW.hash)
AND NEW.hash IS NOT NULL AND NEW.track_id IS NOT NULL AND NEW.tree_hash IS NOT NULL
AND NEW.manifest_schema_version IS NOT NULL AND NEW.created_at IS NOT NULL
AND EXISTS(SELECT 1 FROM track_vcs_refs r JOIN track_vcs_commits p
 ON p.hash=r.head_hash AND p.track_id=r.track_id
 WHERE r.track_id=NEW.track_id AND r.head_hash=NEW.parent_hash AND p.hash<>NEW.hash));
 DELETE FROM track_vcs_gc_pending WHERE hash=NEW.hash OR (NOT EXISTS(SELECT 1 FROM track_vcs_commits WHERE hash=NEW.hash)
AND NEW.hash IS NOT NULL AND NEW.track_id IS NOT NULL AND NEW.tree_hash IS NOT NULL
AND NEW.manifest_schema_version IS NOT NULL AND NEW.created_at IS NOT NULL
AND EXISTS(SELECT 1 FROM track_vcs_refs r JOIN track_vcs_commits p
 ON p.hash=r.head_hash AND p.track_id=r.track_id
 WHERE r.track_id=NEW.track_id AND r.head_hash=NEW.parent_hash AND p.hash<>NEW.hash));
 INSERT INTO track_vcs_gc_pending(singleton,hash,track_id)
 SELECT 1,NEW.hash,NEW.track_id WHERE NOT EXISTS(SELECT 1 FROM track_vcs_commits WHERE hash=NEW.hash)
AND NEW.hash IS NOT NULL AND NEW.track_id IS NOT NULL AND NEW.tree_hash IS NOT NULL
AND NEW.manifest_schema_version IS NOT NULL AND NEW.created_at IS NOT NULL
AND EXISTS(SELECT 1 FROM track_vcs_refs r JOIN track_vcs_commits p
 ON p.hash=r.head_hash AND p.track_id=r.track_id
 WHERE r.track_id=NEW.track_id AND r.head_hash=NEW.parent_hash AND p.hash<>NEW.hash);
END;
CREATE TRIGGER frontier_commits_update BEFORE UPDATE ON track_vcs_commits
BEGIN
    SELECT RAISE(ABORT,'required unknown epoch invalid') WHERE NOT EXISTS (SELECT 1 FROM track_vcs_gc_frontier_state WHERE singleton=1 AND typeof(revision)='integer' AND revision>=0);
    UPDATE track_vcs_gc_frontier_state SET revision=revision+1 WHERE singleton=1;
    DELETE FROM track_vcs_gc_pending WHERE hash=OLD.hash;

END;
CREATE TRIGGER frontier_commits_delete BEFORE DELETE ON track_vcs_commits
BEGIN
    SELECT RAISE(ABORT,'required unknown epoch invalid') WHERE NOT EXISTS (SELECT 1 FROM track_vcs_gc_frontier_state WHERE singleton=1 AND typeof(revision)='integer' AND revision>=0);
    UPDATE track_vcs_gc_frontier_state SET revision=revision+1 WHERE singleton=1;
    DELETE FROM track_vcs_gc_pending WHERE hash=OLD.hash;

END;

CREATE TRIGGER frontier_refs_update BEFORE UPDATE ON track_vcs_refs BEGIN
 SELECT RAISE(ABORT,'required unknown epoch invalid') WHERE NOT EXISTS (SELECT 1 FROM track_vcs_gc_frontier_state WHERE singleton=1 AND typeof(revision)='integer' AND revision>=0);
 UPDATE track_vcs_gc_frontier_state SET revision=revision+1 WHERE singleton=1 AND NOT (NEW.track_id IS OLD.track_id AND (NEW.head_hash IS OLD.head_hash OR EXISTS(
 SELECT 1 FROM track_vcs_gc_pending q JOIN track_vcs_commits c ON c.hash=q.hash
 JOIN track_vcs_commits p ON p.hash=OLD.head_hash AND p.track_id=OLD.track_id
 WHERE q.hash=NEW.head_hash AND q.track_id=OLD.track_id AND c.track_id=OLD.track_id
 AND c.parent_hash=OLD.head_hash AND c.hash<>p.hash)));
END;
CREATE TRIGGER frontier_refs_cleanup AFTER UPDATE ON track_vcs_refs BEGIN
 DELETE FROM track_vcs_gc_pending WHERE hash=NEW.head_hash AND track_id=NEW.track_id;
END;
CREATE TRIGGER frontier_refs_insert AFTER INSERT ON track_vcs_refs
BEGIN
    SELECT RAISE(ABORT,'required unknown epoch invalid') WHERE NOT EXISTS (SELECT 1 FROM track_vcs_gc_frontier_state WHERE singleton=1 AND typeof(revision)='integer' AND revision>=0);
    UPDATE track_vcs_gc_frontier_state SET revision=revision+1 WHERE singleton=1;

END;
CREATE TRIGGER frontier_refs_delete BEFORE DELETE ON track_vcs_refs
BEGIN
    SELECT RAISE(ABORT,'required unknown epoch invalid') WHERE NOT EXISTS (SELECT 1 FROM track_vcs_gc_frontier_state WHERE singleton=1 AND typeof(revision)='integer' AND revision>=0);
    UPDATE track_vcs_gc_frontier_state SET revision=revision+1 WHERE singleton=1;

END;

CREATE TRIGGER frontier_objects_insert BEFORE INSERT ON track_vcs_objects BEGIN
 SELECT RAISE(ABORT,'required unknown epoch invalid') WHERE NOT EXISTS (SELECT 1 FROM track_vcs_gc_frontier_state WHERE singleton=1 AND typeof(revision)='integer' AND revision>=0);
 UPDATE track_vcs_gc_frontier_state SET revision=revision+1 WHERE singleton=1
 AND EXISTS(SELECT 1 FROM track_vcs_objects o WHERE o.hash=NEW.hash AND
 (o.kind IS NOT NEW.kind OR o.bytes IS NOT NEW.bytes));
END;
CREATE TRIGGER frontier_objects_update BEFORE UPDATE ON track_vcs_objects
BEGIN
    SELECT RAISE(ABORT,'required unknown epoch invalid') WHERE NOT EXISTS (SELECT 1 FROM track_vcs_gc_frontier_state WHERE singleton=1 AND typeof(revision)='integer' AND revision>=0);
    UPDATE track_vcs_gc_frontier_state SET revision=revision+1 WHERE singleton=1;

END;
CREATE TRIGGER frontier_objects_delete BEFORE DELETE ON track_vcs_objects
BEGIN
    SELECT RAISE(ABORT,'required unknown epoch invalid') WHERE NOT EXISTS (SELECT 1 FROM track_vcs_gc_frontier_state WHERE singleton=1 AND typeof(revision)='integer' AND revision>=0);
    UPDATE track_vcs_gc_frontier_state SET revision=revision+1 WHERE singleton=1;

END;
