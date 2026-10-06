-- #2227: a tool that comes and goes with a plugin's enablement or a Track's plugin scope is a
-- plugin tool `plugin_<id>_<tool>`, compiled built-ins included, so `neige_dev_publish` becomes
-- `plugin_gitforge_publish` and `neige_calendar_{add,ls,set,rm}` become
-- `plugin_calendar_{add,ls,set,rm}`. No alias serves an old name. Two stored values are read back
-- by name and are rewritten once:
--   * the `tool` field of Planner transcript rows (`$.item.tool`), read by the fe history. Each
--     old name maps exactly to its new name; nothing else in params is touched.
--   * the body of a track recipe, read by agents as instructions, through the same names. The scan
--     is 0141's: occurrences, identifier continuations unchanged, trailing sentence dots kept as
--     punctuation. Each changed row bumps `revision` and `updated_at`.
-- History stores (events, report history, settled git deliveries) are not rewritten. No other
-- store holds these names: a publish's operation key is `track.publish:<idempotency_key>` under
-- the plugin id, and a calendar entry records its creator card, not the tool.
UPDATE harness_items
   SET params = json_set(params, '$.item.tool',
         CASE json_extract(params, '$.item.tool')
           WHEN 'neige_dev_publish' THEN 'plugin_gitforge_publish'
           WHEN 'neige_calendar_add' THEN 'plugin_calendar_add'
           WHEN 'neige_calendar_ls' THEN 'plugin_calendar_ls'
           WHEN 'neige_calendar_set' THEN 'plugin_calendar_set'
           WHEN 'neige_calendar_rm' THEN 'plugin_calendar_rm'
         END)
 WHERE json_valid(params)
   AND json_type(params, '$.item.tool') = 'text'
   AND json_extract(params, '$.item.tool') IN (
         'neige_dev_publish', 'neige_calendar_add', 'neige_calendar_ls', 'neige_calendar_set',
         'neige_calendar_rm');

WITH RECURSIVE
names(step, old, new) AS (VALUES
  (1, 'neige_dev_publish', 'plugin_gitforge_publish'),
  (2, 'neige_calendar_add', 'plugin_calendar_add'),
  (3, 'neige_calendar_ls', 'plugin_calendar_ls'),
  (4, 'neige_calendar_set', 'plugin_calendar_set'),
  (5, 'neige_calendar_rm', 'plugin_calendar_rm')
),
scan(id, step, rest, output, previous) AS (
  SELECT id, 1, body, '', '' FROM track_recipes
   WHERE instr(body, 'neige_dev_publish') > 0
      OR instr(body, 'neige_calendar_') > 0
  UNION ALL
  SELECT id,
         CASE WHEN instr(rest, old) = 0 THEN scan.step + 1 ELSE scan.step END,
         CASE WHEN instr(rest, old) = 0 THEN output || rest
              ELSE substr(rest, instr(rest, old) + length(old)) END,
         CASE WHEN instr(rest, old) = 0 THEN ''
              ELSE output || substr(rest, 1, instr(rest, old) - 1) ||
                CASE WHEN
                  (CASE WHEN instr(rest, old) = 1 THEN previous
                        ELSE substr(rest, instr(rest, old) - 1, 1) END)
                    NOT GLOB '[A-Za-z0-9_.-]'
                  AND substr(rest, instr(rest, old) + length(old), 1)
                    NOT GLOB '[A-Za-z0-9_-]'
                  AND substr(ltrim(substr(rest, instr(rest, old) + length(old)), '.'), 1, 1)
                    NOT GLOB '[A-Za-z0-9_.-]'
                THEN new ELSE old END END,
         CASE WHEN instr(rest, old) = 0 THEN '' ELSE substr(old, -1) END
    FROM scan JOIN names ON names.step = scan.step
)
UPDATE track_recipes
   SET body = scan.rest,
       revision = track_recipes.revision + 1,
       updated_at = MAX(track_recipes.updated_at + 1,
         CAST((julianday('now') - 2440587.5) * 86400000 AS INTEGER))
  FROM scan
 WHERE scan.step = 6 AND track_recipes.id = scan.id
   AND track_recipes.body <> scan.rest;
