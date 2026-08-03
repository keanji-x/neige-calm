-- #985 防御性清理：pending stale 在已发布版本中不可达；所有 stale 写者都有
-- in-flight 守卫，且行不会回到 pending。仅为曾带 claim 侧误写的短命开发分支兜底。
UPDATE tasks
SET context_stale_at_ms = NULL
WHERE status = 'pending'
  AND context_stale_at_ms IS NOT NULL;
