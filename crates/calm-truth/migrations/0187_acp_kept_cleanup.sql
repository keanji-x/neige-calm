-- #2836 C2: no backfill. Absence means the receipt still fences its worker.
ALTER TABLE acp_submissions ADD COLUMN kept_cleanup_json TEXT;
DROP INDEX acp_one_unresolved_submission;
CREATE UNIQUE INDEX acp_one_unresolved_submission ON acp_submissions(worker_session_id)
  WHERE state = 'sending' OR (state = 'unknown' AND kept_cleanup_json IS NULL);

-- Evidence is a later owner observation, never supplied by a receipt insertion.
CREATE TRIGGER acp_cleanup_no_insert BEFORE INSERT ON acp_submissions
WHEN NEW.kept_cleanup_json IS NOT NULL
BEGIN
  SELECT RAISE(ABORT, 'ACP cleanup requires an existing Unknown receipt');
END;

-- REPLACE deletes conflicts without UPDATE (or recursive DELETE) triggers.
-- Guard both receipt keys before conflict handling, including UPSERT/IGNORE.
CREATE TRIGGER acp_cleanup_no_insert_collision BEFORE INSERT ON acp_submissions
WHEN EXISTS(SELECT 1 FROM acp_submissions
  WHERE kept_cleanup_json IS NOT NULL AND (
    (worker_session_id = NEW.worker_session_id AND client_id = NEW.client_id)
    OR turn_id = NEW.turn_id))
BEGIN
  SELECT RAISE(ABORT, 'ACP released Unknown receipt is immutable');
END;

-- An unreleased source row must not evict a released target with UPDATE OR REPLACE.
CREATE TRIGGER acp_cleanup_no_update_collision BEFORE UPDATE ON acp_submissions
WHEN EXISTS(SELECT 1 FROM acp_submissions
  WHERE kept_cleanup_json IS NOT NULL AND rowid != OLD.rowid AND (
    (worker_session_id = NEW.worker_session_id AND client_id = NEW.client_id)
    OR turn_id = NEW.turn_id))
BEGIN
  SELECT RAISE(ABORT, 'ACP released Unknown receipt is immutable');
END;
-- Plain DELETE and worker ON DELETE CASCADE intentionally remain allowed.

CREATE TRIGGER acp_cleanup_scope BEFORE UPDATE ON acp_submissions
WHEN NEW.kept_cleanup_json IS NOT NULL
BEGIN
  SELECT CASE WHEN NEW.state IS NOT 'unknown'
    OR json_extract(NEW.input_json, '$.version') IS NOT 'canonical_v1'
    OR NOT json_valid(NEW.kept_cleanup_json)
    THEN RAISE(ABORT, 'ACP cleanup requires canonical Unknown evidence') END;
  SELECT CASE WHEN json_type(NEW.kept_cleanup_json) IS NOT 'object'
    OR (SELECT count(*) FROM json_each(NEW.kept_cleanup_json)) != 12
    OR json_extract(NEW.kept_cleanup_json, '$.version') IS NOT 'v1'
    OR json_extract(NEW.kept_cleanup_json, '$.worker_session_id') IS NOT NEW.worker_session_id
    OR json_extract(NEW.kept_cleanup_json, '$.client_id') IS NOT NEW.client_id
    OR json_extract(NEW.kept_cleanup_json, '$.turn_id') IS NOT NEW.turn_id
    OR json_extract(NEW.kept_cleanup_json, '$.thread_id') IS NOT NEW.thread_id
    OR json_extract(NEW.kept_cleanup_json, '$.native_session_id') IS NOT NEW.native_session_id
    OR json_extract(NEW.kept_cleanup_json, '$.association') IS NOT json_extract(NEW.input_json, '$.association')
    OR json_type(NEW.kept_cleanup_json, '$.registration_digest') IS NOT 'text'
    OR length(json_extract(NEW.kept_cleanup_json, '$.registration_digest')) = 0
    OR json_type(NEW.kept_cleanup_json, '$.keep_key') IS NOT 'text'
    OR json_type(NEW.kept_cleanup_json, '$.keep_principal') IS NOT 'text'
    OR json_type(NEW.kept_cleanup_json, '$.keep_fingerprint') IS NOT 'text'
    OR json_extract(NEW.kept_cleanup_json, '$.observation') IS NOT 'stopped_revoked_normally_joined'
    THEN RAISE(ABORT, 'ACP cleanup evidence shape or association changed') END;
END;

CREATE TRIGGER acp_cleanup_immutable BEFORE UPDATE ON acp_submissions
WHEN OLD.kept_cleanup_json IS NOT NULL OR NEW.kept_cleanup_json IS NOT NULL
BEGIN
  SELECT CASE WHEN (OLD.kept_cleanup_json IS NOT NULL AND
      NEW.kept_cleanup_json IS NOT OLD.kept_cleanup_json)
    OR NEW.worker_session_id IS NOT OLD.worker_session_id
    OR NEW.client_id IS NOT OLD.client_id
    OR NEW.turn_id IS NOT OLD.turn_id
    OR NEW.thread_id IS NOT OLD.thread_id
    OR NEW.native_session_id IS NOT OLD.native_session_id
    OR NEW.input_json IS NOT OLD.input_json
    OR NEW.created_at_ms IS NOT OLD.created_at_ms
    OR NEW.state IS NOT OLD.state
    OR NEW.outcome_json IS NOT OLD.outcome_json
    THEN RAISE(ABORT, 'ACP released Unknown receipt is immutable') END;
END;
