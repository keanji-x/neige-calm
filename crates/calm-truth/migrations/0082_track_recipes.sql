-- #1292 S1 — user-defined wave recipes.
--
-- A recipe is a saved `TrackReportPayload`: a title (which doubles as the
-- report summary) plus a report body whose `neige-block` fences ARE its
-- tasks. It is deliberately NOT a track — #1300 removed "template = a hidden
-- wave" because that shape cost seven "this wave is special" exceptions
-- across unrelated subsystems plus a kernel write that impersonated the
-- user. Recipes carry none of that: nothing schedules them, nothing lists
-- them among tracks, and every byte in one was written by the user.
--
-- `revision` is the optimistic-lock anchor and is NOT `updated_at`: a wall
-- clock is not a version. Writers do
-- `UPDATE ... WHERE id = ?1 AND revision = ?2`, which validates and bumps
-- in one statement. `updated_at` is display-only.
CREATE TABLE track_recipes (
  id         TEXT PRIMARY KEY,
  title      TEXT NOT NULL,
  body       TEXT NOT NULL,
  revision   INTEGER NOT NULL,
  created_at INTEGER NOT NULL,
  updated_at INTEGER NOT NULL
);
