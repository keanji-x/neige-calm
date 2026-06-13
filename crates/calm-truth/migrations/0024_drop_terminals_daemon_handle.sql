-- #388 Phase 3c: daemon binary retired; daemon_handle no longer written
-- since Phase 3b. Drop the column. SQLite 3.35+ supports ALTER TABLE
-- DROP COLUMN; the dev/CI sqlite is new enough.
ALTER TABLE terminals DROP COLUMN daemon_handle;
