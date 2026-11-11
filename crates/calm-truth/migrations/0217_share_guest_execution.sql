-- #3633: LinkGuest authors and immutable execution approval snapshots.
-- Track history deliberately has no FK to Area-scoped share_links: a moved
-- Track survives deletion of its former Area and its original delegation.
PRAGMA defer_foreign_keys=ON;
CREATE TEMP TABLE share_guest_backup_fk AS SELECT * FROM pragma_foreign_key_check;
CREATE TEMP TABLE share_guest_backup_sequence AS SELECT * FROM sqlite_sequence WHERE name='conversation_presentations';
CREATE TEMP TABLE share_guest_backup_conversation_presentations AS SELECT * FROM conversation_presentations;
CREATE TEMP TABLE share_guest_backup_conversation_submission_inputs AS SELECT * FROM conversation_submission_inputs;
CREATE TEMP TABLE share_guest_backup_conversation_input_retry_relations AS SELECT * FROM conversation_input_retry_relations;
CREATE TEMP TABLE share_guest_backup_conversation_input_submissions AS SELECT * FROM conversation_input_submissions;
CREATE TEMP TABLE share_guest_backup_conversation_turns AS SELECT * FROM conversation_turns;
CREATE TEMP TABLE share_guest_backup_conversation_message_revisions AS SELECT * FROM conversation_message_revisions;
CREATE TEMP TABLE share_guest_backup_conversation_messages AS SELECT * FROM conversation_messages;
DROP TABLE conversation_presentations;
DROP TABLE conversation_submission_inputs;
DROP TABLE conversation_input_retry_relations;
DROP TABLE conversation_input_submissions;
DROP TABLE conversation_turns;
DROP TABLE conversation_message_revisions;
DROP TABLE conversation_messages;
CREATE TABLE conversation_messages (
    id TEXT PRIMARY KEY NOT NULL CHECK(length(id)=32 AND id NOT GLOB '*[^0-9a-f]*'),
    card_id TEXT NOT NULL REFERENCES cards(id) ON DELETE CASCADE,
    current_revision INTEGER NOT NULL CHECK(current_revision > 0),
    author_kind TEXT NOT NULL CHECK(author_kind IN ('authenticated_human','link_guest')),
    author_user_id TEXT REFERENCES application_users(id) ON DELETE RESTRICT,
    author_share_link_id TEXT,
    created_at_ms INTEGER NOT NULL,
    withdrawn_at_ms INTEGER,
    CHECK((author_kind='authenticated_human' AND author_user_id IS NOT NULL AND author_share_link_id IS NULL)
       OR (author_kind='link_guest' AND author_user_id IS NULL AND author_share_link_id IS NOT NULL)),
    UNIQUE(id,card_id),
    FOREIGN KEY(id,current_revision)
      REFERENCES conversation_message_revisions(message_id,revision)
      DEFERRABLE INITIALLY DEFERRED
);
CREATE TABLE conversation_message_revisions (
    message_id TEXT NOT NULL REFERENCES conversation_messages(id) ON DELETE CASCADE,
    revision INTEGER NOT NULL CHECK(revision>0),
    body_sha256 TEXT NOT NULL CHECK(length(body_sha256)=64 AND body_sha256 NOT GLOB '*[^0-9a-f]*'),
    payload_json TEXT NOT NULL CHECK(json_valid(payload_json)),
    created_at_ms INTEGER NOT NULL,
    PRIMARY KEY(message_id,revision),
    CHECK(CASE WHEN json_valid(payload_json) THEN
      (json_extract(payload_json,'$.state')='present'
       AND json_type(payload_json,'$.value')='object'
       AND json_type(payload_json,'$.operation_id') IS NULL)
      OR (json_extract(payload_json,'$.state')='erased_by_private_history_operation'
          AND length(json_extract(payload_json,'$.operation_id'))=32
          AND json_extract(payload_json,'$.operation_id') NOT GLOB '*[^0-9a-f]*'
          AND json_type(payload_json,'$.value') IS NULL)
      ELSE 0 END IS TRUE)
);
CREATE TABLE conversation_turns (
    id TEXT PRIMARY KEY NOT NULL CHECK(length(id)=32 AND id NOT GLOB '*[^0-9a-f]*'),
    card_id TEXT NOT NULL REFERENCES cards(id) ON DELETE CASCADE,
    initiating_runtime_id TEXT NOT NULL,
    initial_submission_id TEXT NOT NULL UNIQUE,
    execution_kind TEXT NOT NULL CHECK(execution_kind IN ('legacy_single_user_deployment','share_delegation')),
    deployment_owner_user_id TEXT NOT NULL REFERENCES application_users(id) ON DELETE RESTRICT,
    provider TEXT NOT NULL CHECK(provider IN ('codex','claude','opencode','grok','antigravity')),
    profile TEXT NOT NULL CHECK(profile IN ('spec','plain_chat','assistant')),
    created_at_ms INTEGER NOT NULL,
    share_delegation_json TEXT CHECK(share_delegation_json IS NULL OR json_valid(share_delegation_json)),
    CHECK((execution_kind='legacy_single_user_deployment' AND share_delegation_json IS NULL)
       OR (execution_kind='share_delegation' AND share_delegation_json IS NOT NULL
          AND json_extract(share_delegation_json,'$.approverUserId') IS deployment_owner_user_id)),
    UNIQUE(id,card_id),
    FOREIGN KEY(initial_submission_id,id,card_id)
      REFERENCES conversation_input_submissions(id,logical_turn_id,card_id)
      DEFERRABLE INITIALLY DEFERRED
);
CREATE TABLE conversation_input_submissions (
    id TEXT PRIMARY KEY NOT NULL CHECK(length(id)=32 AND id NOT GLOB '*[^0-9a-f]*'),
    card_id TEXT NOT NULL REFERENCES cards(id) ON DELETE CASCADE,
    logical_turn_id TEXT NOT NULL,
    ordinal INTEGER NOT NULL CHECK(ordinal>=0),
    kind TEXT NOT NULL CHECK(kind IN ('initial','supplemental')),
    render_plan_json TEXT CHECK(render_plan_json IS NULL OR json_valid(render_plan_json)),
    render_plan_root_submission_id TEXT REFERENCES conversation_input_submissions(id) ON DELETE NO ACTION
      DEFERRABLE INITIALLY DEFERRED,
    retry_intent_id TEXT REFERENCES conversation_input_retry_relations(retry_intent_id) ON DELETE NO ACTION DEFERRABLE INITIALLY DEFERRED,
    render_plan_sha256 TEXT NOT NULL CHECK(length(render_plan_sha256)=64 AND render_plan_sha256 NOT GLOB '*[^0-9a-f]*'),
    native_correlation_json TEXT CHECK(native_correlation_json IS NULL OR json_valid(native_correlation_json)),
    created_at_ms INTEGER NOT NULL,
    UNIQUE(logical_turn_id,ordinal),
    UNIQUE(id,logical_turn_id,card_id),
    FOREIGN KEY(logical_turn_id,card_id) REFERENCES conversation_turns(id,card_id) ON DELETE CASCADE,
    CHECK((render_plan_json IS NOT NULL AND render_plan_root_submission_id IS NULL AND retry_intent_id IS NULL)
       OR (render_plan_json IS NULL AND render_plan_root_submission_id IS NOT NULL AND retry_intent_id IS NOT NULL)),
    CHECK((kind='initial' AND ordinal=0) OR (kind='supplemental' AND ordinal>0 AND retry_intent_id IS NULL AND render_plan_root_submission_id IS NULL))
);
CREATE TABLE conversation_input_retry_relations (
    retry_intent_id TEXT PRIMARY KEY NOT NULL
      CHECK(length(retry_intent_id)=32 AND retry_intent_id NOT GLOB '*[^0-9a-f]*'),
    card_id TEXT NOT NULL REFERENCES cards(id) ON DELETE CASCADE,
    carrier_id TEXT NOT NULL,
    predecessor_submission_id TEXT NOT NULL REFERENCES conversation_input_submissions(id) ON DELETE NO ACTION DEFERRABLE INITIALLY DEFERRED,
    root_submission_id TEXT NOT NULL REFERENCES conversation_input_submissions(id) ON DELETE NO ACTION DEFERRABLE INITIALLY DEFERRED,
    predecessor_attempt_id TEXT NOT NULL CHECK(length(predecessor_attempt_id)=32 AND predecessor_attempt_id NOT GLOB '*[^0-9a-f]*'),
    resolution_key TEXT NOT NULL CHECK(length(resolution_key)>0 AND length(resolution_key)<=128),
    principal_user_id TEXT NOT NULL REFERENCES application_users(id) ON DELETE RESTRICT,
    fingerprint TEXT NOT NULL CHECK(length(fingerprint)=64 AND fingerprint NOT GLOB '*[^0-9a-f]*'),
    created_at_ms INTEGER NOT NULL,
    queue_position INTEGER NOT NULL CHECK(queue_position>=0),
    UNIQUE(carrier_id,predecessor_attempt_id),
    UNIQUE(carrier_id,resolution_key),
    CHECK(retry_intent_id!=predecessor_submission_id AND retry_intent_id!=root_submission_id)
);
CREATE TABLE conversation_submission_inputs (
    submission_id TEXT NOT NULL REFERENCES conversation_input_submissions(id) ON DELETE CASCADE,
    input_position INTEGER NOT NULL CHECK(input_position>=0),
    input_kind TEXT NOT NULL CHECK(input_kind IN ('human','system')),
    message_id TEXT,
    revision INTEGER,
    observation_id TEXT CHECK(observation_id IS NULL OR (length(observation_id)=32 AND observation_id NOT GLOB '*[^0-9a-f]*')),
    queue_position INTEGER NOT NULL CHECK(queue_position>=0),
    queued_at_ms INTEGER NOT NULL,
    envelope_id INTEGER,
    system_addressable INTEGER CHECK(system_addressable IN (0,1)),
    effective INTEGER NOT NULL CHECK(effective IN (0,1)),
    released_by_attempt_id TEXT CHECK(released_by_attempt_id IS NULL OR (length(released_by_attempt_id)=32 AND released_by_attempt_id NOT GLOB '*[^0-9a-f]*')),
    retirement_reason TEXT CHECK(retirement_reason IN ('not_dispatched','human_resolution')),
    retired_by_retry_intent_id TEXT REFERENCES conversation_input_retry_relations(retry_intent_id) ON DELETE NO ACTION
      DEFERRABLE INITIALLY DEFERRED,
    PRIMARY KEY(submission_id,input_position),
    FOREIGN KEY(message_id,revision) REFERENCES conversation_message_revisions(message_id,revision),
    CHECK((input_kind='human' AND message_id IS NOT NULL AND revision IS NOT NULL AND observation_id IS NULL AND system_addressable IS NULL)
       OR (input_kind='system' AND message_id IS NULL AND revision IS NULL AND observation_id IS NOT NULL AND system_addressable IS NOT NULL)),
    CHECK(((effective=1 AND retirement_reason IS NULL AND released_by_attempt_id IS NULL
           AND retired_by_retry_intent_id IS NULL)
       OR (effective=0 AND retirement_reason='not_dispatched' AND released_by_attempt_id IS NOT NULL
           AND retired_by_retry_intent_id IS NULL)
       OR (effective=0 AND retirement_reason='human_resolution' AND released_by_attempt_id IS NULL
           AND retired_by_retry_intent_id IS NOT NULL)) IS TRUE)
);
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
INSERT INTO conversation_messages(id,card_id,current_revision,author_kind,author_user_id,created_at_ms,withdrawn_at_ms) SELECT id,card_id,current_revision,author_kind,author_user_id,created_at_ms,withdrawn_at_ms FROM share_guest_backup_conversation_messages;
INSERT INTO conversation_message_revisions(message_id,revision,body_sha256,payload_json,created_at_ms) SELECT message_id,revision,body_sha256,payload_json,created_at_ms FROM share_guest_backup_conversation_message_revisions;
INSERT INTO conversation_turns(id,card_id,initiating_runtime_id,initial_submission_id,execution_kind,deployment_owner_user_id,provider,profile,created_at_ms) SELECT id,card_id,initiating_runtime_id,initial_submission_id,execution_kind,deployment_owner_user_id,provider,profile,created_at_ms FROM share_guest_backup_conversation_turns;
INSERT INTO conversation_input_submissions(id,card_id,logical_turn_id,ordinal,kind,render_plan_json,render_plan_root_submission_id,retry_intent_id,render_plan_sha256,native_correlation_json,created_at_ms) SELECT id,card_id,logical_turn_id,ordinal,kind,render_plan_json,render_plan_root_submission_id,retry_intent_id,render_plan_sha256,native_correlation_json,created_at_ms FROM share_guest_backup_conversation_input_submissions;
INSERT INTO conversation_input_retry_relations(retry_intent_id,card_id,carrier_id,predecessor_submission_id,root_submission_id,predecessor_attempt_id,resolution_key,principal_user_id,fingerprint,created_at_ms,queue_position) SELECT retry_intent_id,card_id,carrier_id,predecessor_submission_id,root_submission_id,predecessor_attempt_id,resolution_key,principal_user_id,fingerprint,created_at_ms,queue_position FROM share_guest_backup_conversation_input_retry_relations;
INSERT INTO conversation_submission_inputs(submission_id,input_position,input_kind,message_id,revision,observation_id,queue_position,queued_at_ms,envelope_id,system_addressable,effective,released_by_attempt_id,retirement_reason,retired_by_retry_intent_id) SELECT submission_id,input_position,input_kind,message_id,revision,observation_id,queue_position,queued_at_ms,envelope_id,system_addressable,effective,released_by_attempt_id,retirement_reason,retired_by_retry_intent_id FROM share_guest_backup_conversation_submission_inputs;
INSERT INTO conversation_presentations(sequence,card_id,source_kind,source_identity,message_id,observation_id,native_item_row_id,submission_id,erased_by_operation_id,imported_provenance_json,import_boundary_sequence,imported_native_identity) SELECT sequence,card_id,source_kind,source_identity,message_id,observation_id,native_item_row_id,submission_id,erased_by_operation_id,imported_provenance_json,import_boundary_sequence,imported_native_identity FROM share_guest_backup_conversation_presentations;
DELETE FROM sqlite_sequence WHERE name='conversation_presentations';
INSERT INTO sqlite_sequence SELECT * FROM share_guest_backup_sequence;
CREATE TRIGGER conversation_message_identity_immutable
BEFORE UPDATE ON conversation_messages
WHEN NEW.id IS NOT OLD.id OR NEW.card_id IS NOT OLD.card_id
 OR NEW.author_kind IS NOT OLD.author_kind OR NEW.author_user_id IS NOT OLD.author_user_id
 OR NEW.author_share_link_id IS NOT OLD.author_share_link_id
 OR NEW.created_at_ms IS NOT OLD.created_at_ms
 OR NEW.current_revision < OLD.current_revision
 OR (OLD.withdrawn_at_ms IS NOT NULL AND NEW.withdrawn_at_ms IS NOT OLD.withdrawn_at_ms)
