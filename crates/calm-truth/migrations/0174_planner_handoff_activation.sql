-- One explicit forward activation per committed, user-audited identity cutover.
-- This is resource evidence, never a global pending->active bypass.
CREATE TABLE planner_handoff_activations (
    cutover_operation_id TEXT PRIMARY KEY REFERENCES operations(id),
    activation_operation_id TEXT NOT NULL UNIQUE REFERENCES operations(id),
    session_id TEXT NOT NULL UNIQUE REFERENCES worker_sessions(id) ON DELETE CASCADE,
    retired_source_session_id TEXT NOT NULL UNIQUE,
    retired_target_session_id TEXT NOT NULL UNIQUE,
    thread_id TEXT NULL CHECK(thread_id IS NULL OR length(thread_id)>0),
    token_hash TEXT NULL,
    CHECK(token_hash IS NULL OR thread_id IS NOT NULL)
);
CREATE TRIGGER planner_handoff_activation_claim BEFORE INSERT ON planner_handoff_activations
WHEN NEW.thread_id IS NOT NULL OR NEW.token_hash IS NOT NULL OR NOT EXISTS (
    SELECT 1 FROM operations original JOIN operations activation
      ON activation.id=NEW.activation_operation_id
    JOIN worker_sessions s ON s.id=NEW.session_id
    JOIN cards c ON c.id=s.card_id JOIN tracks t ON t.id=c.track_id
    WHERE original.id=NEW.cutover_operation_id AND original.kind='handoff-cutover'
      AND original.phase='succeeded'
      AND json_extract(original.tx_output_json,'$.result.state')='cutover_committed_pending_activation'
      AND length(json_extract(original.payload_json,'$.scope.user_audit_id'))>0
      AND json_extract(original.tx_output_json,'$.result.operation_id')=original.id
      AND json_extract(original.tx_output_json,'$.result.retired_source_session_id')=NEW.retired_source_session_id
      AND json_extract(original.tx_output_json,'$.result.retired_target_session_id')=NEW.retired_target_session_id
      AND json_extract(original.tx_output_json,'$.result.planner_session_id')=s.id
      AND json_extract(original.tx_output_json,'$.result.planner_card_id')=c.id
      AND json_extract(original.tx_output_json,'$.result.plan_id')=json_extract(original.payload_json,'$.scope.plan_id')
      AND json_extract(activation.payload_json,'$.cutover_operation_id')=original.id
      AND activation.idempotency_key=original.id
      AND activation.kind='handoff-activation' AND activation.phase='pending'
      AND s.state='pending_activation' AND s.contract='planner'
      AND c.role='planner' AND c.session_id=s.id AND t.root_session_id=s.id
      AND t.closed_at IS NULL AND t.purpose IS NULL
      AND json_extract(c.payload,'$.handoff_pending_activation')=original.id
)
BEGIN SELECT RAISE(ABORT,'Activation requires original exact audited cutover'); END;
CREATE TRIGGER planner_handoff_activation_identity BEFORE UPDATE OF cutover_operation_id,activation_operation_id,session_id,retired_source_session_id,retired_target_session_id ON planner_handoff_activations
BEGIN SELECT RAISE(ABORT,'Activation identity is immutable'); END;
CREATE TRIGGER planner_handoff_activation_resource BEFORE UPDATE OF thread_id,token_hash ON planner_handoff_activations
WHEN OLD.thread_id IS NOT NULL
BEGIN SELECT RAISE(ABORT,'Activation resource result is write-once'); END;
CREATE TRIGGER planner_handoff_activation_permanent BEFORE DELETE ON planner_handoff_activations
WHEN EXISTS(SELECT 1 FROM worker_sessions WHERE id=OLD.session_id)
BEGIN SELECT RAISE(ABORT,'Activation evidence is permanent'); END;

