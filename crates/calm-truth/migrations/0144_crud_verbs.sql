-- #2087 B1c: Calendar and preview take the collection verbs `add`, `ls`, `set` and `rm`, and a
-- task verdict is the verb `accept` or `reject` instead of a `status` argument. No alias serves an
-- old name. Two stored values are read back by name and are rewritten once:
--   * the `tool` field of Planner transcript rows (`$.item.tool`), read by the fe history. The
--     plain renames map exactly. A stored `neige_calendar_update` becomes `neige_calendar_rm` when
--     its arguments carry `cancelled: true` and `neige_calendar_set` otherwise; a stored
--     `neige_task_verdict` becomes `neige_task_accept` or `neige_task_reject` by its `status`
--     argument, and one with a missing or unknown status keeps its name. Nothing else in params is
--     touched, so stored call arguments keep `id`, `until`, `key`, `cancelled` and `status`.
--   * the body of a track recipe, read by agents as instructions, through the same names. The scan
--     is 0141's: occurrences, identifier continuations unchanged, trailing sentence dots kept as
--     punctuation. Text has no arguments, so a name that split in two is written as both names.
--     Each changed row bumps `revision` and `updated_at`.
UPDATE harness_items
   SET params = json_set(params, '$.item.tool',
         CASE json_extract(params, '$.item.tool')
           WHEN 'neige_calendar_create' THEN 'neige_calendar_add'
           WHEN 'neige_calendar_list' THEN 'neige_calendar_ls'
           WHEN 'neige_calendar_update' THEN
             CASE WHEN json_type(params, '$.item.arguments.cancelled') = 'true'
                  THEN 'neige_calendar_rm' ELSE 'neige_calendar_set' END
           WHEN 'neige_preview_register' THEN 'neige_preview_add'
           WHEN 'neige_preview_unregister' THEN 'neige_preview_rm'
           WHEN 'neige_task_verdict' THEN
             CASE json_extract(params, '$.item.arguments.status')
               WHEN 'accepted' THEN 'neige_task_accept'
               WHEN 'rejected' THEN 'neige_task_reject'
             END
         END)
 WHERE json_valid(params)
   AND json_type(params, '$.item.tool') = 'text'
   AND (json_extract(params, '$.item.tool') IN (
          'neige_calendar_create', 'neige_calendar_list', 'neige_calendar_update',
          'neige_preview_register', 'neige_preview_unregister')
        OR (json_extract(params, '$.item.tool') = 'neige_task_verdict'
            AND json_type(params, '$.item.arguments.status') = 'text'
            AND json_extract(params, '$.item.arguments.status') IN ('accepted', 'rejected')));

WITH RECURSIVE
names(step, old, new) AS (VALUES
  (1, 'neige_calendar_create', 'neige_calendar_add'),
  (2, 'neige_calendar_list', 'neige_calendar_ls'),
  (3, 'neige_calendar_update', 'neige_calendar_set/neige_calendar_rm'),
  (4, 'neige_preview_register', 'neige_preview_add'),
  (5, 'neige_preview_unregister', 'neige_preview_rm'),
  (6, 'neige_task_verdict', 'neige_task_accept/neige_task_reject')
),
scan(id, step, rest, output, previous) AS (
  SELECT id, 1, body, '', '' FROM track_recipes
   WHERE instr(body, 'neige_calendar_') > 0
      OR instr(body, 'neige_preview_') > 0
      OR instr(body, 'neige_task_verdict') > 0
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
 WHERE scan.step = 7 AND track_recipes.id = scan.id
   AND track_recipes.body <> scan.rest;
