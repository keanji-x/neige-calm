-- Version two relates resource evidence directly to the original domain owner.
CREATE TABLE planner_handoff_resources (
    operation_id TEXT PRIMARY KEY REFERENCES operations(id),
    session_id TEXT NOT NULL UNIQUE REFERENCES worker_sessions(id) ON DELETE CASCADE,
    retired_source_session_id TEXT NOT NULL UNIQUE,
    retired_target_session_id TEXT NOT NULL UNIQUE,
    thread_id TEXT NULL CHECK(thread_id IS NULL OR length(thread_id)>0),
    token_hash TEXT NULL,
    CHECK(token_hash IS NULL OR thread_id IS NOT NULL)
);
CREATE TRIGGER planner_handoff_owner_claim BEFORE INSERT ON planner_handoff_resources
WHEN (NEW.thread_id IS NOT NULL OR NEW.token_hash IS NOT NULL OR NOT EXISTS (
    SELECT 1 FROM operations o JOIN planner_handoff_coordinators r ON r.operation_id=o.id
    JOIN worker_sessions s ON s.id=NEW.session_id JOIN cards c ON c.id=s.card_id JOIN tracks t ON t.id=c.track_id
    WHERE o.id=NEW.operation_id AND o.kind='handoff-workflow' AND o.phase='app_server_interact'
      AND o.lease_owner IS NOT NULL AND r.active=1
      AND json_extract(o.payload_json,'$.version')=2
      AND json_extract(o.tx_output_json,'$.result.state')='committed'
      AND json_extract(o.tx_output_json,'$.result.resources.state')='unprepared'
      AND json_extract(o.payload_json,'$.new_session_id')=s.id
      AND json_extract(o.payload_json,'$.request.source_session_id')=NEW.retired_source_session_id
      AND json_extract(o.payload_json,'$.request.target_session_id')=NEW.retired_target_session_id
      AND s.state='pending_activation' AND c.role='planner' AND c.session_id=s.id AND t.root_session_id=s.id
      AND json_extract(c.payload,'$.handoff_pending_activation')=coalesce(json_extract(o.tx_output_json,'$.result.identity.origin.cutover_operation_id'),o.id)
)) AND NOT EXISTS (
    SELECT 1 FROM planner_handoff_adoptions adoption JOIN operations owner ON owner.id=adoption.owner_operation_id
    JOIN planner_handoff_activations proof ON proof.cutover_operation_id=adoption.legacy_operation_id
    WHERE owner.id=NEW.operation_id AND owner.kind='handoff-workflow' AND json_extract(owner.payload_json,'$.version')=2
      AND proof.session_id=NEW.session_id AND proof.retired_source_session_id=NEW.retired_source_session_id
      AND proof.retired_target_session_id=NEW.retired_target_session_id AND proof.thread_id IS NEW.thread_id
      AND proof.token_hash IS NEW.token_hash
)
BEGIN SELECT RAISE(ABORT,'Workflow resources require exact committed owner'); END;
CREATE TRIGGER planner_handoff_owner_identity BEFORE UPDATE OF operation_id,session_id,retired_source_session_id,retired_target_session_id ON planner_handoff_resources
BEGIN SELECT RAISE(ABORT,'Workflow resource identity is immutable'); END;
CREATE TRIGGER planner_handoff_owner_resource BEFORE UPDATE OF thread_id,token_hash ON planner_handoff_resources
WHEN OLD.thread_id IS NOT NULL
BEGIN SELECT RAISE(ABORT,'Workflow resource result is write-once'); END;
CREATE TRIGGER planner_handoff_owner_permanent BEFORE DELETE ON planner_handoff_resources
WHEN EXISTS(SELECT 1 FROM worker_sessions WHERE id=OLD.session_id)
BEGIN SELECT RAISE(ABORT,'Workflow evidence is permanent'); END;
CREATE TRIGGER planner_handoff_owner_retired_no_regrant BEFORE UPDATE OF state,mcp_token_hash ON worker_sessions
WHEN (NEW.state IN ('starting','running','idle','turn_pending') OR NEW.mcp_token_hash IS NOT NULL)
 AND (EXISTS(SELECT 1 FROM planner_handoff_resources a WHERE a.retired_source_session_id=OLD.id OR a.retired_target_session_id=OLD.id) OR EXISTS(SELECT 1 FROM operations o WHERE
    (o.kind='handoff-workflow' AND json_extract(o.tx_output_json,'$.result.state') IN ('committed','activated')
      AND (json_extract(o.payload_json,'$.request.source_session_id')=OLD.id OR json_extract(o.payload_json,'$.request.target_session_id')=OLD.id))
    OR (o.kind='handoff-cutover' AND json_extract(o.tx_output_json,'$.result.state')='cutover_committed_pending_activation'
      AND (json_extract(o.payload_json,'$.scope.source_session_id')=OLD.id OR json_extract(o.payload_json,'$.scope.target_session_id')=OLD.id))))
