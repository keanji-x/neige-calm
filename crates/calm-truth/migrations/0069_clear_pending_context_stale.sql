-- #985: claim 前定位失败从未产生判决事件；清除历史误写，使投影与事件日志重新一致。
-- 幂等且只触碰仍为 pending 的行；material 只能属于 in-flight 行。
UPDATE tasks
SET context_stale_at_ms = NULL
WHERE status = 'pending'
  AND context_stale_at_ms IS NOT NULL;
