-- #2348 — every Planner card carries the server-owned payload key `permission_mode`, which its
-- creation mints as "never". A Planner card created before that is stamped "never" here. A card
-- is selected exactly as `PlannerBinding::from_shape` binds a Planner: `kind = 'codex'`,
-- `role = 'planner'`, and a `planner_provider` that is a JSON string naming an `AgentProvider`
-- ("codex" or "claude"). A stored `permission_mode`, whatever it holds, is kept, and a payload
-- that is not valid JSON is not touched. A rerun changes nothing.
UPDATE cards
   SET payload = json_set(payload, '$.permission_mode', 'never')
 WHERE kind = 'codex'
   AND role = 'planner'
   AND CASE WHEN json_valid(payload)
            THEN json_type(payload) = 'object'
             AND json_type(payload, '$.planner_provider') = 'text'
             AND json_extract(payload, '$.planner_provider') IN ('codex', 'claude')
             AND json_type(payload, '$.permission_mode') IS NULL
            ELSE 0
       END;