BEGIN
    SELECT RAISE(ABORT, 'Canonical message identity and provenance are immutable');
END;
CREATE TRIGGER conversation_revision_only_private_erasure
BEFORE UPDATE ON conversation_message_revisions
WHEN NEW.message_id IS NOT OLD.message_id OR NEW.revision IS NOT OLD.revision
 OR NEW.body_sha256 IS NOT OLD.body_sha256 OR NEW.created_at_ms IS NOT OLD.created_at_ms
 OR json_extract(OLD.payload_json,'$.state') != 'present'
 OR json_extract(NEW.payload_json,'$.state') != 'erased_by_private_history_operation'
BEGIN
    SELECT RAISE(ABORT, 'Canonical revisions are immutable except explicit private erasure');
END;
CREATE TRIGGER conversation_retry_relation_scope
BEFORE INSERT ON conversation_input_retry_relations
WHEN EXISTS(SELECT 1 FROM conversation_input_retry_relations old
    WHERE old.retry_intent_id=NEW.retry_intent_id
       OR (old.carrier_id=NEW.carrier_id AND (old.predecessor_attempt_id=NEW.predecessor_attempt_id
                                            OR old.resolution_key=NEW.resolution_key)))
 OR NOT EXISTS(SELECT 1 FROM (SELECT
    NEW.retry_intent_id AS retry_intent_id,
    NEW.card_id AS card_id,
    NEW.carrier_id AS carrier_id,
    NEW.predecessor_submission_id AS predecessor_submission_id,
    NEW.root_submission_id AS root_submission_id,
    NEW.predecessor_attempt_id AS predecessor_attempt_id,
    NEW.resolution_key AS resolution_key,
    NEW.principal_user_id AS principal_user_id,
    NEW.fingerprint AS fingerprint,
    NEW.queue_position AS queue_position,
    NEW.created_at_ms AS created_at_ms) r JOIN worker_sessions ws
    JOIN conversation_input_submissions predecessor ON predecessor.id=r.predecessor_submission_id
    JOIN conversation_turns original ON original.id=predecessor.logical_turn_id
    JOIN conversation_input_submissions root ON root.id=r.root_submission_id
    JOIN json_each(CASE WHEN json_valid(ws.handle_state_json) THEN ws.handle_state_json ELSE '{}' END,
                   '$.canonical_inputs.receipts') receipt
    JOIN json_each(ws.handle_state_json,'$.canonical_inputs.attempts') attempt
    JOIN json_each(ws.handle_state_json,'$.canonical_inputs.pending') pending
    WHERE json_extract(pending.value,'$.kind')='explicit_successor'
      AND json_extract(pending.value,'$.retry_intent_id')=r.retry_intent_id
      AND json_extract(pending.value,'$.queue_position')=r.queue_position
      AND json_extract(pending.value,'$.queued_at_ms')=r.created_at_ms
      AND ws.id=r.carrier_id AND ws.card_id=r.card_id
      AND predecessor.card_id=r.card_id AND original.initiating_runtime_id=r.carrier_id
      AND root.card_id=r.card_id AND root.render_plan_root_submission_id IS NULL
      AND root.render_plan_json IS NOT NULL
      AND r.root_submission_id=COALESCE(predecessor.render_plan_root_submission_id,predecessor.id)
      AND json_extract(ws.handle_state_json,'$.schema_version')=5
      AND json_extract(receipt.value,'$.key')=r.resolution_key
      AND json_extract(receipt.value,'$.principal')=r.principal_user_id
      AND json_extract(receipt.value,'$.fingerprint')=r.fingerprint
      AND json_extract(receipt.value,'$.decision.kind')='retry'
      AND json_extract(receipt.value,'$.decision.predecessor.carrier_id')=r.carrier_id
      AND json_extract(receipt.value,'$.decision.predecessor.submission_id')=r.predecessor_submission_id
      AND json_extract(receipt.value,'$.decision.predecessor.attempt_id')=r.predecessor_attempt_id
      AND json_extract(receipt.value,'$.decision.retry_intent_id')=r.retry_intent_id
      AND json_extract(attempt.value,'$.attempt.submission_id')=r.predecessor_submission_id
      AND json_extract(attempt.value,'$.attempt.attempt_id')=r.predecessor_attempt_id
      AND json_type(receipt.value,'$.decision.predecessor.revision')='text'
      AND json_type(receipt.value,'$.decision.revision')='text'
      AND CAST(json_extract(receipt.value,'$.decision.predecessor.revision') AS INTEGER)>=0
      AND CAST(CAST(json_extract(receipt.value,'$.decision.predecessor.revision') AS INTEGER) AS TEXT)
          =json_extract(receipt.value,'$.decision.predecessor.revision')
      AND CAST(CAST(json_extract(receipt.value,'$.decision.revision') AS INTEGER) AS TEXT)
          =json_extract(receipt.value,'$.decision.revision')
      AND json_type(attempt.value,'$.revision')='integer'
      AND CAST(json_extract(receipt.value,'$.decision.predecessor.revision') AS INTEGER)+1
          =CAST(json_extract(receipt.value,'$.decision.revision') AS INTEGER)
      AND json_extract(attempt.value,'$.revision')>=CAST(json_extract(receipt.value,'$.decision.revision') AS INTEGER))