BEGIN SELECT RAISE(ABORT,'Handoff retired session requires forward recovery'); END;
CREATE TRIGGER planner_handoff_owner_retired_no_current BEFORE UPDATE OF session_id ON cards
WHEN NEW.session_id IS NOT OLD.session_id
 AND (EXISTS(SELECT 1 FROM planner_handoff_resources a WHERE a.retired_source_session_id=NEW.session_id OR a.retired_target_session_id=NEW.session_id) OR EXISTS(SELECT 1 FROM operations o WHERE
    (o.kind='handoff-workflow' AND json_extract(o.tx_output_json,'$.result.state') IN ('committed','activated')
      AND (json_extract(o.payload_json,'$.request.source_session_id')=NEW.session_id OR json_extract(o.payload_json,'$.request.target_session_id')=NEW.session_id))
    OR (o.kind='handoff-cutover' AND json_extract(o.tx_output_json,'$.result.state')='cutover_committed_pending_activation'
      AND (json_extract(o.payload_json,'$.scope.source_session_id')=NEW.session_id OR json_extract(o.payload_json,'$.scope.target_session_id')=NEW.session_id))))
BEGIN SELECT RAISE(ABORT,'Handoff retired session cannot become current'); END;

DROP TRIGGER pending_activation_no_generic_start;
CREATE TRIGGER pending_activation_no_generic_start BEFORE UPDATE ON worker_sessions
WHEN OLD.state='pending_activation' AND
 (NEW.state != OLD.state OR NEW.thread_id IS NOT NULL OR NEW.agent_session_id IS NOT NULL
  OR NEW.mcp_token_hash IS NOT NULL OR NEW.active_turn_id IS NOT NULL OR NEW.terminal_run_id IS NOT NULL)
 AND NOT (
EXISTS (
      SELECT 1 FROM planner_handoff_resources a JOIN operations o ON o.id=a.operation_id
      JOIN cards c ON c.id=NEW.card_id JOIN tracks t ON t.id=c.track_id
      WHERE NEW.state='starting' AND NEW.contract=OLD.contract AND NEW.provider=OLD.provider
        AND NEW.card_id IS OLD.card_id AND NEW.track_id=OLD.track_id AND NEW.id=OLD.id
        AND NEW.agent_session_id IS NULL AND NEW.active_turn_id IS NULL AND NEW.terminal_run_id IS NULL
        AND a.session_id=OLD.id AND a.thread_id=NEW.thread_id AND a.token_hash IS NEW.mcp_token_hash
        AND o.kind='handoff-workflow' AND o.phase='app_server_interact' AND o.lease_owner IS NOT NULL
        AND json_extract(o.payload_json,'$.version')=2
        AND json_extract(o.tx_output_json,'$.result.state')='committed'
        AND json_extract(o.tx_output_json,'$.result.resources.state')='proven'
        AND json_extract(o.payload_json,'$.new_session_id')=OLD.id
        AND json_extract(o.tx_output_json,'$.data.codex_thread_id')=a.thread_id
        AND json_extract(o.tx_output_json,'$.data.snapshot')=json(NEW.handle_state_json)
        AND c.session_id=OLD.id AND c.role='planner' AND t.root_session_id=OLD.id
        AND t.closed_at IS NULL AND t.purpose IS NULL
        AND json_extract(c.payload,'$.handoff_pending_activation')=coalesce(json_extract(o.tx_output_json,'$.result.identity.origin.cutover_operation_id'),o.id)
        AND (NEW.provider!='codex' OR a.token_hash IS NOT NULL)
        AND json_remove(c.payload,'$.handoff_pending_activation')=json_extract(o.tx_output_json,'$.result.identity.configuration.conversation.payload')
        AND t.template_id IS json_extract(o.tx_output_json,'$.result.identity.configuration.template_id')
        AND EXISTS (
          SELECT 1 FROM worker_sessions source,worker_sessions target
          WHERE source.id=a.retired_source_session_id AND target.id=a.retired_target_session_id
            AND source.state='superseded' AND target.state='superseded'
            AND source.mcp_token_hash IS NULL AND target.mcp_token_hash IS NULL
            AND json_extract(source.handle_state_json,'$.handoff_pause.state')='consumed_by_handoff'
            AND json_extract(target.handle_state_json,'$.handoff_pause.state')='consumed_by_handoff'
            AND json_extract(source.handle_state_json,'$.handoff_pause.operation_id')=coalesce(json_extract(o.tx_output_json,'$.result.identity.origin.cutover_operation_id'),o.id)
            AND json_extract(target.handle_state_json,'$.handoff_pause.operation_id')=coalesce(json_extract(o.tx_output_json,'$.result.identity.origin.cutover_operation_id'),o.id)
            AND json_extract(source.handle_state_json,'$.handoff_pause.plan_id')=json_extract(o.payload_json,'$.plan_id')
            AND json_extract(target.handle_state_json,'$.handoff_pause.plan_id')=json_extract(o.payload_json,'$.plan_id')
            AND json_extract(source.handle_state_json,'$.handoff_pause.new_session_id')=OLD.id
            AND json_extract(target.handle_state_json,'$.handoff_pause.new_session_id')=OLD.id
            AND json_extract(source.handle_state_json,'$.handoff_pause.receipt')=json_extract(o.tx_output_json,'$.result.identity.source_pause')
            AND json_extract(target.handle_state_json,'$.handoff_pause.receipt')=json_extract(o.tx_output_json,'$.result.identity.target_pause')
        )
    )
 )
