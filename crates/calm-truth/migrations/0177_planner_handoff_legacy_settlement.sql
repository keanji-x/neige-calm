-- Legacy Finished could precede an original pause already past its intent seam.
-- Adopted old claims must also be fenced before serial Resolve: a later command
-- cannot land between its absence barrier and the terminal transaction.
-- Index only legacy identities; ordinary live writers never scan workflow history.
CREATE INDEX planner_handoff_legacy_source_claim
ON operations(json_extract(payload_json,'$.source'))
WHERE kind='handoff-coordinator';
CREATE INDEX planner_handoff_legacy_target_claim
ON operations(json_extract(payload_json,'$.target'))
WHERE kind='handoff-coordinator';
CREATE TRIGGER planner_handoff_legacy_no_late_pause BEFORE UPDATE OF handle_state_json ON worker_sessions
WHEN json_extract(CASE WHEN json_valid(NEW.handle_state_json) THEN NEW.handle_state_json ELSE NULL END,'$.handoff_pause.state') IN
    ('paused','faulted_paused','recovery_health_unconfirmed')
 AND NOT (
    json_extract(CASE WHEN json_valid(OLD.handle_state_json) THEN OLD.handle_state_json ELSE NULL END,'$.handoff_pause.state') IN
      ('paused','faulted_paused','recovery_health_unconfirmed')
    AND json_extract(OLD.handle_state_json,'$.handoff_pause.receipt') IS json_extract(NEW.handle_state_json,'$.handoff_pause.receipt')
 )
 AND (
    EXISTS (SELECT 1 FROM operations o
      WHERE o.kind='handoff-coordinator'
        AND (json_extract(o.tx_output_json,'$.result.stage')='finished' OR EXISTS (SELECT 1 FROM planner_handoff_adoptions a WHERE a.legacy_operation_id=o.id))
        AND json_extract(o.payload_json,'$.source')=json_extract(NEW.handle_state_json,'$.handoff_pause.receipt'))
    OR EXISTS (SELECT 1 FROM operations o
      WHERE o.kind='handoff-coordinator'
        AND (json_extract(o.tx_output_json,'$.result.stage')='finished' OR EXISTS (SELECT 1 FROM planner_handoff_adoptions a WHERE a.legacy_operation_id=o.id))
        AND json_extract(o.payload_json,'$.target')=json_extract(NEW.handle_state_json,'$.handoff_pause.receipt'))
 )
BEGIN SELECT RAISE(ABORT,'Original legacy handoff claim is already settled'); END;
