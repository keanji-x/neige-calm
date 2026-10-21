-- #2836: reference-capable immutable plans and exact Human retirement.
-- The live creation fence uses strict V5; the retry arm below consumes tagged
-- resolution receipts when the remaining stages in this same PR activate it.
-- Sqlx runs this forward migration in its transaction with foreign_keys ON.
-- Stage every member before DROP: cascades/SET NULL must never eat live data.
PRAGMA defer_foreign_keys=ON;
-- The rebuild must introduce no FK violations. Do not turn this migration into
-- a validator of unrelated legacy rows; their owning migrations retain that job.
CREATE TEMP TABLE retry_backup_fk_violations AS SELECT * FROM pragma_foreign_key_check;
DROP TRIGGER conversation_human_track_creation_fact_scope;
CREATE TEMP TABLE retry_backup_conversation_turns AS SELECT * FROM conversation_turns;
CREATE TEMP TABLE retry_backup_conversation_input_submissions AS SELECT * FROM conversation_input_submissions;
CREATE TEMP TABLE retry_backup_conversation_submission_inputs AS SELECT * FROM conversation_submission_inputs;
CREATE TEMP TABLE retry_backup_conversation_presentations AS SELECT * FROM conversation_presentations;
CREATE TEMP TABLE retry_backup_sequence AS SELECT seq FROM sqlite_sequence WHERE name='conversation_presentations';
DROP TABLE conversation_presentations;
DROP TABLE conversation_submission_inputs;
DROP TABLE conversation_input_submissions;
DROP TABLE conversation_turns;

CREATE TABLE conversation_turns (
    id TEXT PRIMARY KEY NOT NULL CHECK(length(id)=32 AND id NOT GLOB '*[^0-9a-f]*'),
    card_id TEXT NOT NULL REFERENCES cards(id) ON DELETE CASCADE,
    initiating_runtime_id TEXT NOT NULL,
    initial_submission_id TEXT NOT NULL UNIQUE,
    execution_kind TEXT NOT NULL CHECK(execution_kind='legacy_single_user_deployment'),
    deployment_owner_user_id TEXT NOT NULL REFERENCES application_users(id) ON DELETE RESTRICT,
    provider TEXT NOT NULL CHECK(provider IN ('codex','claude','opencode')),
    profile TEXT NOT NULL CHECK(profile IN ('spec','plain_chat','assistant')),
    created_at_ms INTEGER NOT NULL,
    UNIQUE(id,card_id),
    FOREIGN KEY(initial_submission_id,id,card_id)
      REFERENCES conversation_input_submissions(id,logical_turn_id,card_id)
      DEFERRABLE INITIALLY DEFERRED
);
CREATE TABLE conversation_input_submissions (
    id TEXT PRIMARY KEY NOT NULL CHECK(length(id)=32 AND id NOT GLOB '*[^0-9a-f]*'),
    card_id TEXT NOT NULL REFERENCES cards(id) ON DELETE CASCADE,
    logical_turn_id TEXT NOT NULL,
    ordinal INTEGER NOT NULL CHECK(ordinal=0),
    kind TEXT NOT NULL CHECK(kind='initial'),
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
    CHECK(kind='initial' AND ordinal=0)
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
    source_kind TEXT NOT NULL CHECK(source_kind IN ('human_message','system_observation','native_item')),
    source_identity TEXT NOT NULL CHECK(length(source_identity)>0),
    message_id TEXT,
    observation_id TEXT,
    native_item_row_id INTEGER REFERENCES harness_items(id) ON DELETE SET NULL,
    submission_id TEXT REFERENCES conversation_input_submissions(id) ON DELETE SET NULL,
    erased_by_operation_id TEXT,
    UNIQUE(card_id,source_kind,source_identity),
    FOREIGN KEY(message_id,card_id) REFERENCES conversation_messages(id,card_id) ON DELETE CASCADE,
    CHECK(erased_by_operation_id IS NULL OR (length(erased_by_operation_id)=32 AND erased_by_operation_id NOT GLOB '*[^0-9a-f]*')),
    CHECK(source_kind!='human_message' OR source_identity=message_id),
    CHECK(source_kind!='system_observation' OR (source_identity=observation_id AND length(observation_id)=32 AND observation_id NOT GLOB '*[^0-9a-f]*')),
    CHECK((source_kind='human_message' AND message_id IS NOT NULL AND observation_id IS NULL AND native_item_row_id IS NULL)
       OR (source_kind='system_observation' AND message_id IS NULL AND observation_id IS NOT NULL AND native_item_row_id IS NULL)
       OR (source_kind='native_item' AND message_id IS NULL AND observation_id IS NULL))
);

-- Retry intent identities deliberately name no submission or Turn. Each claim
-- creates fresh crossing facts while retaining the same intent. Deferred canonical
-- edges retain roots until card GC. Carrier identity, like Turn provenance,
-- survives runtime deletion; the insert guards below check its live scope.
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
INSERT INTO conversation_turns SELECT * FROM retry_backup_conversation_turns;
INSERT INTO conversation_input_submissions(id,card_id,logical_turn_id,ordinal,kind,render_plan_json,render_plan_sha256,native_correlation_json,created_at_ms)
    SELECT * FROM retry_backup_conversation_input_submissions;
INSERT INTO conversation_submission_inputs(submission_id,input_position,input_kind,message_id,revision,observation_id,queue_position,queued_at_ms,envelope_id,system_addressable,effective,released_by_attempt_id,retirement_reason)
    SELECT *,CASE WHEN effective=0 THEN 'not_dispatched' END FROM retry_backup_conversation_submission_inputs;
