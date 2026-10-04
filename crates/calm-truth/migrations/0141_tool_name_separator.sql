-- #2087: `_` is the only separator of a tool name, so the name a model sees is the name that
-- prompts, docs and help print. Two stored values are read back by name and are rewritten once:
--   * the `tool` field of Planner transcript rows (`$.item.tool`), read by the fe history and the
--     activity projector. Every dotted kernel name becomes `_`-separated; the two Worker report
--     actions become `neige_task_done` / `neige_task_fail`; a minted plugin name changes only its
--     `plugin.` prefix to `plugin_`. Nothing else in params is touched.
--   * the body of a track recipe, read by agents as instructions, through an explicit map: the
--     registered names, the names migrations 0134 and 0135 can leave in a body, and the CLI
--     report spellings. The scan is 0135's: occurrences, identifier continuations unchanged,
--     trailing sentence dots kept as punctuation. Each changed row bumps `revision` and
--     `updated_at`.
UPDATE harness_items
   SET params = json_set(params, '$.item.tool',
         CASE
           WHEN json_extract(params, '$.item.tool')
                IN ('neige.task.report_success', 'neige.task.complete') THEN 'neige_task_done'
           WHEN json_extract(params, '$.item.tool') = 'neige.task.report_failure' THEN 'neige_task_fail'
           WHEN substr(json_extract(params, '$.item.tool'), 1, 6) = 'neige.'
             THEN replace(json_extract(params, '$.item.tool'), '.', '_')
           ELSE 'plugin_' || substr(json_extract(params, '$.item.tool'), 8)
         END)
 WHERE json_valid(params)
   AND json_type(params, '$.item.tool') = 'text'
   AND (substr(json_extract(params, '$.item.tool'), 1, 6) = 'neige.'
     OR substr(json_extract(params, '$.item.tool'), 1, 7) = 'plugin.');

WITH RECURSIVE
names(step, old, new) AS (VALUES
  (1, 'neige.admin.gc', 'neige_admin_gc'),
  (2, 'neige.admin.vacuum', 'neige_admin_vacuum'),
  (3, 'neige.area.outline', 'neige_area_outline'),
  (4, 'neige.calendar.create', 'neige_calendar_create'),
  (5, 'neige.calendar.list', 'neige_calendar_list'),
  (6, 'neige.calendar.update', 'neige_calendar_update'),
  (7, 'neige.dev.publish', 'neige_dev_publish'),
  (8, 'neige.plan.cancel', 'neige_plan_cancel'),
  (9, 'neige.plan.list', 'neige_plan_list'),
  (10, 'neige.preview.register', 'neige_preview_register'),
  (11, 'neige.preview.unregister', 'neige_preview_unregister'),
  (12, 'neige.ratify.request', 'neige_ratify_request'),
  (13, 'neige.report.backlinks', 'neige_report_backlinks'),
  (14, 'neige.report.commit', 'neige_report_commit'),
  (15, 'neige.report.find', 'neige_report_find'),
  (16, 'neige.report.kinds', 'neige_report_kinds'),
  (17, 'neige.report.read', 'neige_report_read'),
  (18, 'neige.report.tag', 'neige_report_tag'),
  (19, 'neige.report.write', 'neige_report_write'),
  (20, 'neige.source.capture', 'neige_source_capture'),
  (21, 'neige.source.list', 'neige_source_list'),
  (22, 'neige.task.report_success', 'neige_task_done'),
  (23, 'neige.task.report_failure', 'neige_task_fail'),
  (24, 'neige.task.verdict', 'neige_task_verdict'),
  (25, 'neige.terminal.control', 'neige_terminal_control'),
  (26, 'neige.terminal.input', 'neige_terminal_input'),
  (27, 'neige.terminal.observe', 'neige_terminal_observe'),
  (28, 'neige.terminal.open', 'neige_terminal_open'),
  (29, 'neige.terminal.resolve', 'neige_terminal_resolve'),
  (30, 'neige.track.cat', 'neige_track_cat'),
  (31, 'neige.track.close', 'neige_track_close'),
  (32, 'neige.track.diff', 'neige_track_diff'),
  (33, 'neige.track.log', 'neige_track_log'),
  (34, 'neige.track.ls', 'neige_track_ls'),
  (35, 'neige.track.rename', 'neige_track_rename'),
  (36, 'neige.track.show', 'neige_track_show'),
  (37, 'neige.track.state', 'neige_track_state'),
  (38, 'neige.user.notify', 'neige_user_notify'),
  (39, 'neige.workspace.changes', 'neige_workspace_changes'),
  (40, 'neige.workspace.edits', 'neige_workspace_edits'),
  (41, 'neige.workspace.report', 'neige_workspace_report'),
  (42, 'neige.workspace.reports', 'neige_workspace_reports'),
  (43, 'neige.task.complete', 'neige_task_done'),
  (44, 'neige.task.fail', 'neige_task_fail'),
  (45, 'neige.track.publish', 'neige_dev_publish'),
  (46, 'neige.dispatch.request', 'neige_dispatch_request'),
  (47, 'neige.plan.upsert', 'neige_plan_upsert'),
  (48, 'neige.report.delete', 'neige_report_delete'),
  (49, 'neige.report.upsert', 'neige_report_upsert'),
  (50, 'neige.review.round', 'neige_review_round'),
  (51, 'neige.task.replace', 'neige_task_replace'),
  (52, 'task report-success', 'task done'),
  (53, 'task report-failure', 'task fail'),
  (54, 'task-report-success', 'task done'),
  (55, 'task-report-failure', 'task fail')
),
scan(id, step, rest, output, previous) AS (
  SELECT id, 1, body, '', '' FROM track_recipes
   WHERE instr(body, 'neige.') > 0
      OR instr(body, 'report-success') > 0
      OR instr(body, 'report-failure') > 0
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
 WHERE scan.step = 56 AND track_recipes.id = scan.id
   AND track_recipes.body <> scan.rest;
