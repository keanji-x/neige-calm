-- Terminal creation provenance is informational; placement never grants authority.
-- Keep immutable operation JSON/hashes and historical event snapshots untouched.
-- Only explicit standalone-operation evidence or task dispatch establishes old origin.
-- Missing flags, generic shells, old defaults, and unmatched history remain unrecorded.
CREATE TEMP TABLE terminal_presentation_upgrade (
    card_id TEXT PRIMARY KEY,
    source TEXT NOT NULL
);
INSERT INTO terminal_presentation_upgrade(card_id, source)
SELECT id, 'unrecorded' FROM cards WHERE kind='terminal';

WITH evidence AS (
    SELECT json_extract(tx_output_json, '$.result.id') AS card_id,
        CASE
            WHEN kind='terminal-worker' THEN 'task'
            WHEN kind='terminal-execution-create' THEN 'manual'
            WHEN kind='terminal-create' AND json_type(payload_json,'$.planner_hooks')='true' THEN 'agent'
            WHEN kind='terminal-create' AND json_type(payload_json,'$.planner_hooks')='false' THEN 'manual'
            ELSE 'unrecorded'
        END AS source
    FROM operations
    WHERE kind IN ('terminal-create','terminal-execution-create','terminal-worker')
      AND tx_output_json IS NOT NULL AND json_valid(tx_output_json)
), unique_source AS (
    SELECT card_id, MIN(source) AS source FROM evidence
    WHERE card_id IS NOT NULL AND source <> 'unrecorded'
    GROUP BY card_id HAVING COUNT(DISTINCT source)=1
)
UPDATE terminal_presentation_upgrade
SET source=COALESCE((SELECT source FROM unique_source WHERE unique_source.card_id=terminal_presentation_upgrade.card_id),'unrecorded');

UPDATE cards SET payload=json_set(
    CASE WHEN json_type(payload)='object' THEN payload ELSE '{}' END,
    '$.terminal_presentation', json_object(
        'source', (SELECT source FROM terminal_presentation_upgrade WHERE card_id=cards.id),
        'in_workbench', json(CASE WHEN (SELECT source FROM terminal_presentation_upgrade WHERE card_id=cards.id)='manual' THEN 'true' ELSE 'false' END)
    )
) WHERE kind='terminal';
DROP TABLE terminal_presentation_upgrade;

-- Before this reservation, clients could write the same key on any card kind.
-- Remove that untrusted value so a later kind transition mints Unrecorded.
UPDATE cards SET payload=json_remove(payload,'$.terminal_presentation')
WHERE kind<>'terminal' AND json_type(payload)='object'
  AND json_type(payload,'$.terminal_presentation') IS NOT NULL;
