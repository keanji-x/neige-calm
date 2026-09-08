-- A1: retained candidates are not qualified delivery receipts.
CREATE TABLE task_file_candidates (
  operation_id TEXT PRIMARY KEY NOT NULL REFERENCES operations(id),
  track_id TEXT NOT NULL REFERENCES tracks(id) ON DELETE CASCADE,
  producer_attempt_id TEXT NOT NULL,
  slot TEXT NOT NULL,
  candidate_json TEXT NOT NULL CHECK(json_valid(candidate_json)),
  UNIQUE(producer_attempt_id, slot),
  UNIQUE(operation_id, track_id)
);
CREATE TRIGGER task_file_candidate_immutable BEFORE UPDATE ON task_file_candidates
BEGIN SELECT RAISE(ABORT, 'candidate identity is immutable'); END;
CREATE TABLE task_candidate_input_bindings (
  attempt_id TEXT PRIMARY KEY NOT NULL,
  track_id TEXT NOT NULL REFERENCES tracks(id) ON DELETE CASCADE,
  publication_operation_id TEXT NOT NULL,
  verification_operation_id TEXT NOT NULL REFERENCES operations(id),
  binding_json TEXT NOT NULL CHECK(json_valid(binding_json)),
  state TEXT NOT NULL CHECK(state IN ('bound','prepared')),
  prepared_operation_id TEXT,
  FOREIGN KEY(publication_operation_id,track_id) REFERENCES task_file_candidates(operation_id,track_id),
  CHECK((state='bound' AND prepared_operation_id IS NULL) OR
        (state='prepared' AND prepared_operation_id IS NOT NULL))
);
CREATE TRIGGER task_candidate_binding_immutable BEFORE UPDATE ON task_candidate_input_bindings
WHEN NEW.attempt_id != OLD.attempt_id OR NEW.track_id != OLD.track_id
 OR NEW.publication_operation_id != OLD.publication_operation_id
 OR NEW.verification_operation_id != OLD.verification_operation_id
 OR NEW.binding_json != OLD.binding_json OR OLD.state='prepared'
BEGIN SELECT RAISE(ABORT, 'candidate input identity is immutable'); END;
-- Durable reservation before Operation submission; missing Operation after a crash
-- still consumes capacity until the same reservation is submitted and settled.
CREATE TABLE task_candidate_verification_allocations (
  publication_operation_id TEXT PRIMARY KEY NOT NULL REFERENCES task_file_candidates(operation_id),
  track_id TEXT NOT NULL REFERENCES tracks(id) ON DELETE CASCADE,
  operation_key TEXT NOT NULL UNIQUE
);
CREATE TRIGGER task_candidate_verification_allocation_immutable BEFORE UPDATE ON task_candidate_verification_allocations
BEGIN SELECT RAISE(ABORT, 'candidate verification allocation is immutable'); END;

UPDATE events SET event_version = 20 WHERE kind = 'task.candidate_verification_settled';

-- Backstop raw Track/Area cascades, including callers outside repository guards.
CREATE TRIGGER task_candidate_verification_allocation_delete_guard
BEFORE DELETE ON task_candidate_verification_allocations
WHEN NOT EXISTS (SELECT 1 FROM operations o WHERE o.operation_key=OLD.operation_key
  AND o.kind='candidate-verify' AND o.phase IN ('succeeded','failed'))
BEGIN
  SELECT RAISE(ABORT, 'unresolved candidate verification; wait for verification or owned cleanup to settle before deletion');
END;
