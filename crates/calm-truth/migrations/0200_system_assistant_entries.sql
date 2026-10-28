-- Immutable source/creation facts, never a dispatch queue. Keys survive resource deletion.
CREATE TABLE conversation_system_assistant_entries (
    card_id TEXT PRIMARY KEY,
    track_id TEXT NOT NULL,
    original_worker TEXT NOT NULL UNIQUE,
    source TEXT NOT NULL CHECK(source='today-summary'),
    creation_floor INTEGER NOT NULL CHECK(creation_floor>=0),
    bootstrap_digest TEXT NOT NULL CHECK(length(bootstrap_digest)=64)
);
CREATE TABLE conversation_system_assistant_receipts (
    card_id TEXT NOT NULL,
    source TEXT NOT NULL CHECK(source='today-summary'),
    subject TEXT NOT NULL,
    event_id INTEGER NOT NULL UNIQUE CHECK(event_id>0),
    observation_id TEXT NOT NULL UNIQUE,
    bootstrap INTEGER NOT NULL CHECK(bootstrap IN (0,1)),
    receipt_json TEXT NOT NULL CHECK(json_valid(receipt_json)),
    PRIMARY KEY(card_id,source,subject),
    FOREIGN KEY(card_id) REFERENCES conversation_system_assistant_entries(card_id)
);
CREATE UNIQUE INDEX system_assistant_one_bootstrap ON conversation_system_assistant_receipts(card_id) WHERE bootstrap=1;
CREATE TRIGGER system_assistant_entry_immutable BEFORE UPDATE ON conversation_system_assistant_entries BEGIN
    SELECT RAISE(ABORT,'System Assistant entry is immutable');
END;
CREATE TRIGGER system_assistant_entry_permanent BEFORE DELETE ON conversation_system_assistant_entries BEGIN
    SELECT RAISE(ABORT,'System Assistant creation key is exhausted');
END;
CREATE TRIGGER system_assistant_receipt_immutable BEFORE UPDATE ON conversation_system_assistant_receipts BEGIN
    SELECT RAISE(ABORT,'System Assistant receipt is immutable');
END;
CREATE TRIGGER system_assistant_receipt_permanent BEFORE DELETE ON conversation_system_assistant_receipts BEGIN
    SELECT RAISE(ABORT,'System Assistant input key is exhausted');
END;
-- REPLACE performs implicit deletes without DELETE triggers when recursive_triggers=OFF.
-- Fence every unique identity before SQLite can remove an original fact.
CREATE TRIGGER system_assistant_entry_no_replace BEFORE INSERT ON conversation_system_assistant_entries
WHEN EXISTS (SELECT 1 FROM conversation_system_assistant_entries
    WHERE card_id=NEW.card_id OR original_worker=NEW.original_worker)
BEGIN SELECT RAISE(ABORT,'System Assistant creation key is exhausted'); END;
CREATE TRIGGER system_assistant_receipt_no_replace BEFORE INSERT ON conversation_system_assistant_receipts
WHEN EXISTS (SELECT 1 FROM conversation_system_assistant_receipts
    WHERE (card_id=NEW.card_id AND source=NEW.source AND subject=NEW.subject)
       OR event_id=NEW.event_id OR observation_id=NEW.observation_id
       OR (NEW.bootstrap=1 AND bootstrap=1 AND card_id=NEW.card_id))
