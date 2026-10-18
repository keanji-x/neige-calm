SELECT id,
       json_extract(payload, '$.plugin_id'),
       json_extract(payload, '$.entity_kind'),
       json_extract(payload, '$.entity_id'),
       json_extract(payload, '$.kind'),
       payload
FROM events
WHERE kind = 'overlay.set' AND json_valid(payload) AND id IN (
    SELECT MAX(id) FROM events
    WHERE kind = 'overlay.set' AND id > ?1 AND id <= ?2 AND json_valid(payload)
    GROUP BY json_extract(payload, '$.plugin_id'),
             json_extract(payload, '$.entity_kind'),
             json_extract(payload, '$.entity_id'),
             json_extract(payload, '$.kind')
)
