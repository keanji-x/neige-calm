-- One question channel (#2209): `ask.requested` and `ask.answered` are new event kinds.
-- Historical `ratify.*` rows keep their version and still decode.
UPDATE events SET event_version = 26 WHERE kind IN ('ask.requested', 'ask.answered');
