-- #570 P2-C — one-shot: null thread_id on spec runtimes whose card has
-- no per-card MCP token row. Such runtimes attached to a codex thread
-- spawned BEFORE PR #567 introduced shell_environment_policy.set
-- injection, so the AI shell has no NEIGE_MCP_TOKEN in its env and
-- every `neige` call returns -32401. Nulling forces the next
-- spec-harness-start to mint a fresh thread (and write the per-card
-- token). Conversation history of those threads is lost once.
--
-- Cards that already minted under PR #567 own a row in
-- card_mcp_tokens; their runtimes are untouched.

UPDATE runtimes
   SET thread_id = NULL,
       handle_state_json = json_remove(handle_state_json, '$.last_thread_id'),
       updated_at_ms = CAST((julianday('now') - 2440587.5) * 86400000 AS INTEGER)
 WHERE thread_id IS NOT NULL
   AND thread_id <> ''
   AND card_id IN (SELECT id FROM cards WHERE role = 'spec')
   AND card_id NOT IN (SELECT card_id FROM card_mcp_tokens);
