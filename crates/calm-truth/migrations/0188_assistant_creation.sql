-- Assistant entry receipts are independent of the Planner-only Track allocation contract.
-- Keep the original key/ACK after deletion, so it can never authorize another acceptance.
CREATE TABLE conversation_assistant_creations (
    card_id TEXT PRIMARY KEY,
    track_id TEXT NOT NULL,
    request_sha256 TEXT NOT NULL CHECK(length(request_sha256)=64),
    source_user_id TEXT NOT NULL REFERENCES application_users(id) ON DELETE RESTRICT,
    origin_runtime_id TEXT NOT NULL UNIQUE,
    message_id TEXT NOT NULL UNIQUE,
    presentation_sequence INTEGER NOT NULL CHECK(presentation_sequence>0),
    receipt_json TEXT NOT NULL CHECK(json_valid(receipt_json))
);
CREATE TRIGGER assistant_creation_fact_scope
BEFORE INSERT ON conversation_assistant_creations
WHEN NOT EXISTS (
    SELECT 1 FROM cards c
    JOIN worker_sessions ws ON ws.id=c.session_id AND ws.card_id=c.id AND ws.track_id=c.track_id
    JOIN conversation_messages m ON m.id=NEW.message_id AND m.card_id=c.id
    JOIN conversation_message_revisions mr ON mr.message_id=m.id AND mr.revision=1
    JOIN conversation_presentations p ON p.card_id=c.id AND p.message_id=m.id AND p.source_kind='human_message'
    WHERE c.id=NEW.card_id AND c.track_id=NEW.track_id AND c.role='assistant'
      AND json_extract(c.payload,'$.harness_profile')='assistant'
      AND ws.provider=json_extract(c.payload,'$.assistant_provider')
      AND ws.id=NEW.origin_runtime_id AND ws.mode='resumable' AND ws.contract='assistant' AND ws.state='idle'
      AND ws.thread_id IS NULL AND ws.agent_session_id IS NULL AND ws.active_turn_id IS NULL
      AND ws.spawn_op_id IS NULL AND ws.mcp_token_hash IS NULL
      AND json_extract(ws.handle_state_json,'$.schema_version')=5
      AND json_extract(ws.handle_state_json,'$.mode')='harness'
      AND json_extract(ws.handle_state_json,'$.phase')='idle'
      AND json_array_length(ws.handle_state_json,'$.canonical_inputs.pending')=1
      AND json_extract(ws.handle_state_json,'$.canonical_inputs.next_queue_position')=1
      AND json_extract(ws.handle_state_json,'$.canonical_inputs.pending[0].kind')='human'
      AND json_extract(ws.handle_state_json,'$.canonical_inputs.pending[0].message.message_id')=NEW.message_id
      AND json_extract(ws.handle_state_json,'$.canonical_inputs.pending[0].message.revision')=1
      AND json_type(ws.handle_state_json,'$.canonical_inputs.active')='null'
      AND json_array_length(ws.handle_state_json,'$.canonical_inputs.attempts')=0
      AND json_array_length(ws.handle_state_json,'$.canonical_inputs.receipts')=0
      AND m.author_kind='authenticated_human' AND m.author_user_id=NEW.source_user_id
      AND m.current_revision=1 AND m.withdrawn_at_ms IS NULL
      AND json_extract(mr.payload_json,'$.state')='present'
      AND p.sequence=NEW.presentation_sequence
      AND json_extract(NEW.receipt_json,'$.version')='initial-input-v1'
      AND json_extract(NEW.receipt_json,'$.entryId')=NEW.message_id
      AND json_extract(NEW.receipt_json,'$.entryRev')=1
      AND json_extract(NEW.receipt_json,'$.initialRuntimeId')=ws.id
      AND json_extract(NEW.receipt_json,'$.queuedAtMs')=m.created_at_ms
      AND NOT EXISTS (SELECT 1 FROM conversation_assistant_creations old
          WHERE old.card_id=NEW.card_id OR old.origin_runtime_id=NEW.origin_runtime_id OR old.message_id=NEW.message_id)
)
BEGIN
    SELECT RAISE(ABORT, 'Assistant creation must bind its dormant carrier and original acceptance');
END;
CREATE TRIGGER assistant_creation_immutable BEFORE UPDATE ON conversation_assistant_creations
BEGIN
    SELECT RAISE(ABORT, 'Assistant original acceptance is immutable');
END;
CREATE TRIGGER assistant_creation_permanent BEFORE DELETE ON conversation_assistant_creations
BEGIN
    SELECT RAISE(ABORT, 'Assistant creation keys cannot be reused');
END;
