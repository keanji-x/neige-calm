-- #570 P2-C followup - clear HarnessSnapshot.last_thread_id from
-- runtimes.handle_state_json on the same set of pre-#567 spec
-- runtimes that 0035 already nulled (thread_id IS NULL, card is
-- spec, card has no card_mcp_tokens row). Without this, boot
-- recovery's SpecHarness::run thread_id fallback (harness/mod.rs:66)
-- would re-attach to the stale snapshot thread_id and persist it
-- back into runtimes.thread_id, defeating 0035.
--
-- json_remove(NULL, ...) returns NULL and is a no-op on rows whose
-- snapshot already lacks the key, so the WHERE filter narrows by
-- whichever spec+no-token runtimes survived 0035 with a non-NULL
-- snapshot.

UPDATE runtimes
   SET handle_state_json = json_remove(handle_state_json, '$.last_thread_id'),
       updated_at_ms = CAST((julianday('now') - 2440587.5) * 86400000 AS INTEGER)
 WHERE handle_state_json IS NOT NULL
   AND (thread_id IS NULL OR thread_id = '')
   AND card_id IN (SELECT id FROM cards WHERE role = 'spec')
   AND card_id NOT IN (SELECT card_id FROM card_mcp_tokens);
