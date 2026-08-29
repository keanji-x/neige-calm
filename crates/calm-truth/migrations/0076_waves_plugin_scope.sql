-- Issue #1110 S4 — flatten per-wave plugin tool scope.
--
-- `plugin_scope` is the owning plugin id copied at create time. NULL means
-- unbound (historical All-tools behavior). `workflow_id` stays this slice;
-- S5 drops the workflow entity. Do not edit already-applied migrations.
ALTER TABLE waves ADD COLUMN plugin_scope TEXT NULL;

-- Existing bound waves would otherwise widen Only(owner) → All when
-- tool_visibility starts reading plugin_scope instead of workflow_id.
--
-- Walk only JSON that json_each / json_extract can consume: a string or
-- scalar `$.workflows` (or an array of non-objects) must yield no match,
-- not abort migrate with "malformed JSON". `json_type(json, path)` keeps
-- the JSON type without unwrapping a string value (the
-- `json_type(json_extract(...))` form does unwrap and then fails).
-- Element values that are not JSON objects are skipped via json_valid.
--
-- Unmatched bound rows (no owning plugin, weird manifest) copy
-- `workflow_id` as an unresolvable plugin id so the gate is fail-closed
-- None, not All.
UPDATE waves
SET plugin_scope = COALESCE(
    (
        SELECT p.id
        FROM plugins AS p
        WHERE json_valid(p.manifest)
          AND json_type(p.manifest, '$.workflows') = 'array'
          AND EXISTS (
            SELECT 1
            FROM json_each(
                CASE
                    WHEN json_valid(p.manifest)
                         AND json_type(p.manifest, '$.workflows') = 'array'
                    THEN json_extract(p.manifest, '$.workflows')
                    ELSE '[]'
                END
            ) AS wf
            WHERE json_extract(
                    CASE WHEN json_valid(wf.value) THEN wf.value ELSE '{}' END,
                    '$.id'
                  ) = waves.workflow_id
          )
        LIMIT 1
    ),
    workflow_id
)
WHERE workflow_id IS NOT NULL
  AND plugin_scope IS NULL;