BEGIN SELECT RAISE(ABORT,'Planner awaits explicit activation'); END;

CREATE TABLE planner_handoff_adoptions (
    legacy_operation_id TEXT PRIMARY KEY REFERENCES operations(id),
    owner_operation_id TEXT NOT NULL REFERENCES operations(id),
    legacy_attempt INTEGER NOT NULL CHECK(legacy_attempt>=0)
);
CREATE TRIGGER planner_handoff_adoption_immutable BEFORE UPDATE ON planner_handoff_adoptions
BEGIN SELECT RAISE(ABORT,'Handoff adoption is write-once'); END;
CREATE TRIGGER planner_handoff_adoption_permanent BEFORE DELETE ON planner_handoff_adoptions
BEGIN SELECT RAISE(ABORT,'Handoff adoption is permanent'); END;
CREATE TRIGGER planner_handoff_adopted_driver_fence BEFORE UPDATE ON operations
WHEN EXISTS(SELECT 1 FROM planner_handoff_adoptions WHERE legacy_operation_id=OLD.id)
BEGIN SELECT RAISE(ABORT,'Legacy handoff has a single recovery owner'); END;
-- Old nonterminal journals are recovery evidence, never background work after upgrade.
UPDATE operations SET phase='stuck',phase_detail_json=json_object('from_phase',phase,'reason','Legacy handoff requires explicit adoption','since',0),
    lease_owner=NULL,lease_until_ms=NULL,last_error='Legacy handoff requires explicit adoption'
WHERE kind IN ('handoff-coordinator','handoff-cutover','handoff-activation')
    AND phase NOT IN ('succeeded','failed','stuck');
DROP TRIGGER planner_handoff_coordinator_identity;
ALTER TABLE planner_handoff_coordinators DROP COLUMN request_hash;
CREATE TRIGGER planner_handoff_coordinator_identity
BEFORE UPDATE OF operation_id,track_id ON planner_handoff_coordinators
WHEN NEW.track_id IS NOT OLD.track_id OR NOT EXISTS (
    SELECT 1 FROM planner_handoff_adoptions a JOIN operations o ON o.id=a.owner_operation_id
    WHERE a.legacy_operation_id=OLD.operation_id AND a.owner_operation_id=NEW.operation_id
      AND o.kind='handoff-workflow' AND json_extract(o.payload_json,'$.version')=2
      AND json_extract(o.payload_json,'$.request.track_id')=OLD.track_id
)
BEGIN SELECT RAISE(ABORT,'Handoff reservation identity is immutable'); END;

CREATE TRIGGER planner_handoff_owner_terminal_absorbing BEFORE UPDATE OF tx_output_json ON operations
WHEN OLD.kind='handoff-workflow'
 AND json_extract(OLD.tx_output_json,'$.result.state') IN ('activated','finished')
 AND json_extract(NEW.tx_output_json,'$.result') IS NOT json_extract(OLD.tx_output_json,'$.result')
BEGIN SELECT RAISE(ABORT,'Handoff business terminal is absorbing'); END;
CREATE TRIGGER planner_handoff_owner_terminal_release BEFORE UPDATE OF active ON planner_handoff_coordinators
WHEN OLD.active=1 AND NEW.active=0 AND EXISTS (
    SELECT 1 FROM operations o WHERE o.id=NEW.operation_id AND o.kind='handoff-workflow'
      AND coalesce(json_extract(o.tx_output_json,'$.result.state'),'') NOT IN ('activated','finished')
)
BEGIN SELECT RAISE(ABORT,'Workflow reservation requires committed business terminal'); END;

