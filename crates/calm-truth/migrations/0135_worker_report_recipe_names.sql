-- #2053: recipes are executable instructions, unlike recorded transcript calls.
-- Scan occurrences, not individual characters, so long recipes remain inexpensive.
-- Identifier continuations stay unchanged; trailing sentence dots are punctuation.
-- Released migrations and historical calls keep the names they originally used.
WITH RECURSIVE
names(step, old, new) AS (VALUES
  (1, 'neige.task.complete', 'neige.task.report_success'),
  (2, 'neige.task.fail', 'neige.task.report_failure'),
  (3, 'task-completed', 'task-report-success'),
  (4, 'task-failed', 'task-report-failure')
),
scan(id, step, rest, output, previous) AS (
  SELECT id, 1, body, '', '' FROM track_recipes
   WHERE instr(body, 'neige.task.complete') > 0
      OR instr(body, 'neige.task.fail') > 0
      OR instr(body, 'task-completed') > 0
      OR instr(body, 'task-failed') > 0
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
 WHERE scan.step = 5 AND track_recipes.id = scan.id
   AND track_recipes.body <> scan.rest;
