-- #2104 K1 — which Track's Planner created this Track with `neige_track_add`, and under which
-- key. A created Track is an ordinary top-level Track: `parent_track_id` stays NULL and the tree
-- budget is not charged, so this provenance needs columns of its own.
--
-- `creator_key` is the raw `idempotency_key` the creator passed, not the prefixed binding key in
-- `track_create_idempotency`; plugins read both columns through `_meta["dev.neige/track"]`.
--
-- No `REFERENCES`, for 0085's reason: the creator may be deleted later, and the id stays as a
-- dangling-but-truthful record of who created this Track.
--
-- Additive, like 0085: rebuilding `tracks` would mean reproducing every historical partial index
-- and CHECK constraint.
ALTER TABLE tracks ADD COLUMN creator_track_id TEXT;

-- Both columns or neither. The constraint is named because `tracks` carries more than one CHECK
-- and SQLite puts the name into the error text; `the_database_refuses_half_a_creator_provenance`
-- asserts on it.
ALTER TABLE tracks ADD COLUMN creator_key TEXT
  CONSTRAINT track_creator_is_whole
  CHECK ((creator_track_id IS NULL) = (creator_key IS NULL));

-- The open-created-Track cap counts a creator's rows inside every `neige_track_add` mint.
CREATE INDEX idx_tracks_creator_track_id ON tracks(creator_track_id)
  WHERE creator_track_id IS NOT NULL;