CREATE TRIGGER planner_handoff_owner_resource_intent BEFORE UPDATE OF thread_id,token_hash ON planner_handoff_resources
WHEN OLD.thread_id IS NULL AND NOT EXISTS (
    SELECT 1 FROM operations o WHERE o.id=OLD.operation_id AND o.kind='handoff-workflow'
      AND o.phase='app_server_interact' AND o.lease_owner IS NOT NULL
      AND json_extract(o.tx_output_json,'$.result.state')='committed'
      AND json_extract(o.tx_output_json,'$.result.resources.state')='native_intent'
)
BEGIN SELECT RAISE(ABORT,'Resource proof requires original sending intent'); END;

CREATE TRIGGER planner_handoff_adoption_scope BEFORE INSERT ON planner_handoff_adoptions
WHEN NOT EXISTS (
    SELECT 1 FROM operations legacy JOIN operations owner ON owner.id=NEW.owner_operation_id
    WHERE legacy.id=NEW.legacy_operation_id AND legacy.attempt=NEW.legacy_attempt
      AND owner.kind='handoff-workflow' AND json_extract(owner.payload_json,'$.version')=2
      AND owner.phase='awaiting_retry'
      AND (
        (legacy.kind='handoff-coordinator'
          AND legacy.idempotency_key=owner.idempotency_key AND legacy.payload_hash=owner.payload_hash
          AND json_extract(legacy.payload_json,'$.request')=json_extract(owner.payload_json,'$.request')
          AND json_extract(legacy.payload_json,'$.source')=json_extract(owner.payload_json,'$.source')
          AND json_extract(legacy.payload_json,'$.target')=json_extract(owner.payload_json,'$.target')
          AND json_extract(legacy.payload_json,'$.plan_id')=json_extract(owner.payload_json,'$.plan_id')
          AND json_extract(legacy.payload_json,'$.new_session_id')=json_extract(owner.payload_json,'$.new_session_id'))
        OR (legacy.kind='handoff-cutover' AND json_extract(legacy.tx_output_json,'$.result.state')='cutover_committed_pending_activation'
          AND json_extract(legacy.payload_json,'$.scope.source_pause')=json_extract(owner.payload_json,'$.source')
          AND json_extract(legacy.payload_json,'$.scope.target_pause')=json_extract(owner.payload_json,'$.target')
          AND json_extract(legacy.payload_json,'$.scope.plan_id')=json_extract(owner.payload_json,'$.plan_id')
          AND json_extract(legacy.payload_json,'$.scope.new_session_id')=json_extract(owner.payload_json,'$.new_session_id')
          AND json_extract(legacy.payload_json,'$.scope.user_audit_id')=json_extract(owner.payload_json,'$.request.user_audit_id'))
        OR (legacy.kind='handoff-activation' AND EXISTS (
          SELECT 1 FROM planner_handoff_adoptions cutover
          WHERE cutover.legacy_operation_id=json_extract(legacy.payload_json,'$.cutover_operation_id')
            AND cutover.owner_operation_id=owner.id))
      )
)
BEGIN SELECT RAISE(ABORT,'Handoff adoption requires exact original scope and generation'); END;

-- An original driver can lose its lease between its intent and serial pause command.
-- Once exact compensation has settled, that late command cannot recreate the claim.
CREATE TRIGGER planner_handoff_owner_no_late_pause BEFORE UPDATE OF handle_state_json ON worker_sessions
WHEN json_extract(CASE WHEN json_valid(NEW.handle_state_json) THEN NEW.handle_state_json ELSE NULL END,'$.handoff_pause.state') IN
    ('paused','faulted_paused','recovery_health_unconfirmed')
 AND EXISTS (
    SELECT 1 FROM operations o WHERE o.kind='handoff-workflow'
      AND json_extract(o.tx_output_json,'$.result.state') IN ('finished','activated')
      AND (json_extract(o.payload_json,'$.source')=json_extract(CASE WHEN json_valid(NEW.handle_state_json) THEN NEW.handle_state_json ELSE NULL END,'$.handoff_pause.receipt')
        OR json_extract(o.payload_json,'$.target')=json_extract(CASE WHEN json_valid(NEW.handle_state_json) THEN NEW.handle_state_json ELSE NULL END,'$.handoff_pause.receipt'))
 )
BEGIN SELECT RAISE(ABORT,'Original handoff claim is already settled'); END;
