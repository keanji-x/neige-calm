-- #2671 C3: canonical facts only. V1 body adoption belongs to the stopped-writer
-- activation/recovery owner, never to opening a SQLite pool.
ALTER TABLE areas ADD COLUMN conversation_history_contract TEXT NOT NULL
    DEFAULT 'private' CHECK(conversation_history_contract IN ('private','shared_history'));
CREATE TRIGGER area_history_contract_cannot_downgrade
BEFORE UPDATE OF conversation_history_contract ON areas
WHEN OLD.conversation_history_contract = 'shared_history'
 AND NEW.conversation_history_contract != 'shared_history'
BEGIN
    SELECT RAISE(ABORT, 'Shared conversation history cannot become private');
END;

CREATE TABLE conversation_messages (
    id TEXT PRIMARY KEY NOT NULL CHECK(length(id)=32 AND id NOT GLOB '*[^0-9a-f]*'),
    card_id TEXT NOT NULL REFERENCES cards(id) ON DELETE CASCADE,
    current_revision INTEGER NOT NULL CHECK(current_revision > 0),
    author_kind TEXT NOT NULL CHECK(author_kind IN ('authenticated_human','legacy_input')),
    author_user_id TEXT REFERENCES application_users(id) ON DELETE RESTRICT,
    legacy_source_locator TEXT,
    legacy_boundary TEXT CHECK(legacy_boundary IN ('single_recorded_input','opaque_aggregate')),
    created_at_ms INTEGER NOT NULL,
    withdrawn_at_ms INTEGER,
    UNIQUE(id,card_id),
    CHECK((author_kind='authenticated_human' AND author_user_id IS NOT NULL
           AND legacy_source_locator IS NULL AND legacy_boundary IS NULL)
       OR (author_kind='legacy_input' AND author_user_id IS NULL
           AND legacy_source_locator IS NOT NULL AND length(legacy_source_locator)>0
           AND legacy_boundary IS NOT NULL)),
    FOREIGN KEY(id,current_revision)
      REFERENCES conversation_message_revisions(message_id,revision)
      DEFERRABLE INITIALLY DEFERRED
);
CREATE UNIQUE INDEX conversation_legacy_message_source
    ON conversation_messages(card_id,legacy_source_locator) WHERE author_kind='legacy_input';
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
CREATE TRIGGER conversation_message_identity_immutable
BEFORE UPDATE ON conversation_messages
WHEN NEW.id IS NOT OLD.id OR NEW.card_id IS NOT OLD.card_id
 OR NEW.author_kind IS NOT OLD.author_kind OR NEW.author_user_id IS NOT OLD.author_user_id
 OR NEW.legacy_source_locator IS NOT OLD.legacy_source_locator
 OR NEW.legacy_boundary IS NOT OLD.legacy_boundary OR NEW.created_at_ms IS NOT OLD.created_at_ms
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
    ordinal INTEGER NOT NULL CHECK(ordinal>=0),
    kind TEXT NOT NULL CHECK(kind IN ('initial','supplemental')),
    render_plan_json TEXT NOT NULL CHECK(json_valid(render_plan_json)),
    render_plan_sha256 TEXT NOT NULL CHECK(length(render_plan_sha256)=64 AND render_plan_sha256 NOT GLOB '*[^0-9a-f]*'),
    native_correlation_json TEXT CHECK(native_correlation_json IS NULL OR json_valid(native_correlation_json)),
    created_at_ms INTEGER NOT NULL,
    UNIQUE(logical_turn_id,ordinal),
    UNIQUE(id,logical_turn_id,card_id),
    FOREIGN KEY(logical_turn_id,card_id) REFERENCES conversation_turns(id,card_id) ON DELETE CASCADE,
    CHECK((kind='initial' AND ordinal=0) OR (kind='supplemental' AND ordinal>0))
);
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
 OR NEW.created_at_ms IS NOT OLD.created_at_ms
 OR (OLD.native_correlation_json IS NOT NULL
     AND NEW.native_correlation_json IS NOT OLD.native_correlation_json)
