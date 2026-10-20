-- Creation-time facts only; existing tracks deliberately receive no capabilities.
CREATE TABLE track_capabilities (
    track_id TEXT NOT NULL REFERENCES tracks(id) ON DELETE CASCADE,
    capability TEXT NOT NULL CHECK(capability IN('objectives')),
    PRIMARY KEY(track_id,capability)
) WITHOUT ROWID;
CREATE TRIGGER track_capabilities_no_replace BEFORE INSERT ON track_capabilities
WHEN EXISTS(SELECT 1 FROM track_capabilities WHERE track_id=NEW.track_id AND capability=NEW.capability)
BEGIN SELECT RAISE(ABORT,'track capabilities are immutable'); END;
CREATE TRIGGER track_capabilities_no_update BEFORE UPDATE ON track_capabilities
BEGIN SELECT RAISE(ABORT,'track capabilities are immutable'); END;
-- Only deletion of the parent Track may remove its capabilities.
CREATE TRIGGER track_capabilities_no_delete BEFORE DELETE ON track_capabilities
WHEN EXISTS(SELECT 1 FROM tracks WHERE id=OLD.track_id)
BEGIN SELECT RAISE(ABORT,'track capabilities are immutable'); END;