BEGIN SELECT RAISE(ABORT,'System Assistant input key is exhausted'); END;
CREATE TRIGGER system_assistant_entry_scope BEFORE INSERT ON conversation_system_assistant_entries
WHEN NOT EXISTS (
    SELECT 1 FROM cards c JOIN worker_sessions ws ON ws.id=c.session_id AND ws.card_id=c.id AND ws.track_id=c.track_id
    WHERE c.id=NEW.card_id AND c.track_id=NEW.track_id AND c.role='assistant'
      AND ws.id=NEW.original_worker AND ws.contract='assistant' AND ws.provider='codex' AND ws.mode='resumable' AND ws.state='idle'
      AND json_extract(c.payload,'$.harness_profile')='assistant' AND json_extract(c.payload,'$.assistant_provider')='codex'
      AND NOT EXISTS (SELECT 1 FROM conversation_assistant_creations h WHERE h.card_id=c.id)
      AND NOT EXISTS (SELECT 1 FROM conversation_presentations p WHERE p.card_id=c.id)
      AND ws.active_turn_id IS NULL AND ws.mcp_token_hash IS NULL
      AND ws.thread_id IS NULL AND ws.agent_session_id IS NULL AND ws.spawn_op_id IS NULL
      AND json_extract(ws.handle_state_json,'$.schema_version')=5
      AND json_extract(ws.handle_state_json,'$.push_watermark')=NEW.creation_floor
      AND NEW.creation_floor=COALESCE((SELECT seq FROM sqlite_sequence WHERE name='events'),0)
      AND json_extract(ws.handle_state_json,'$.mode')='harness'
      AND json_extract(ws.handle_state_json,'$.phase')='idle'
      AND json_array_length(ws.handle_state_json,'$.canonical_inputs.pending')=0
      AND json_extract(ws.handle_state_json,'$.canonical_inputs.next_queue_position')=0
      AND json_type(ws.handle_state_json,'$.canonical_inputs.active')='null'
      AND json_array_length(ws.handle_state_json,'$.canonical_inputs.attempts')=0
      AND json_array_length(ws.handle_state_json,'$.canonical_inputs.receipts')=0
)
BEGIN SELECT RAISE(ABORT,'System Assistant entry must bind its empty original carrier'); END;
CREATE TRIGGER system_assistant_receipt_scope BEFORE INSERT ON conversation_system_assistant_receipts
WHEN NOT EXISTS (
    SELECT 1 FROM conversation_system_assistant_entries e
    JOIN cards c ON c.id=e.card_id AND c.track_id=e.track_id AND c.session_id=e.original_worker
    JOIN tracks t ON t.id=c.track_id
    JOIN worker_sessions ws ON ws.id=e.original_worker AND ws.card_id=c.id AND ws.track_id=t.id
    JOIN conversation_presentations p ON p.card_id=e.card_id AND p.source_kind='system_observation' AND p.observation_id=NEW.observation_id AND p.erased_by_operation_id IS NULL
    JOIN json_each(ws.handle_state_json,'$.canonical_inputs.pending') pending
    JOIN events v ON v.id=NEW.event_id AND v.kind='assistant.input_requested' AND v.scope_kind='card'
    WHERE c.role='assistant' AND ws.contract='assistant' AND ws.provider='codex' AND ws.mode='resumable'
      AND ws.state IN ('starting','running','idle','turn_pending')
      AND json_extract(c.payload,'$.harness_profile')='assistant'
      AND json_extract(c.payload,'$.assistant_provider')='codex'
      AND e.card_id=NEW.card_id AND e.source=NEW.source AND v.scope_card=e.card_id AND v.scope_track=e.track_id
      AND json_extract(v.actor,'$.kind') IN ('Kernel','KernelDispatcher')
      AND json_extract(v.payload,'$.card_id')=e.card_id AND json_extract(v.payload,'$.track_id')=e.track_id
      AND v.scope_area=t.area_id
      AND json_extract(v.payload,'$.source')=e.source
      AND json_extract(v.payload,'$.original_worker')=e.original_worker
      AND json_extract(v.payload,'$.subject')=NEW.subject AND json_extract(v.payload,'$.version')=1
      AND v.id>e.creation_floor
      AND json_extract(NEW.receipt_json,'$.version')='system-assistant-v1'
      AND json_extract(NEW.receipt_json,'$.cardId')=e.card_id AND json_extract(NEW.receipt_json,'$.trackId')=e.track_id
      AND json_extract(NEW.receipt_json,'$.originalWorker')=e.original_worker
      AND json_extract(NEW.receipt_json,'$.eventId')=v.id AND json_extract(NEW.receipt_json,'$.observationId')=NEW.observation_id
      AND json_extract(NEW.receipt_json,'$.subject')=NEW.subject AND length(NEW.subject)>0
      AND json_extract(NEW.receipt_json,'$.source')=e.source
      AND json_extract(NEW.receipt_json,'$.presentationSequence')=p.sequence
      AND length(json_extract(NEW.receipt_json,'$.payloadDigest'))=64
      AND length(json_extract(NEW.receipt_json,'$.eventFingerprint'))=64
      AND json_extract(pending.value,'$.kind')='system'
      AND json_extract(pending.value,'$.observation_id')=NEW.observation_id
      AND json_extract(pending.value,'$.envelope_id')=NEW.event_id
      AND json_extract(pending.value,'$.queued_at_ms')=json_extract(NEW.receipt_json,'$.queuedAtMs')
      AND ((NEW.bootstrap=1 AND NEW.subject='bootstrap'
          AND json_extract(NEW.receipt_json,'$.acceptance.kind')='bootstrap'
          AND json_extract(NEW.receipt_json,'$.acceptance.creation_floor')=e.creation_floor
          AND json_extract(NEW.receipt_json,'$.payloadDigest')=e.bootstrap_digest)
        OR (NEW.bootstrap=0 AND NEW.subject!='bootstrap'
          AND json_extract(NEW.receipt_json,'$.acceptance.kind')='input'))
)
BEGIN SELECT RAISE(ABORT,'System Assistant receipt must bind its original targeted event'); END;

-- Both existing and new creation owners consume the same permanent card namespace.
CREATE TRIGGER assistant_creation_system_namespace BEFORE INSERT ON conversation_assistant_creations
WHEN EXISTS (SELECT 1 FROM conversation_system_assistant_entries WHERE card_id=NEW.card_id)
BEGIN SELECT RAISE(ABORT,'Assistant creation key is exhausted'); END;