BEGIN
    SELECT RAISE(ABORT, 'Retry relation requires its exact original-carrier receipt and reserved identities');
END;
CREATE TRIGGER conversation_retry_relation_immutable
BEFORE UPDATE ON conversation_input_retry_relations
BEGIN
    SELECT RAISE(ABORT, 'Retry relation identity is immutable');
END;
CREATE TRIGGER conversation_retry_relation_delete_only_with_card
BEFORE DELETE ON conversation_input_retry_relations
WHEN EXISTS(SELECT 1 FROM cards WHERE id=OLD.card_id)
BEGIN
    SELECT RAISE(ABORT, 'Retry relation survives until whole-card deletion');
END;
CREATE TRIGGER conversation_turn_provenance_immutable
BEFORE UPDATE ON conversation_turns
BEGIN
    SELECT RAISE(ABORT, 'Logical Turn provenance is immutable');
END;
CREATE UNIQUE INDEX conversation_one_initial_submission
    ON conversation_input_submissions(logical_turn_id) WHERE kind='initial';
CREATE TRIGGER conversation_submission_initial_identity
BEFORE INSERT ON conversation_input_submissions
WHEN (NEW.kind='initial') != (NEW.id=(SELECT initial_submission_id
     FROM conversation_turns WHERE id=NEW.logical_turn_id))
