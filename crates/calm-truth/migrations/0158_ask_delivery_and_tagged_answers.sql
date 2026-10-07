-- Planner approval via blocking asks (#2348). `ask.requested` gains a required `delivery`
-- (`wake` | `hold`) and every `ask.answered` answer becomes tagged (`{"option": i}` or
-- `{"text": s}`). Every row written before this migration is a `wake` ask whose answers were
-- typed text, so the backfill is fixed: no row is looked up and no id or count is assumed.
--
-- Guards follow `0094`: `events.payload` has no `json_valid` CHECK and `json_extract` on a
-- non-JSON body aborts the migration, so validity is ordered by a `CASE`, not an `AND` sibling.

-- 1. `ask.requested`: a row without `delivery` is a `wake` ask.
UPDATE events
   SET payload = json_set(payload, '$.delivery', 'wake')
 WHERE kind = 'ask.requested'
   AND CASE WHEN json_valid(payload)
            THEN json_type(payload, '$.delivery') IS NULL
       END;

-- 2. `ask.answered`: an array of bare strings becomes the same strings tagged `text`, in order.
--    An array holding anything but strings is left alone, so a rerun changes nothing.
UPDATE events
   SET payload = json_set(
                   payload,
                   '$.answers',
                   json((SELECT json_group_array(json_object('text', a.value))
                           FROM json_each(events.payload, '$.answers') AS a)))
 WHERE kind = 'ask.answered'
   AND CASE WHEN json_valid(payload)
            THEN json_type(payload, '$.answers') = 'array'
                 AND NOT EXISTS (SELECT 1 FROM json_each(events.payload, '$.answers') AS a
                                  WHERE a.type <> 'text')
       END;

-- 3. The ask kinds carry the new shapes; `ask.withdrawn` is new.
UPDATE events SET event_version = 27 WHERE kind IN ('ask.requested', 'ask.answered', 'ask.withdrawn');