INSERT INTO conversation_presentations SELECT * FROM retry_backup_conversation_presentations;
DELETE FROM sqlite_sequence WHERE name='conversation_presentations' AND NOT EXISTS(SELECT 1 FROM retry_backup_sequence);
UPDATE sqlite_sequence SET seq=(SELECT seq FROM retry_backup_sequence) WHERE name='conversation_presentations' AND EXISTS(SELECT 1 FROM retry_backup_sequence);

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
-- The reverse edge matters when a caller defers all FKs and inserts the child
-- first. The composite deferred FK owns scope; these two insertion guards own
-- the Initial kind/id relation in either supported insertion order.
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

-- One resolution path for both typed proof reads and SQL System provenance.
-- Chains, mismatched cards/digests and missing roots never resolve silently.
CREATE VIEW conversation_resolved_submission_plans AS
SELECT s.id,s.card_id,s.render_plan_json,s.render_plan_sha256
FROM conversation_input_submissions s WHERE s.render_plan_root_submission_id IS NULL
UNION ALL
SELECT s.id,s.card_id,root.render_plan_json,s.render_plan_sha256
FROM conversation_input_submissions s
JOIN conversation_input_submissions root ON root.id=s.render_plan_root_submission_id
WHERE root.id!=s.id AND root.card_id=s.card_id
  AND root.render_plan_root_submission_id IS NULL AND root.render_plan_json IS NOT NULL
  AND root.render_plan_sha256=s.render_plan_sha256;

CREATE TRIGGER conversation_human_track_creation_fact_scope
BEFORE INSERT ON conversation_human_track_creations
WHEN NOT EXISTS (
    SELECT 1 FROM track_create_idempotency b
    JOIN tracks t ON t.id=b.track_id AND t.area_id=b.area_id
    JOIN cards c ON c.id=b.planner_card_id AND c.track_id=t.id AND c.role='planner'
    JOIN cards r ON r.id=b.report_card_id AND r.track_id=t.id AND r.role='reportcard'
    JOIN worker_sessions ws ON ws.id=c.session_id AND ws.card_id=c.id AND ws.track_id=t.id
    JOIN conversation_messages m ON m.id=NEW.message_id AND m.card_id=c.id
    JOIN conversation_message_revisions mr ON mr.message_id=m.id AND mr.revision=NEW.message_revision
    JOIN conversation_presentations p ON p.card_id=c.id AND p.source_kind='human_message' AND p.message_id=m.id
    WHERE b.area_id=NEW.area_id AND b.idempotency_key=NEW.idempotency_key
      AND b.request_fingerprint_version=1
      AND ws.id=NEW.origin_runtime_id AND ws.mode='resumable' AND ws.contract='planner' AND ws.state='idle'
      AND ws.thread_id IS NULL AND ws.agent_session_id IS NULL AND ws.active_turn_id IS NULL
      AND ws.mcp_token_hash IS NULL
      AND CASE WHEN json_valid(ws.handle_state_json) THEN
        json_extract(ws.handle_state_json,'$.schema_version')=5
        AND json_extract(ws.handle_state_json,'$.mode')='harness'
        AND json_extract(ws.handle_state_json,'$.phase')='idle'
        AND json_array_length(ws.handle_state_json,'$.canonical_inputs.pending')=1
        AND json_extract(ws.handle_state_json,'$.canonical_inputs.next_queue_position')=1
        AND json_type(ws.handle_state_json,'$.canonical_inputs.active')='null'
        AND json_array_length(ws.handle_state_json,'$.canonical_inputs.attempts')=0
        AND json_type(ws.handle_state_json,'$.canonical_inputs.receipts')='array'
        AND json_array_length(ws.handle_state_json,'$.canonical_inputs.receipts')=0
        AND json_extract(ws.handle_state_json,'$.canonical_inputs.pending[0].kind')='human'
        AND json_extract(ws.handle_state_json,'$.canonical_inputs.pending[0].message.message_id')=NEW.message_id
        AND json_extract(ws.handle_state_json,'$.canonical_inputs.pending[0].message.revision')=NEW.message_revision
      ELSE 0 END IS TRUE
      AND m.author_kind='authenticated_human' AND m.author_user_id=NEW.source_user_id AND m.current_revision=1
      AND m.withdrawn_at_ms IS NULL AND json_extract(mr.payload_json,'$.state')='present'
      AND p.sequence=NEW.presentation_sequence
      -- Preserve the original one-creation-per-resource relation and refuse
      -- INSERT OR REPLACE even when recursive DELETE triggers are disabled.
      AND NOT EXISTS (
          SELECT 1 FROM conversation_human_track_creations old
          JOIN track_create_idempotency original
            ON original.area_id=old.area_id AND original.idempotency_key=old.idempotency_key
          WHERE (old.area_id=NEW.area_id AND old.idempotency_key=NEW.idempotency_key)
             OR original.track_id=b.track_id OR original.planner_card_id=b.planner_card_id
             OR old.origin_runtime_id=NEW.origin_runtime_id OR old.message_id=NEW.message_id
      )
)
BEGIN
    SELECT RAISE(ABORT, 'Human creation acceptance must bind its actual dormant allocation and first input');
END;

CREATE TEMP TABLE retry_fk_assertion (valid INTEGER NOT NULL CHECK(valid=1));
INSERT INTO retry_fk_assertion SELECT 0 FROM (
    SELECT * FROM pragma_foreign_key_check
    EXCEPT SELECT * FROM retry_backup_fk_violations
);
DROP TABLE retry_fk_assertion;
DROP TABLE retry_backup_conversation_turns;
DROP TABLE retry_backup_conversation_input_submissions;
DROP TABLE retry_backup_conversation_submission_inputs;
DROP TABLE retry_backup_conversation_presentations;
DROP TABLE retry_backup_sequence;
DROP TABLE retry_backup_fk_violations;
