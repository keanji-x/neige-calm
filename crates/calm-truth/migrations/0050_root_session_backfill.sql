-- PR7b-i Unit 1 (#679): backfill wave roots from active planner sessions.
--
-- Deterministic and idempotent: each wave derives the same root from the
-- newest active planner session by updated_at_ms, then created_at_ms, then id.
UPDATE waves
   SET root_session_id = (
       SELECT ws.id
         FROM worker_sessions ws
        WHERE ws.wave_id = waves.id
          AND ws.contract = 'planner'
          AND ws.state IN ('starting', 'running', 'idle', 'turn_pending')
        ORDER BY ws.updated_at_ms DESC,
                 ws.created_at_ms DESC,
                 ws.id DESC
        LIMIT 1
   );