BEGIN
    SELECT RAISE(ABORT, 'Turn initial submission identity must match its required initial claim');
END;
CREATE TRIGGER conversation_turn_existing_initial_identity
BEFORE INSERT ON conversation_turns
WHEN EXISTS(SELECT 1 FROM conversation_input_submissions
            WHERE id=NEW.initial_submission_id AND
              (logical_turn_id!=NEW.id OR card_id!=NEW.card_id
               OR kind!='initial' OR ordinal!=0))
BEGIN
    SELECT RAISE(ABORT, 'Turn must name its own Initial submission at ordinal zero');
END;
CREATE TRIGGER conversation_submission_plan_immutable
BEFORE UPDATE ON conversation_input_submissions
WHEN NEW.id IS NOT OLD.id OR NEW.card_id IS NOT OLD.card_id
 OR NEW.logical_turn_id IS NOT OLD.logical_turn_id OR NEW.ordinal IS NOT OLD.ordinal
 OR NEW.kind IS NOT OLD.kind OR NEW.render_plan_sha256 IS NOT OLD.render_plan_sha256
 OR NEW.retry_intent_id IS NOT OLD.retry_intent_id
 OR NEW.render_plan_root_submission_id IS NOT OLD.render_plan_root_submission_id
 OR NEW.created_at_ms IS NOT OLD.created_at_ms
 OR (OLD.native_correlation_json IS NOT NULL
     AND NEW.native_correlation_json IS NOT OLD.native_correlation_json)
