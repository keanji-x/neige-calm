-- Compensating is the irreversible execution-revocation fact for both original
-- claims. Reuse it and the immutable legacy Finished/adoption facts; no second log.
DROP TRIGGER planner_handoff_owner_no_late_pause;
DROP TRIGGER planner_handoff_legacy_no_late_pause;
DROP INDEX planner_handoff_legacy_source_claim;
DROP INDEX planner_handoff_legacy_target_claim;
CREATE INDEX planner_handoff_source_claim ON operations(json_extract(payload_json,'$.source'))
WHERE kind IN ('handoff-workflow','handoff-coordinator');
CREATE INDEX planner_handoff_target_claim ON operations(json_extract(payload_json,'$.target'))
WHERE kind IN ('handoff-workflow','handoff-coordinator');

CREATE TRIGGER planner_handoff_revocation_monotone
BEFORE UPDATE OF kind,payload_json,tx_output_json ON operations
WHEN ((OLD.kind='handoff-workflow'
       AND json_extract(OLD.tx_output_json,'$.result.state') IN ('compensating','finished','activated'))
   OR (OLD.kind='handoff-coordinator' AND json_extract(OLD.tx_output_json,'$.result.stage')='finished'))
 AND (NEW.kind IS NOT OLD.kind
   OR json_extract(NEW.payload_json,'$.source') IS NOT json_extract(OLD.payload_json,'$.source')
   OR json_extract(NEW.payload_json,'$.target') IS NOT json_extract(OLD.payload_json,'$.target')
   OR (OLD.kind='handoff-workflow' AND json_extract(OLD.tx_output_json,'$.result.state')='compensating'
       AND coalesce(json_extract(NEW.tx_output_json,'$.result.state'),'') NOT IN ('compensating','finished'))
   OR (OLD.kind='handoff-coordinator' AND coalesce(json_extract(NEW.tx_output_json,'$.result.stage'),'')!='finished'))
BEGIN SELECT RAISE(ABORT,'Original handoff claim revocation is irreversible'); END;

-- Creation only: already persisted identical claims retain fault/health/release
-- write authority. Final snapshot UPDATE and revocation serialize as SQLite writers.
CREATE TRIGGER planner_handoff_no_new_revoked_claim BEFORE UPDATE OF handle_state_json ON worker_sessions
WHEN json_extract(CASE WHEN json_valid(NEW.handle_state_json) THEN NEW.handle_state_json END,'$.handoff_pause.state')
     IN ('paused','faulted_paused','recovery_health_unconfirmed')
 AND NOT (
    json_extract(CASE WHEN json_valid(OLD.handle_state_json) THEN OLD.handle_state_json END,'$.handoff_pause.state')
      IN ('paused','faulted_paused','recovery_health_unconfirmed')
    AND json_extract(OLD.handle_state_json,'$.handoff_pause.receipt')
        IS json_extract(NEW.handle_state_json,'$.handoff_pause.receipt')
 )
 AND EXISTS (
    SELECT 1 FROM (
        SELECT id,kind,tx_output_json FROM operations WHERE kind IN ('handoff-workflow','handoff-coordinator')
          AND json_extract(payload_json,'$.source')=json_extract(NEW.handle_state_json,'$.handoff_pause.receipt')
        UNION ALL
        SELECT id,kind,tx_output_json FROM operations WHERE kind IN ('handoff-workflow','handoff-coordinator')
          AND json_extract(payload_json,'$.target')=json_extract(NEW.handle_state_json,'$.handoff_pause.receipt')
    ) o WHERE ((o.kind='handoff-workflow'
            AND json_extract(o.tx_output_json,'$.result.state') IN ('compensating','finished','activated'))
        OR (o.kind='handoff-coordinator'
            AND (json_extract(o.tx_output_json,'$.result.stage')='finished'
              OR EXISTS (SELECT 1 FROM planner_handoff_adoptions a WHERE a.legacy_operation_id=o.id))))
 )
BEGIN SELECT RAISE(ABORT,'Original handoff claim execution was revoked'); END;