BEGIN
    SELECT RAISE(ABORT, 'Submission identity and original render digest are immutable');
END;
CREATE TRIGGER conversation_submission_only_private_erasure
BEFORE UPDATE OF render_plan_json ON conversation_input_submissions
WHEN NEW.render_plan_json IS NOT OLD.render_plan_json AND (
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

CREATE TABLE conversation_submission_inputs (
    submission_id TEXT NOT NULL REFERENCES conversation_input_submissions(id) ON DELETE CASCADE,
    input_position INTEGER NOT NULL CHECK(input_position>=0),
    input_kind TEXT NOT NULL CHECK(input_kind IN ('human','system','legacy')),
    message_id TEXT,
    revision INTEGER,
    observation_id TEXT CHECK(observation_id IS NULL OR (length(observation_id)=32 AND observation_id NOT GLOB '*[^0-9a-f]*')),
    queue_position INTEGER NOT NULL CHECK(queue_position>=0),
    queued_at_ms INTEGER NOT NULL,
    envelope_id INTEGER,
    system_addressable INTEGER CHECK(system_addressable IN (0,1)),
    effective INTEGER NOT NULL CHECK(effective IN (0,1)),
    released_by_attempt_id TEXT CHECK(released_by_attempt_id IS NULL OR (length(released_by_attempt_id)=32 AND released_by_attempt_id NOT GLOB '*[^0-9a-f]*')),
    PRIMARY KEY(submission_id,input_position),
    FOREIGN KEY(message_id,revision) REFERENCES conversation_message_revisions(message_id,revision),
    CHECK((input_kind IN ('human','legacy') AND message_id IS NOT NULL AND revision IS NOT NULL AND observation_id IS NULL AND system_addressable IS NULL)
       OR (input_kind='system' AND message_id IS NULL AND revision IS NULL AND observation_id IS NOT NULL AND system_addressable IS NOT NULL)),
    CHECK((effective=1 AND released_by_attempt_id IS NULL) OR (effective=0 AND released_by_attempt_id IS NOT NULL))
);
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
 OR OLD.effective!=1 OR NEW.effective!=0 OR NEW.released_by_attempt_id IS NULL
BEGIN
    SELECT RAISE(ABORT, 'Claims can only be released once by their exact attempt evidence');
END;

-- Ordering is independent of a native thread (the first accepted message has none).
-- Rows retain identity-only erasure tombstones so late provider echoes cannot revive bodies.
CREATE TABLE conversation_presentations (
    sequence INTEGER PRIMARY KEY AUTOINCREMENT,
    card_id TEXT NOT NULL REFERENCES cards(id) ON DELETE CASCADE,
    source_kind TEXT NOT NULL CHECK(source_kind IN ('human_message','system_observation','native_item','legacy')),
    source_identity TEXT NOT NULL CHECK(length(source_identity)>0),
    message_id TEXT,
    observation_id TEXT,
    native_item_row_id INTEGER REFERENCES harness_items(id) ON DELETE SET NULL,
    legacy_source_locator TEXT,
    legacy_segment INTEGER,
    submission_id TEXT REFERENCES conversation_input_submissions(id) ON DELETE SET NULL,
    erased_by_operation_id TEXT,
    UNIQUE(card_id,source_kind,source_identity),
    FOREIGN KEY(message_id,card_id) REFERENCES conversation_messages(id,card_id) ON DELETE CASCADE,
    CHECK(erased_by_operation_id IS NULL OR (length(erased_by_operation_id)=32 AND erased_by_operation_id NOT GLOB '*[^0-9a-f]*')),
    CHECK(source_kind!='human_message' OR source_identity=message_id),
    CHECK(source_kind!='system_observation' OR (source_identity=observation_id AND length(observation_id)=32 AND observation_id NOT GLOB '*[^0-9a-f]*')),
    CHECK((source_kind='human_message' AND message_id IS NOT NULL AND observation_id IS NULL AND native_item_row_id IS NULL AND legacy_source_locator IS NULL AND legacy_segment IS NULL)
       OR (source_kind='system_observation' AND message_id IS NULL AND observation_id IS NOT NULL AND native_item_row_id IS NULL AND legacy_source_locator IS NULL AND legacy_segment IS NULL)
       OR (source_kind='native_item' AND message_id IS NULL AND observation_id IS NULL AND legacy_source_locator IS NULL AND legacy_segment IS NULL)
       OR (source_kind='legacy' AND message_id IS NULL AND observation_id IS NULL AND native_item_row_id IS NULL AND legacy_source_locator IS NOT NULL AND legacy_segment IS NOT NULL AND legacy_segment>=0))
);
CREATE TABLE conversation_migration_aliases (
    card_id TEXT NOT NULL REFERENCES cards(id) ON DELETE CASCADE,
    source_locator TEXT NOT NULL CHECK(length(source_locator)>0),
    accepted_ref_json TEXT NOT NULL CHECK(json_valid(accepted_ref_json)),
    PRIMARY KEY(card_id,source_locator)
);

-- Human first-input acceptance extends the permanent original create binding.
-- Resource identities and raw fingerprints have one source, not a copied ACK.
CREATE TABLE conversation_human_track_creations (
    area_id TEXT NOT NULL,
    idempotency_key TEXT NOT NULL CHECK(length(idempotency_key)>0 AND length(idempotency_key)<=128),
    source_user_id TEXT NOT NULL REFERENCES application_users(id) ON DELETE RESTRICT,
    ack_version INTEGER NOT NULL CHECK(ack_version=1),
    origin_runtime_id TEXT NOT NULL CHECK(length(origin_runtime_id)=32 AND origin_runtime_id NOT GLOB '*[^0-9a-f]*'),
    message_id TEXT NOT NULL CHECK(length(message_id)=32 AND message_id NOT GLOB '*[^0-9a-f]*'),
    message_revision INTEGER NOT NULL CHECK(message_revision=1),
    presentation_sequence INTEGER NOT NULL CHECK(presentation_sequence>0),
    PRIMARY KEY(area_id,idempotency_key),
    UNIQUE(origin_runtime_id),
    UNIQUE(message_id),
    -- No reference to deletable Track/Card/runtime/revision rows. The original
    -- key and ACK survive their deletion. RESTRICT also rejects REPLACE of the
    -- parent binding, independently of recursive DELETE-trigger configuration.
    FOREIGN KEY(area_id,idempotency_key)
        REFERENCES track_create_idempotency(area_id,idempotency_key)
        ON UPDATE RESTRICT ON DELETE RESTRICT
);
CREATE TRIGGER canonical_track_creation_binding_immutable
BEFORE UPDATE ON track_create_idempotency
WHEN EXISTS (
    SELECT 1 FROM conversation_human_track_creations c
    WHERE (c.area_id=OLD.area_id AND c.idempotency_key=OLD.idempotency_key)
       OR (c.area_id=NEW.area_id AND c.idempotency_key=NEW.idempotency_key)
)
BEGIN
    SELECT RAISE(ABORT, 'Canonical creation binding is immutable');
END;
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
        json_extract(ws.handle_state_json,'$.schema_version')=2
        AND json_extract(ws.handle_state_json,'$.mode')='harness'
        AND json_extract(ws.handle_state_json,'$.phase')='idle'
        AND json_array_length(ws.handle_state_json,'$.canonical_inputs.pending')=1
        AND json_extract(ws.handle_state_json,'$.canonical_inputs.next_queue_position')=1
        AND json_type(ws.handle_state_json,'$.canonical_inputs.active')='null'
        AND json_array_length(ws.handle_state_json,'$.canonical_inputs.attempts')=0
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
CREATE TRIGGER conversation_human_track_creation_immutable
BEFORE UPDATE ON conversation_human_track_creations
BEGIN
    SELECT RAISE(ABORT, 'Original Human creation acceptance is immutable');
END;
CREATE TRIGGER conversation_human_track_creation_permanent
BEFORE DELETE ON conversation_human_track_creations
BEGIN
    SELECT RAISE(ABORT, 'Original Human creation keys cannot be reused');
END;
