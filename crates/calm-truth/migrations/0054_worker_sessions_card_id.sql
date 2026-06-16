-- #679 PR9b-0 — add worker_sessions.card_id (additive, nullable, no rebuild).
--
-- WHY NULLABLE + IN-PLACE (not a CREATE-new/DROP/RENAME rebuild):
--   worker_sessions has inbound FKs with ON DELETE SET NULL (cards.session_id
--   -> worker_sessions(id), migration 0045) and NO ACTION (waves.root_session_id).
--   The migration runner wraps each file in a transaction, where
--   `PRAGMA foreign_keys=OFF` is a no-op and `PRAGMA defer_foreign_keys=ON` does
--   NOT suppress ON DELETE *actions*. A `DROP TABLE worker_sessions` rebuild would
--   fire `cards.session_id := NULL` for every card during DROP's implicit delete
--   (verified by spike), destroying the load-bearing card<->session pointers.
--   So we add the column in place. SQLite also cannot ADD COLUMN that is BOTH
--   NOT NULL and REFERENCES in one step; NOT NULL is enforced at the app layer
--   (non-null dual-write + boot-assert on active rows) instead.
--
-- BACKFILL AUTHORITY: `runtimes` (dual-written id-for-id) is truth during the
-- dual-write era; `cards.session_id` reverse-fill covers deferred-spec
-- placeholders that have no runtimes row. A residual NULL can only remain on a
-- TERMINAL (failed/superseded) deferred-spec placeholder whose card was deleted
-- before Phase-2 minted its runtimes mirror — an already-leaked, unreachable
-- historical artifact (card_delete_tx only deletes ws rows that HAVE a runtimes
-- mirror). Such rows keep card_id = NULL and are excluded from every card-keyed
-- read (NULL never matches `ws.card_id = ?`). The boot-assert only flags ACTIVE
-- rows, so these terminal orphans do not trip it.

ALTER TABLE worker_sessions ADD COLUMN card_id TEXT;

UPDATE worker_sessions
   SET card_id = COALESCE(
       (SELECT r.card_id FROM runtimes r WHERE r.id = worker_sessions.id),
       (SELECT c.id      FROM cards    c WHERE c.session_id = worker_sessions.id)
   )
 WHERE card_id IS NULL;

CREATE INDEX IF NOT EXISTS ws_card_id_idx
    ON worker_sessions(card_id) WHERE card_id IS NOT NULL;
