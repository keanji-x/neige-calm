-- Assistant approval modes used to be unconditionally Never. Preserve that
-- effective authorization when this stored field becomes active, including
-- historical stray Ask/Full values. Provider eligibility stays with the binding
-- decoder; this one-time migration does not introduce another provider roster.
UPDATE cards
   SET payload = json_set(payload, '$.permission_mode', 'never')
 WHERE kind = 'codex'
   AND role = 'assistant'
   AND CASE WHEN json_valid(payload)
            THEN json_type(payload) = 'object'
             AND json_extract(payload, '$.harness_profile') = 'assistant'
            ELSE 0
       END;
