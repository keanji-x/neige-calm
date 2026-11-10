-- Only receipts written by a boot-sweep-owned shared process qualify.
ALTER TABLE acp_submissions ADD COLUMN shared_restart_owner_json TEXT
  CHECK(shared_restart_owner_json IS NULL OR json_valid(shared_restart_owner_json));
ALTER TABLE acp_submissions ADD COLUMN restart_released_at_ms INTEGER
  CHECK(restart_released_at_ms IS NULL OR
    (shared_restart_owner_json IS NOT NULL AND state='unknown' AND kept_cleanup_json IS NULL
     AND json_extract(input_json,'$.version') IS 'canonical_v1'));

DROP INDEX acp_one_unresolved_submission;
CREATE UNIQUE INDEX acp_one_unresolved_submission ON acp_submissions(worker_session_id)
  WHERE state = 'sending' OR (state = 'unknown' AND kept_cleanup_json IS NULL AND restart_released_at_ms IS NULL);

CREATE TRIGGER acp_restart_release_update BEFORE UPDATE ON acp_submissions
WHEN NEW.shared_restart_owner_json IS NOT OLD.shared_restart_owner_json
  OR OLD.restart_released_at_ms IS NOT NULL
BEGIN
  SELECT RAISE(ABORT, 'ACP restart provenance and released receipts are immutable');
END;
CREATE TRIGGER acp_restart_release_insert BEFORE INSERT ON acp_submissions
WHEN NEW.restart_released_at_ms IS NOT NULL OR EXISTS(
  SELECT 1 FROM acp_submissions WHERE restart_released_at_ms IS NOT NULL AND
    ((worker_session_id=NEW.worker_session_id AND client_id=NEW.client_id) OR turn_id=NEW.turn_id)
)
BEGIN
  SELECT RAISE(ABORT, 'ACP restart release requires its original receipt');
END;
CREATE TRIGGER acp_restart_release_collision BEFORE UPDATE OF worker_session_id,client_id,turn_id ON acp_submissions
WHEN EXISTS(
  SELECT 1 FROM acp_submissions WHERE restart_released_at_ms IS NOT NULL AND
    ((worker_session_id=NEW.worker_session_id AND client_id=NEW.client_id) OR turn_id=NEW.turn_id)
)
BEGIN
  SELECT RAISE(ABORT, 'ACP restart release requires its original receipt');
END;
