SELECT id,
       json_extract(payload, '$.plugin_id'),
       json_extract(payload, '$.entity_kind'),
       json_extract(payload, '$.entity_id'),
       json_extract(payload, '$.kind')
FROM events
WHERE kind = 'overlay.set' AND id > ?1 AND id <= ?2 AND at < ?3 AND json_valid(payload)
ORDER BY id
LIMIT ?4
