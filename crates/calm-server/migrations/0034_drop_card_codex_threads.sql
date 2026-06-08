UPDATE runtimes
SET thread_id = (
    SELECT cct.thread_id
    FROM card_codex_threads cct
    WHERE cct.card_id = runtimes.card_id
    ORDER BY cct.updated_at DESC
    LIMIT 1
)
WHERE (thread_id IS NULL OR thread_id = '')
  AND EXISTS (
      SELECT 1
      FROM card_codex_threads cct
      WHERE cct.card_id = runtimes.card_id
  );

DROP TABLE IF EXISTS card_codex_threads;
