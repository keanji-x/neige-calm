-- PR publication belongs to the development component. Migration 0134 first
-- normalizes the 12 historical calm.track.publish calls observed on 4140.
-- Preserve the transcript envelope, arguments and all unrelated tool names.
UPDATE harness_items
   SET params = json_set(params, '$.item.tool', 'neige.dev.publish')
 WHERE json_valid(params)
   AND json_extract(params, '$.item.tool') = 'neige.track.publish';
