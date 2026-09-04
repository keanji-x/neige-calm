-- Issue #1370 — Area-scoped defaults are creation preferences for new Tracks.
-- Existing Areas keep the current behavior: no template and a server-managed
-- Neige workspace. The exact attached working directory is stored as `cwd`
-- rather than an area_folder id because a Track may run in a descendant of an
-- Area's claimed folder, and the claim row cannot reconstruct that exact path.

ALTER TABLE areas ADD COLUMN default_template_id TEXT NULL;
ALTER TABLE areas ADD COLUMN default_cwd TEXT NULL
    CHECK (default_cwd IS NULL OR substr(default_cwd, 1, 1) = '/');
