-- LinkGuest creation keeps the original dormant-allocation proof and owner attribution.
-- Replay additionally checks the immutable message author at the ingress owner.

DROP TRIGGER assistant_creation_fact_scope;
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
      AND ((m.author_kind='authenticated_human' AND m.author_user_id=NEW.source_user_id)
        OR (m.author_kind='link_guest' AND EXISTS (
            SELECT 1 FROM share_links sl WHERE sl.id=m.author_share_link_id
              AND sl.approver_user_id=NEW.source_user_id
        )))
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

DROP TRIGGER conversation_human_track_creation_fact_scope;
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
      AND ((m.author_kind='authenticated_human' AND m.author_user_id=NEW.source_user_id)
        OR (m.author_kind='link_guest' AND EXISTS (
            SELECT 1 FROM share_links sl WHERE sl.id=m.author_share_link_id
              AND sl.approver_user_id=NEW.source_user_id
        ))) AND m.current_revision=1
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
