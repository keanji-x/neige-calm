-- #2003 — the public tool surface is `neige.<object>.<action>` under the MCP server key `neige`.
-- Two stored values are read back by name, so they are rewritten once:
--   * the `tool` field of Planner transcript rows (`$.item.tool` in their params), read by
--     the fe history and the activity projector. Nothing else in params is touched.
--   * the body of a track recipe, read by agents as instructions; each changed row bumps
--     `revision` (the optimistic-lock anchor) and `updated_at`.
-- The map is the #2003 rename table, the four historical aliases, the two retired shims, the
-- three retired writers stored on 4140, and the five Claude-qualified plugin names stored there.
WITH tool_map(old, new) AS (VALUES
  ('calm.admin.track_gc', 'neige.admin.gc'),
  ('calm.admin.vacuum', 'neige.admin.vacuum'),
  ('calm.area.outline', 'neige.area.outline'),
  ('calm.calendar.create', 'neige.calendar.create'),
  ('calm.calendar.list', 'neige.calendar.list'),
  ('calm.calendar.update', 'neige.calendar.update'),
  ('calm.plan.cancel', 'neige.plan.cancel'),
  ('calm.plan.list', 'neige.plan.list'),
  ('calm.preview.register', 'neige.preview.register'),
  ('calm.preview.unregister', 'neige.preview.unregister'),
  ('calm.ratify.request', 'neige.ratify.request'),
  ('calm.report.blocks.kinds', 'neige.report.kinds'),
  ('calm.report.commit', 'neige.report.commit'),
  ('calm.report.find', 'neige.report.find'),
  ('calm.report.links.backlinks', 'neige.report.backlinks'),
  ('calm.report.read', 'neige.report.read'),
  ('calm.report.tag', 'neige.report.tag'),
  ('calm.report.write_markdown', 'neige.report.write'),
  ('calm.review.round', 'neige.review.round'),
  ('calm.source.capture', 'neige.source.capture'),
  ('calm.source.list', 'neige.source.list'),
  ('calm.task.complete', 'neige.task.complete'),
  ('calm.task.fail', 'neige.task.fail'),
  ('calm.task.verdict', 'neige.task.verdict'),
  ('calm.terminal.control', 'neige.terminal.control'),
  ('calm.terminal.input', 'neige.terminal.input'),
  ('calm.terminal.observe', 'neige.terminal.observe'),
  ('calm.terminal.open', 'neige.terminal.open'),
  ('calm.terminal.resolve', 'neige.terminal.resolve'),
  ('calm.track.cat', 'neige.track.cat'),
  ('calm.track.cat_at', 'neige.track.show'),
  ('calm.track.close', 'neige.track.close'),
  ('calm.track.diff', 'neige.track.diff'),
  ('calm.track.log', 'neige.track.log'),
  ('calm.track.ls', 'neige.track.ls'),
  ('calm.track.publish', 'neige.track.publish'),
  ('calm.track.rename', 'neige.track.rename'),
  ('calm.track.state', 'neige.track.state'),
  ('calm.user.notify', 'neige.user.notify'),
  ('calm.get_track_state', 'neige.track.state'),
  ('calm.update_task_meta', 'neige.task.verdict'),
  ('calm.task_completed', 'neige.task.complete'),
  ('calm.task_failed', 'neige.task.fail'),
  ('calm.dispatch_request', 'neige.dispatch.request'),
  ('calm.plan.upsert', 'neige.plan.upsert'),
  ('calm.report.blocks.upsert', 'neige.report.upsert'),
  ('calm.report.blocks.delete', 'neige.report.delete'),
  ('calm.task.replace', 'neige.task.replace'),
  ('mcp__calm__plugin_dev_neige_git-forge_gh_issue_close', 'plugin.dev.neige.git-forge_gh.issue.close'),
  ('mcp__calm__plugin_dev_neige_git-forge_gh_issue_view', 'plugin.dev.neige.git-forge_gh.issue.view'),
  ('mcp__calm__plugin_dev_neige_git-forge_gh_pr_checks', 'plugin.dev.neige.git-forge_gh.pr.checks'),
  ('mcp__calm__plugin_dev_neige_git-forge_gh_pr_diff', 'plugin.dev.neige.git-forge_gh.pr.diff'),
  ('mcp__calm__plugin_dev_neige_git-forge_gh_pr_merge', 'plugin.dev.neige.git-forge_gh.pr.merge')
)
UPDATE harness_items AS item
   SET params = json_set(item.params, '$.item.tool', tool_map.new)
  FROM tool_map
 WHERE json_valid(item.params)
   AND json_extract(item.params, '$.item.tool') = tool_map.old;

UPDATE track_recipes
   SET body = renamed.body,
       revision = track_recipes.revision + 1,
       updated_at = MAX(
         track_recipes.updated_at + 1,
         CAST((julianday('now') - 2440587.5) * 86400000 AS INTEGER))
  FROM (SELECT id,
               replace(replace(replace(replace(replace(replace(replace(replace(replace(replace(
               replace(replace(replace(replace(replace(replace(replace(replace(body,
                 'calm.admin.track_gc', 'neige.admin.gc'),
                 'calm.report.blocks.kinds', 'neige.report.kinds'),
                 'calm.report.links.backlinks', 'neige.report.backlinks'),
                 'calm.report.write_markdown', 'neige.report.write'),
                 'calm.track.cat_at', 'neige.track.show'),
                 'calm.admin.', 'neige.admin.'),
                 'calm.area.', 'neige.area.'),
                 'calm.calendar.', 'neige.calendar.'),
                 'calm.plan.', 'neige.plan.'),
                 'calm.preview.', 'neige.preview.'),
                 'calm.ratify.', 'neige.ratify.'),
                 'calm.report.', 'neige.report.'),
                 'calm.review.', 'neige.review.'),
                 'calm.source.', 'neige.source.'),
                 'calm.task.', 'neige.task.'),
                 'calm.terminal.', 'neige.terminal.'),
                 'calm.track.', 'neige.track.'),
                 'calm.user.', 'neige.user.') AS body
          FROM track_recipes) AS renamed
 WHERE track_recipes.id = renamed.id
   AND track_recipes.body <> renamed.body;
