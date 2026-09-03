-- #1292 S3 — where a track came from, when it came from a user recipe.
--
-- Instantiation (S2) is a value copy: the recipe can be edited or deleted
-- afterwards and the track keeps its snapshot. That is exactly why the origin
-- has to be recorded on the track row itself — there is nothing left to
-- derive it from once the recipe moves on.
--
-- `recipe_id` is deliberately NOT a REFERENCES: a deleted recipe must leave
-- its tracks readable, and the id stays as a dangling-but-truthful record of
-- what the track was built from.
--
-- Deliberately additive, like 0071: rebuilding `tracks` would mean
-- reproducing every historical partial index and CHECK constraint.
ALTER TABLE tracks ADD COLUMN recipe_id TEXT;

-- Both columns or neither. "Built from recipe X, at no known version" and
-- "built at version 3, of nothing" are not states this system has a reading
-- for, so the database refuses them instead of the application agreeing not
-- to write them. The cross-column CHECK rides on the second ADD COLUMN
-- because that is the point at which both names exist.
--
-- The constraint is named because `tracks` carries more than one CHECK — 0071
-- added one that today reads `parent_track_id IS NULL OR parent_track_id <> id`
-- — and SQLite puts the name into the error text
-- (`CHECK constraint failed: track_recipe_origin_is_whole`). Without the name
-- a test asserting on `"CHECK constraint failed"` is satisfied by any of them.
-- `the_database_refuses_half_a_provenance` asserts on this name.
ALTER TABLE tracks ADD COLUMN recipe_revision INTEGER
  CONSTRAINT track_recipe_origin_is_whole
  CHECK ((recipe_id IS NULL) = (recipe_revision IS NULL));
