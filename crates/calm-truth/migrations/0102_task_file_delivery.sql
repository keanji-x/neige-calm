-- F4: retained immutable publication evidence and claim-time input identity.
CREATE TABLE task_file_publications (
  operation_id TEXT PRIMARY KEY NOT NULL,
  track_id TEXT NOT NULL REFERENCES tracks(id) ON DELETE CASCADE,
  producer_attempt_id TEXT NOT NULL,
  source_operation_id TEXT NOT NULL,
  slot TEXT NOT NULL,
  receipt_json TEXT NOT NULL CHECK(json_valid(receipt_json)),
  UNIQUE(producer_attempt_id, slot),
  UNIQUE(operation_id, track_id)
);
CREATE TRIGGER task_file_publication_immutable BEFORE UPDATE ON task_file_publications
BEGIN SELECT RAISE(ABORT, 'file publication identity is immutable'); END;
CREATE TABLE task_file_input_bindings (
  attempt_id TEXT PRIMARY KEY NOT NULL,
  track_id TEXT NOT NULL REFERENCES tracks(id) ON DELETE CASCADE,
  publication_operation_id TEXT NOT NULL,
  binding_json TEXT NOT NULL CHECK(json_valid(binding_json)),
  state TEXT NOT NULL CHECK(state IN ('bound','prepared')),
  prepared_operation_id TEXT,
  FOREIGN KEY(publication_operation_id,track_id) REFERENCES task_file_publications(operation_id,track_id) ON DELETE CASCADE,
  CHECK((state='bound' AND prepared_operation_id IS NULL) OR
        (state='prepared' AND prepared_operation_id IS NOT NULL))
);
CREATE TRIGGER task_file_binding_immutable BEFORE UPDATE ON task_file_input_bindings
WHEN NEW.attempt_id != OLD.attempt_id OR NEW.track_id != OLD.track_id
  OR NEW.publication_operation_id != OLD.publication_operation_id OR NEW.binding_json != OLD.binding_json
  OR OLD.state='prepared'
BEGIN SELECT RAISE(ABORT, 'file input identity is immutable'); END;

-- New settlement vocabulary requires clients to negotiate event version 19.
UPDATE events SET event_version = 19 WHERE kind = 'task.file_publication_settled';
