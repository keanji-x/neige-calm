-- Schema only: no imported rows and no card contract transitions.
PRAGMA defer_foreign_keys=ON;
CREATE TEMP TABLE imported_backup AS SELECT * FROM conversation_presentations;
CREATE TEMP TABLE imported_sequence AS SELECT seq FROM sqlite_sequence WHERE name='conversation_presentations';
DROP TABLE conversation_presentations;
CREATE TABLE conversation_presentations (
    sequence INTEGER PRIMARY KEY AUTOINCREMENT,
    card_id TEXT NOT NULL REFERENCES cards(id) ON DELETE CASCADE,
    source_kind TEXT NOT NULL CHECK(source_kind IN ('human_message','system_observation','native_item','imported_item')),
    source_identity TEXT NOT NULL CHECK(length(source_identity)>0),
    message_id TEXT,
    observation_id TEXT,
    native_item_row_id INTEGER REFERENCES harness_items(id) ON DELETE SET NULL,
    submission_id TEXT REFERENCES conversation_input_submissions(id) ON DELETE SET NULL,
    erased_by_operation_id TEXT,
    imported_provenance_json TEXT CHECK(imported_provenance_json IS NULL OR json_valid(imported_provenance_json)),
    import_boundary_sequence INTEGER,
    imported_native_identity TEXT,
    UNIQUE(card_id,source_kind,source_identity),
    FOREIGN KEY(message_id,card_id) REFERENCES conversation_messages(id,card_id) ON DELETE CASCADE,
    CHECK(erased_by_operation_id IS NULL OR (length(erased_by_operation_id)=32 AND erased_by_operation_id NOT GLOB '*[^0-9a-f]*')),
    CHECK(source_kind!='human_message' OR source_identity=message_id),
    CHECK(source_kind!='system_observation' OR (source_identity=observation_id AND length(observation_id)=32 AND observation_id NOT GLOB '*[^0-9a-f]*')),
    CHECK((source_kind='human_message' AND message_id IS NOT NULL AND observation_id IS NULL AND native_item_row_id IS NULL)
       OR (source_kind='system_observation' AND message_id IS NULL AND observation_id IS NOT NULL AND native_item_row_id IS NULL)
       OR (source_kind IN ('native_item','imported_item') AND message_id IS NULL AND observation_id IS NULL)),
    CHECK((source_kind='imported_item' AND submission_id IS NULL
       AND imported_provenance_json IS NOT NULL AND import_boundary_sequence IS NOT NULL
       AND import_boundary_sequence>=0 AND sequence>import_boundary_sequence
       AND json_extract(imported_provenance_json,'$.kind') IS 'unknown_legacy'
       AND source_identity IS CAST(json_extract(imported_provenance_json,'$.original_row_id') AS TEXT)
       AND card_id IS json_extract(imported_provenance_json,'$.card_id')
       AND import_boundary_sequence IS json_extract(imported_provenance_json,'$.import_boundary_sequence'))
       OR (source_kind!='imported_item' AND imported_provenance_json IS NULL
           AND import_boundary_sequence IS NULL AND imported_native_identity IS NULL))
);
INSERT INTO conversation_presentations(sequence,card_id,source_kind,source_identity,message_id,
 observation_id,native_item_row_id,submission_id,erased_by_operation_id) SELECT * FROM imported_backup;
DELETE FROM sqlite_sequence WHERE name='conversation_presentations';
INSERT INTO sqlite_sequence(name,seq) SELECT 'conversation_presentations',seq FROM imported_sequence;
DROP TABLE imported_backup;
DROP TABLE imported_sequence;
CREATE TRIGGER imported_presentation_immutable BEFORE UPDATE ON conversation_presentations
WHEN OLD.source_kind='imported_item' AND (NEW.sequence IS NOT OLD.sequence
 OR NEW.card_id IS NOT OLD.card_id OR NEW.source_kind IS NOT OLD.source_kind
 OR NEW.source_identity IS NOT OLD.source_identity
 OR NEW.imported_provenance_json IS NOT OLD.imported_provenance_json
 OR NEW.import_boundary_sequence IS NOT OLD.import_boundary_sequence
 OR NEW.imported_native_identity IS NOT OLD.imported_native_identity
 OR (NEW.native_item_row_id IS NOT OLD.native_item_row_id AND NEW.native_item_row_id IS NOT NULL)
 OR (OLD.erased_by_operation_id IS NOT NULL AND NEW.erased_by_operation_id IS NOT OLD.erased_by_operation_id))
BEGIN
 SELECT RAISE(ABORT, 'Imported presentation provenance is immutable');
END;
CREATE TRIGGER imported_presentation_scope BEFORE INSERT ON conversation_presentations
WHEN NEW.source_kind='imported_item' AND NOT EXISTS (
 SELECT 1 FROM harness_items WHERE id=NEW.native_item_row_id AND card_id=NEW.card_id
 AND id=CAST(NEW.source_identity AS INTEGER))
BEGIN
 SELECT RAISE(ABORT, 'Imported presentation requires its original row');
END;