BEGIN
    SELECT RAISE(ABORT, 'Submission identity and original render digest are immutable');
END;
CREATE TRIGGER conversation_submission_only_private_erasure
BEFORE UPDATE OF render_plan_json ON conversation_input_submissions
WHEN NEW.render_plan_json IS NOT OLD.render_plan_json AND (
 OLD.render_plan_root_submission_id IS NOT NULL OR NEW.render_plan_json IS NULL
 OR OLD.render_plan_json IS NULL OR
 json_extract(NEW.render_plan_json,'$.version') IS NOT json_extract(OLD.render_plan_json,'$.version')
 OR json_array_length(NEW.render_plan_json,'$.segments') IS NOT json_array_length(OLD.render_plan_json,'$.segments')
 OR json_remove(NEW.render_plan_json,'$.segments') IS NOT json_remove(OLD.render_plan_json,'$.segments')
 OR EXISTS (
   SELECT 1 FROM json_each(OLD.render_plan_json,'$.segments') old_segment
   JOIN json_each(NEW.render_plan_json,'$.segments') new_segment USING(key)
   WHERE old_segment.value IS NOT new_segment.value AND (
     (json_extract(old_segment.value,'$.kind')='input'
      AND json_extract(old_segment.value,'$.input.kind')='system'
      AND json_extract(old_segment.value,'$.input.payload.state')='present'
      AND json_extract(new_segment.value,'$.input.payload.state')='erased_by_private_history_operation'
      AND length(json_extract(new_segment.value,'$.input.payload.operation_id'))=32
      AND json_extract(new_segment.value,'$.input.payload.operation_id') NOT GLOB '*[^0-9a-f]*'
      AND json_type(new_segment.value,'$.input.payload.value') IS NULL
      AND json_remove(old_segment.value,'$.input.payload')=json_remove(new_segment.value,'$.input.payload'))
     OR (json_extract(old_segment.value,'$.kind')='context'
      AND json_extract(old_segment.value,'$.payload.state')='present'
      AND json_extract(new_segment.value,'$.payload.state')='erased_by_private_history_operation'
      AND length(json_extract(new_segment.value,'$.payload.operation_id'))=32
      AND json_extract(new_segment.value,'$.payload.operation_id') NOT GLOB '*[^0-9a-f]*'
      AND json_type(new_segment.value,'$.payload.value') IS NULL
      AND json_remove(old_segment.value,'$.payload')=json_remove(new_segment.value,'$.payload'))
   ) IS NOT TRUE
 )
)
BEGIN
    SELECT RAISE(ABORT, 'Submission rendering changes only by explicit private erasure');