DROP TRIGGER pending_activation_no_generic_start;
CREATE TRIGGER pending_activation_no_generic_start BEFORE UPDATE ON worker_sessions
WHEN OLD.state='pending_activation' AND
 (NEW.state != OLD.state OR NEW.thread_id IS NOT NULL OR NEW.agent_session_id IS NOT NULL
  OR NEW.mcp_token_hash IS NOT NULL OR NEW.active_turn_id IS NOT NULL OR NEW.terminal_run_id IS NOT NULL)
 AND NOT (
    NEW.state='starting' AND NEW.contract=OLD.contract AND NEW.provider=OLD.provider
    AND NEW.card_id IS OLD.card_id AND NEW.track_id=OLD.track_id AND NEW.id=OLD.id
    AND NEW.agent_session_id IS NULL AND NEW.active_turn_id IS NULL AND NEW.terminal_run_id IS NULL
    AND EXISTS (
      SELECT 1 FROM planner_handoff_activations a
      JOIN operations activation ON activation.id=a.activation_operation_id
      JOIN operations original ON original.id=a.cutover_operation_id
      JOIN cards c ON c.id=NEW.card_id JOIN tracks t ON t.id=c.track_id
      WHERE a.session_id=OLD.id AND a.thread_id=NEW.thread_id AND a.token_hash IS NEW.mcp_token_hash
        AND activation.kind='handoff-activation' AND activation.phase='app_server_interact'
        AND activation.lease_owner IS NOT NULL
        AND json_extract(activation.payload_json,'$.cutover_operation_id')=original.id
        AND json_extract(activation.tx_output_json,'$.result.state')='resources_checkpointed'
        AND json_extract(activation.tx_output_json,'$.data.runtime_id')=OLD.id
        AND json_extract(activation.tx_output_json,'$.data.codex_thread_id')=a.thread_id
        AND json_extract(activation.tx_output_json,'$.data.snapshot')=json(NEW.handle_state_json)
        AND original.kind='handoff-cutover' AND original.phase='succeeded'
        AND length(json_extract(original.payload_json,'$.scope.user_audit_id'))>0
        AND json_extract(original.tx_output_json,'$.result.planner_session_id')=OLD.id
        AND json_extract(original.tx_output_json,'$.result.planner_card_id')=c.id
        AND c.session_id=OLD.id AND c.role='planner' AND t.root_session_id=OLD.id
        AND t.closed_at IS NULL AND t.purpose IS NULL
        AND json_extract(c.payload,'$.handoff_pending_activation')=original.id
        AND activation.idempotency_key=original.id
        AND json_extract(activation.tx_output_json,'$.result.thread_id')=a.thread_id
        AND (NEW.provider!='codex' OR a.token_hash IS NOT NULL)
        AND json_remove(c.payload,'$.handoff_pending_activation')=json_extract(original.tx_output_json,'$.result.configuration.conversation.payload')
        AND t.template_id IS json_extract(original.tx_output_json,'$.result.configuration.template_id')
        AND json_extract(original.payload_json,'$.configuration')=json_extract(original.tx_output_json,'$.result.configuration')
        AND EXISTS (
          SELECT 1 FROM worker_sessions source, worker_sessions target
          WHERE source.id=json_extract(original.payload_json,'$.scope.source_session_id')
            AND target.id=json_extract(original.payload_json,'$.scope.target_session_id')
            AND source.state='superseded' AND target.state='superseded'
            AND source.mcp_token_hash IS NULL AND target.mcp_token_hash IS NULL
            AND json_extract(source.handle_state_json,'$.handoff_pause.state')='consumed_by_handoff'
            AND json_extract(target.handle_state_json,'$.handoff_pause.state')='consumed_by_handoff'
            AND json_extract(source.handle_state_json,'$.handoff_pause.operation_id')=original.id
            AND json_extract(target.handle_state_json,'$.handoff_pause.operation_id')=original.id
            AND json_extract(source.handle_state_json,'$.handoff_pause.plan_id')=json_extract(original.payload_json,'$.scope.plan_id')
            AND json_extract(target.handle_state_json,'$.handoff_pause.plan_id')=json_extract(original.payload_json,'$.scope.plan_id')
            AND json_extract(source.handle_state_json,'$.handoff_pause.new_session_id')=OLD.id
            AND json_extract(target.handle_state_json,'$.handoff_pause.new_session_id')=OLD.id
            AND json_extract(source.handle_state_json,'$.handoff_pause.receipt')=json_extract(original.payload_json,'$.scope.source_pause')
            AND json_extract(target.handle_state_json,'$.handoff_pause.receipt')=json_extract(original.payload_json,'$.scope.target_pause')
        )
    )
 )
BEGIN SELECT RAISE(ABORT,'Planner awaits explicit activation'); END;


-- Starting a successor never re-arms either retired identity, including the former
-- Assistant whose card is now a Planner. The original cutover evidence owns this fence.
CREATE TRIGGER planner_handoff_retired_session_no_regrant BEFORE UPDATE OF state,mcp_token_hash ON worker_sessions
WHEN (NEW.state IN ('starting','running','idle','turn_pending') OR NEW.mcp_token_hash IS NOT NULL)
 AND EXISTS(SELECT 1 FROM planner_handoff_activations a
   WHERE a.retired_source_session_id=OLD.id OR a.retired_target_session_id=OLD.id)
BEGIN SELECT RAISE(ABORT,'Handoff retired session requires forward recovery'); END;
CREATE TRIGGER planner_handoff_retired_session_no_current BEFORE UPDATE OF session_id ON cards
WHEN NEW.session_id IS NOT OLD.session_id
 AND EXISTS(SELECT 1 FROM planner_handoff_activations a
   WHERE a.retired_source_session_id=NEW.session_id OR a.retired_target_session_id=NEW.session_id)
BEGIN SELECT RAISE(ABORT,'Handoff retired session cannot become current'); END;
