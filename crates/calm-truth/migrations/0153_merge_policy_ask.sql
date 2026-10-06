-- #2209 retired ratify: the Planner asks the user with neige_user_ask, so the dev merge policy that
-- asks before merging is `ask`, no longer `hold-for-ratify`. The value is read back by name only from
-- `tracks.template_input` (the Planner's bound input, checked against the template's enum), so only
-- that is rewritten, and only its `merge_policy`. The built-in manifest copy in `plugins` is
-- reinstalled at boot. History stores (events, transcripts, task contexts, operations, the template
-- text a Planner card was started with) are not rewritten.
UPDATE tracks
   SET template_input = json_set(template_input, '$.merge_policy', 'ask')
 WHERE json_valid(template_input)
   AND json_type(template_input, '$.merge_policy') = 'text'
   AND json_extract(template_input, '$.merge_policy') = 'hold-for-ratify';