END;
CREATE UNIQUE INDEX conversation_one_effective_message_claim
    ON conversation_submission_inputs(message_id) WHERE effective=1 AND message_id IS NOT NULL;
CREATE UNIQUE INDEX conversation_one_effective_system_claim
    ON conversation_submission_inputs(observation_id) WHERE effective=1 AND observation_id IS NOT NULL;
CREATE TRIGGER conversation_submission_input_identity_immutable
BEFORE UPDATE ON conversation_submission_inputs
WHEN NEW.submission_id IS NOT OLD.submission_id OR NEW.input_position IS NOT OLD.input_position
 OR NEW.input_kind IS NOT OLD.input_kind OR NEW.message_id IS NOT OLD.message_id
 OR NEW.revision IS NOT OLD.revision OR NEW.observation_id IS NOT OLD.observation_id
 OR NEW.queue_position IS NOT OLD.queue_position OR NEW.queued_at_ms IS NOT OLD.queued_at_ms
 OR NEW.envelope_id IS NOT OLD.envelope_id OR NEW.system_addressable IS NOT OLD.system_addressable
 OR OLD.effective!=1 OR NEW.effective!=0
BEGIN
    SELECT RAISE(ABORT, 'Claims can only be released once by their exact attempt evidence');
END;
CREATE TRIGGER conversation_submission_insert_no_replace
BEFORE INSERT ON conversation_input_submissions
WHEN EXISTS(SELECT 1 FROM conversation_input_submissions WHERE id=NEW.id OR
            (logical_turn_id=NEW.logical_turn_id AND ordinal=NEW.ordinal))
