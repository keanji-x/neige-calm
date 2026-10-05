-- #2087 B5: the two built-in plugin ids are words (`dev.neige.calendar` -> `calendar`,
-- `dev.neige.git-forge` -> `gitforge`), and every plugin tool name is minted
-- `plugin_<id>_<tool>` with each character of <id> and <tool> outside [A-Za-z0-9_] written `_`.
-- Installed external plugins keep their ids. Stored values read back by id or by minted name are
-- rewritten once; history stores (events, report history, settled git deliveries) are not.
--   * `plugins`: foreign keys stay on inside the migrator, and `plugin_kv` / `plugin_tokens`
--     reference `plugins(id)` with no ON UPDATE, so each built-in row is copied under its new id,
--     its children move, and the old row is deleted. `reconcile_builtins` then refreshes the copy.
--   * `tracks.plugin_scope`: the owning plugin id.
--   * `operations.idempotency_key`: a git-forge action's key is `<plugin_id>:<track>:<card>:<idem>`,
--     recomputed on every submit, so the prefix moves and the dedup wall keeps holding. Keyed rows
--     are never deleted; an update is allowed.
--   * the `tool` field of Planner transcript rows (`$.item.tool`), read by the fe history: a
--     built-in id prefix takes its new id, then every character outside [A-Za-z0-9_] becomes `_`,
--     as the kernel mints. Codex-native `plugin_management.*` calls are not ours and are kept.
--   * the body of a track recipe, read by agents as instructions: the minted built-in names
--     through 0141's occurrence scan, and the built-in `neige://plugin/<id>/` prefixes. Each
--     changed row bumps `revision` and `updated_at`.
INSERT INTO plugins (id, version, install_path, manifest, enabled, user_config, installed_at,
                     updated_at)
SELECT CASE id WHEN 'dev.neige.calendar' THEN 'calendar' ELSE 'gitforge' END,
       version,
       CASE id WHEN 'dev.neige.calendar' THEN 'builtin:calendar' ELSE 'builtin:gitforge' END,
       CASE WHEN json_valid(manifest) THEN json_set(manifest, '$.id',
              CASE id WHEN 'dev.neige.calendar' THEN 'calendar' ELSE 'gitforge' END)
            ELSE manifest END,
       enabled, user_config, installed_at, updated_at
  FROM plugins
 WHERE id IN ('dev.neige.calendar', 'dev.neige.git-forge')
   AND NOT EXISTS (SELECT 1 FROM plugins AS p
                    WHERE p.id = CASE plugins.id WHEN 'dev.neige.calendar' THEN 'calendar'
                                                 ELSE 'gitforge' END);

UPDATE plugin_kv
   SET plugin_id = CASE plugin_id WHEN 'dev.neige.calendar' THEN 'calendar' ELSE 'gitforge' END
 WHERE plugin_id IN ('dev.neige.calendar', 'dev.neige.git-forge');

UPDATE plugin_tokens
   SET plugin_id = CASE plugin_id WHEN 'dev.neige.calendar' THEN 'calendar' ELSE 'gitforge' END
 WHERE plugin_id IN ('dev.neige.calendar', 'dev.neige.git-forge');

DELETE FROM plugins WHERE id IN ('dev.neige.calendar', 'dev.neige.git-forge');

UPDATE tracks
   SET plugin_scope = CASE plugin_scope WHEN 'dev.neige.calendar' THEN 'calendar'
                                        ELSE 'gitforge' END
 WHERE plugin_scope IN ('dev.neige.calendar', 'dev.neige.git-forge');

UPDATE operations
   SET idempotency_key = 'gitforge:' || substr(idempotency_key, length('dev.neige.git-forge:') + 1)
 WHERE substr(idempotency_key, 1, length('dev.neige.git-forge:')) = 'dev.neige.git-forge:';

UPDATE harness_items
   SET params = json_set(params, '$.item.tool', (
         WITH RECURSIVE minted(rest, output) AS (
           SELECT CASE
                    WHEN substr(json_extract(params, '$.item.tool'), 1, 27)
                         = 'plugin_dev.neige.git-forge_'
                      THEN 'plugin_gitforge_' || substr(json_extract(params, '$.item.tool'), 28)
                    WHEN substr(json_extract(params, '$.item.tool'), 1, 26)
                         = 'plugin_dev.neige.calendar_'
                      THEN 'plugin_calendar_' || substr(json_extract(params, '$.item.tool'), 27)
                    ELSE json_extract(params, '$.item.tool')
                  END, ''
           UNION ALL
           SELECT substr(rest, 2),
                  output || CASE WHEN substr(rest, 1, 1) GLOB '[A-Za-z0-9_]'
                                 THEN substr(rest, 1, 1) ELSE '_' END
             FROM minted
            WHERE rest <> ''
         )
         SELECT output FROM minted WHERE rest = ''))
 WHERE json_valid(params)
   AND json_type(params, '$.item.tool') = 'text'
   AND substr(json_extract(params, '$.item.tool'), 1, 7) = 'plugin_'
   AND substr(json_extract(params, '$.item.tool'), 1, 18) <> 'plugin_management.'
   AND json_extract(params, '$.item.tool') GLOB '*[^A-Za-z0-9_]*';

WITH RECURSIVE
names(step, old, new) AS (VALUES
  (1, 'plugin_dev.neige.git-forge_git.worktree.add', 'plugin_gitforge_git_worktree_add'),
  (2, 'plugin_dev.neige.git-forge_git.commit', 'plugin_gitforge_git_commit'),
  (3, 'plugin_dev.neige.git-forge_gh.pr.list', 'plugin_gitforge_gh_pr_list'),
  (4, 'plugin_dev.neige.git-forge_gh.pr.diff', 'plugin_gitforge_gh_pr_diff'),
  (5, 'plugin_dev.neige.git-forge_gh.pr.checks', 'plugin_gitforge_gh_pr_checks'),
  (6, 'plugin_dev.neige.git-forge_gh.pr.merge', 'plugin_gitforge_gh_pr_merge'),
  (7, 'plugin_dev.neige.git-forge_gh.issue.view', 'plugin_gitforge_gh_issue_view'),
  (8, 'plugin_dev.neige.git-forge_gh.issue.close', 'plugin_gitforge_gh_issue_close'),
  (9, 'plugin_dev.neige.git-forge_gh.issue.comments', 'plugin_gitforge_gh_issue_comments'),
  (10, 'plugin_dev.neige.git-forge_gh.issue.comment', 'plugin_gitforge_gh_issue_comment')
),
scan(id, step, rest, output, previous) AS (
  SELECT id, 1,
         replace(replace(body, 'neige://plugin/dev.neige.calendar/', 'neige://plugin/calendar/'),
                 'neige://plugin/dev.neige.git-forge/', 'neige://plugin/gitforge/'),
         '', ''
    FROM track_recipes
   WHERE instr(body, 'plugin_dev.neige.git-forge_') > 0
      OR instr(body, 'neige://plugin/dev.neige.calendar/') > 0
      OR instr(body, 'neige://plugin/dev.neige.git-forge/') > 0
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
 WHERE scan.step = 11 AND track_recipes.id = scan.id
   AND track_recipes.body <> scan.rest;
