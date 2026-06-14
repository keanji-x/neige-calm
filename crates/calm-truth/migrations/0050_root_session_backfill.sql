-- PR7b-i Unit 1 (#679): backfill wave roots and session auth identity.
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

-- Upgraded DBs can have runtime/session mirrors without the new card-level
-- session link. Backfill the same current-runtime identity the read path
-- projects: active runtimes first, then latest non-superseded runtime.
UPDATE cards
   SET session_id = (
       SELECT ws.id
         FROM runtimes r
         JOIN worker_sessions ws ON ws.id = r.id
        WHERE r.card_id = cards.id
          AND r.status != 'superseded'
        ORDER BY CASE
                   WHEN r.status IN ('starting', 'running', 'idle', 'turn_pending')
                     THEN 0
                   ELSE 1
                 END ASC,
                 r.updated_at_ms DESC,
                 r.created_at_ms DESC,
                 r.id DESC
        LIMIT 1
   )
 WHERE cards.session_id IS NULL
   AND EXISTS (
       SELECT 1
         FROM runtimes r
         JOIN worker_sessions ws ON ws.id = r.id
        WHERE r.card_id = cards.id
          AND r.status != 'superseded'
   );

-- Pre-session-only MCP tokens live on card_mcp_tokens. Mirror them onto the
-- active same-id worker session so initialize can authenticate through the
-- session lookup after upgrade. The uniqueness guards keep ws_token_idx from
-- being violated by malformed/duplicate historical token rows.
UPDATE worker_sessions
   SET mcp_token_hash = (
       SELECT cmt.hashed_token
         FROM runtimes r
         JOIN card_mcp_tokens cmt ON cmt.card_id = r.card_id
        WHERE r.id = worker_sessions.id
          AND 1 = (
              SELECT COUNT(*)
                FROM card_mcp_tokens dup
               WHERE dup.hashed_token = cmt.hashed_token
          )
          AND NOT EXISTS (
              SELECT 1
                FROM worker_sessions other
               WHERE other.id != worker_sessions.id
                 AND other.mcp_token_hash = cmt.hashed_token
          )
        LIMIT 1
   )
 WHERE worker_sessions.mcp_token_hash IS NULL
   AND worker_sessions.state IN ('starting', 'running', 'idle', 'turn_pending')
   AND EXISTS (
       SELECT 1
         FROM runtimes r
         JOIN card_mcp_tokens cmt ON cmt.card_id = r.card_id
        WHERE r.id = worker_sessions.id
          AND 1 = (
              SELECT COUNT(*)
                FROM card_mcp_tokens dup
               WHERE dup.hashed_token = cmt.hashed_token
          )
          AND NOT EXISTS (
              SELECT 1
                FROM worker_sessions other
               WHERE other.id != worker_sessions.id
                 AND other.mcp_token_hash = cmt.hashed_token
          )
   );