BEGIN
    SELECT RAISE(ABORT, 'Submission facts cannot be replaced');
END;
CREATE TRIGGER conversation_submission_reference_scope
BEFORE INSERT ON conversation_input_submissions
WHEN NEW.render_plan_root_submission_id IS NOT NULL AND NOT EXISTS (
    SELECT 1 FROM conversation_input_submissions root
    JOIN conversation_input_retry_relations r ON r.retry_intent_id=NEW.retry_intent_id
    WHERE root.id=NEW.render_plan_root_submission_id AND root.id!=NEW.id
      AND root.card_id=NEW.card_id AND root.render_plan_root_submission_id IS NULL
      AND root.render_plan_json IS NOT NULL AND root.render_plan_sha256=NEW.render_plan_sha256
      AND r.root_submission_id=root.id AND r.card_id=NEW.card_id
)
BEGIN
    SELECT RAISE(ABORT, 'Reference must name its same-card inline retry root and original digest');
END;
CREATE TRIGGER conversation_submission_input_insert_no_replace
BEFORE INSERT ON conversation_submission_inputs
WHEN NEW.effective!=1 OR EXISTS(SELECT 1 FROM conversation_submission_inputs old
    WHERE (old.submission_id=NEW.submission_id AND old.input_position=NEW.input_position)
       OR (old.effective=1 AND NEW.effective=1 AND
           ((old.message_id IS NOT NULL AND old.message_id=NEW.message_id)
            OR (old.observation_id IS NOT NULL AND old.observation_id=NEW.observation_id))))
BEGIN
    SELECT RAISE(ABORT, 'Input claims cannot replace another consumer');
