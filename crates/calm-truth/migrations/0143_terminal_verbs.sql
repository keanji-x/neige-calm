-- #2087 B1b: the terminal's anchored read is `read`, as `report_read` is, and the lookup of a
-- terminal's worker is `show`. No alias serves an old name. Two stored values are read back by
-- name and are rewritten once:
--   * the `tool` field of Planner transcript rows (`$.item.tool`), read by the fe history. Each
--     old name maps exactly to its new name; nothing else in params is touched, so stored call
--     arguments keep `observe` and `request_id` as they were sent.
--   * the body of a track recipe, read by agents as instructions, through the same names. The scan
--     is 0141's: occurrences, identifier continuations unchanged, trailing sentence dots kept as
--     punctuation. Each changed row bumps `revision` and `updated_at`.
UPDATE harness_items
   SET params = json_set(params, '$.item.tool',
         CASE json_extract(params, '$.item.tool')
           WHEN 'neige_terminal_observe' THEN 'neige_terminal_read'
           WHEN 'neige_terminal_resolve' THEN 'neige_terminal_show'
         END)
 WHERE json_valid(params)
   AND json_type(params, '$.item.tool') = 'text'
   AND json_extract(params, '$.item.tool') IN ('neige_terminal_observe', 'neige_terminal_resolve');

WITH RECURSIVE
names(step, old, new) AS (VALUES
  (1, 'neige_terminal_observe', 'neige_terminal_read'),
  (2, 'neige_terminal_resolve', 'neige_terminal_show')
),
scan(id, step, rest, output, previous) AS (
  SELECT id, 1, body, '', '' FROM track_recipes
   WHERE instr(body, 'neige_terminal_observe') > 0
      OR instr(body, 'neige_terminal_resolve') > 0
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
 WHERE scan.step = 3 AND track_recipes.id = scan.id
   AND track_recipes.body <> scan.rest;
