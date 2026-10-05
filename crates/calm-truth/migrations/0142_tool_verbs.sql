-- #2087 B1: a kernel tool's action is its Unix/git verb on the object it acts on. The view tools
-- 0141 left with noun or synonym actions are renamed; no alias serves an old name. Two stored values
-- are read back by name and are rewritten once:
--   * the `tool` field of Planner transcript rows (`$.item.tool`), read by the fe history. Each
--     old name maps exactly to its new name; nothing else in params is touched.
--   * the body of a track recipe, read by agents as instructions, through the same names plus the
--     CLI spellings `neige track state` and `neige tool list`. The scan is 0141's: occurrences,
--     identifier continuations unchanged, trailing sentence dots kept as punctuation. Each changed
--     row bumps `revision` and `updated_at`.
UPDATE harness_items
   SET params = json_set(params, '$.item.tool',
         CASE json_extract(params, '$.item.tool')
           WHEN 'neige_plan_list' THEN 'neige_task_ls'
           WHEN 'neige_plan_cancel' THEN 'neige_task_cancel'
           WHEN 'neige_source_list' THEN 'neige_source_ls'
           WHEN 'neige_area_outline' THEN 'neige_area_ls'
           WHEN 'neige_report_backlinks' THEN 'neige_link_ls'
           WHEN 'neige_report_kinds' THEN 'neige_report_describe'
           WHEN 'neige_track_state' THEN 'neige_track_status'
           WHEN 'neige_workspace_reports' THEN 'neige_workspace_ls'
           WHEN 'neige_workspace_report' THEN 'neige_workspace_cat'
           WHEN 'neige_workspace_changes' THEN 'neige_workspace_diff'
           WHEN 'neige_workspace_edits' THEN 'neige_workspace_log'
         END)
 WHERE json_valid(params)
   AND json_type(params, '$.item.tool') = 'text'
   AND json_extract(params, '$.item.tool') IN (
         'neige_plan_list', 'neige_plan_cancel', 'neige_source_list', 'neige_area_outline',
         'neige_report_backlinks', 'neige_report_kinds', 'neige_track_state',
         'neige_workspace_reports', 'neige_workspace_report', 'neige_workspace_changes',
         'neige_workspace_edits');

WITH RECURSIVE
names(step, old, new) AS (VALUES
  (1, 'neige_plan_list', 'neige_task_ls'),
  (2, 'neige_plan_cancel', 'neige_task_cancel'),
  (3, 'neige_source_list', 'neige_source_ls'),
  (4, 'neige_area_outline', 'neige_area_ls'),
  (5, 'neige_report_backlinks', 'neige_link_ls'),
  (6, 'neige_report_kinds', 'neige_report_describe'),
  (7, 'neige_track_state', 'neige_track_status'),
  (8, 'neige_workspace_reports', 'neige_workspace_ls'),
  (9, 'neige_workspace_report', 'neige_workspace_cat'),
  (10, 'neige_workspace_changes', 'neige_workspace_diff'),
  (11, 'neige_workspace_edits', 'neige_workspace_log'),
  (12, 'neige track state', 'neige track status'),
  (13, 'neige tool list', 'neige tool ls')
),
scan(id, step, rest, output, previous) AS (
  SELECT id, 1, body, '', '' FROM track_recipes
   WHERE instr(body, 'neige_') > 0
      OR instr(body, 'neige track state') > 0
      OR instr(body, 'neige tool list') > 0
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
 WHERE scan.step = 14 AND track_recipes.id = scan.id
   AND track_recipes.body <> scan.rest;