END;
CREATE TRIGGER conversation_submission_human_retirement_receipt
BEFORE UPDATE ON conversation_submission_inputs
WHEN NEW.retirement_reason='human_resolution' AND NOT EXISTS (
    SELECT 1 FROM conversation_input_retry_relations r
    WHERE r.retry_intent_id=NEW.retired_by_retry_intent_id
      AND r.predecessor_submission_id=OLD.submission_id
      AND EXISTS(SELECT 1 FROM worker_sessions ws
    JOIN conversation_input_submissions predecessor ON predecessor.id=r.predecessor_submission_id
    JOIN conversation_turns original ON original.id=predecessor.logical_turn_id
    JOIN conversation_input_submissions root ON root.id=r.root_submission_id
    JOIN json_each(CASE WHEN json_valid(ws.handle_state_json) THEN ws.handle_state_json ELSE '{}' END,
                   '$.canonical_inputs.receipts') receipt
    JOIN json_each(ws.handle_state_json,'$.canonical_inputs.attempts') attempt
    WHERE ws.id=r.carrier_id AND ws.card_id=r.card_id
      AND predecessor.card_id=r.card_id AND original.initiating_runtime_id=r.carrier_id
      AND root.card_id=r.card_id AND root.render_plan_root_submission_id IS NULL
      AND root.render_plan_json IS NOT NULL
      AND r.root_submission_id=COALESCE(predecessor.render_plan_root_submission_id,predecessor.id)
      AND json_extract(ws.handle_state_json,'$.schema_version')=5
      AND json_extract(receipt.value,'$.key')=r.resolution_key
      AND json_extract(receipt.value,'$.principal')=r.principal_user_id
      AND json_extract(receipt.value,'$.fingerprint')=r.fingerprint
      AND json_extract(receipt.value,'$.decision.kind')='retry'
      AND json_extract(receipt.value,'$.decision.predecessor.carrier_id')=r.carrier_id
      AND json_extract(receipt.value,'$.decision.predecessor.submission_id')=r.predecessor_submission_id
      AND json_extract(receipt.value,'$.decision.predecessor.attempt_id')=r.predecessor_attempt_id
      AND json_extract(receipt.value,'$.decision.retry_intent_id')=r.retry_intent_id
      AND json_extract(attempt.value,'$.attempt.submission_id')=r.predecessor_submission_id
      AND json_extract(attempt.value,'$.attempt.attempt_id')=r.predecessor_attempt_id
      AND json_type(receipt.value,'$.decision.predecessor.revision')='text'
      AND json_type(receipt.value,'$.decision.revision')='text'
      AND CAST(json_extract(receipt.value,'$.decision.predecessor.revision') AS INTEGER)>=0
      AND CAST(CAST(json_extract(receipt.value,'$.decision.predecessor.revision') AS INTEGER) AS TEXT)
          =json_extract(receipt.value,'$.decision.predecessor.revision')
      AND CAST(CAST(json_extract(receipt.value,'$.decision.revision') AS INTEGER) AS TEXT)
          =json_extract(receipt.value,'$.decision.revision')
      AND json_type(attempt.value,'$.revision')='integer'
      AND CAST(json_extract(receipt.value,'$.decision.predecessor.revision') AS INTEGER)+1
          =CAST(json_extract(receipt.value,'$.decision.revision') AS INTEGER)
      AND json_extract(attempt.value,'$.revision')>=CAST(json_extract(receipt.value,'$.decision.revision') AS INTEGER))
)
BEGIN
    SELECT RAISE(ABORT, 'Human retirement requires its exact linked retry receipt');
END;
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
CREATE INDEX conversation_presentations_human_order
ON conversation_presentations(card_id, sequence)
WHERE source_kind = 'human_message';
CREATE INDEX conversation_messages_card_author ON conversation_messages(card_id,author_kind);
CREATE TRIGGER conversation_guest_author_exists BEFORE INSERT ON conversation_messages
WHEN NEW.author_kind='link_guest' AND NOT EXISTS(SELECT 1 FROM share_links WHERE id=NEW.author_share_link_id)
BEGIN SELECT RAISE(ABORT,'LinkGuest requires an existing delegation'); END;
CREATE TRIGGER conversation_share_execution_exists BEFORE INSERT ON conversation_turns
WHEN NEW.execution_kind='share_delegation' AND NOT EXISTS (
 SELECT 1 FROM share_links WHERE id=json_extract(NEW.share_delegation_json,'$.id')
 AND approver_user_id=json_extract(NEW.share_delegation_json,'$.approverUserId')
 AND purpose=json_extract(NEW.share_delegation_json,'$.purpose')
 AND resource_policy=json_extract(NEW.share_delegation_json,'$.resourcePolicy')
 AND created_at=json_extract(NEW.share_delegation_json,'$.createdAt')
 AND ((track_id IS NOT NULL AND json_extract(NEW.share_delegation_json,'$.scope.kind')='track'
       AND track_id=json_extract(NEW.share_delegation_json,'$.scope.id'))
   OR (area_id IS NOT NULL AND json_extract(NEW.share_delegation_json,'$.scope.kind')='area'
       AND area_id=json_extract(NEW.share_delegation_json,'$.scope.id'))))
BEGIN SELECT RAISE(ABORT,'Share execution requires an existing delegation'); END;
CREATE TEMP TABLE share_guest_fk_assertion(ok INTEGER NOT NULL CHECK(ok=1));
INSERT INTO share_guest_fk_assertion SELECT 0 FROM (
 SELECT * FROM pragma_foreign_key_check EXCEPT SELECT * FROM share_guest_backup_fk);
DROP TABLE share_guest_fk_assertion;
DROP TABLE share_guest_backup_fk;
DROP TABLE share_guest_backup_sequence;
DROP TABLE share_guest_backup_conversation_presentations;
DROP TABLE share_guest_backup_conversation_submission_inputs;
DROP TABLE share_guest_backup_conversation_input_retry_relations;
DROP TABLE share_guest_backup_conversation_input_submissions;
DROP TABLE share_guest_backup_conversation_turns;
DROP TABLE share_guest_backup_conversation_message_revisions;
DROP TABLE share_guest_backup_conversation_messages;
