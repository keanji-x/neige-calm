-- User decisions are per Track. Keep denied revisions to invalidate older questions.
CREATE TABLE track_plugin_access (
    track_id TEXT NOT NULL REFERENCES tracks(id) ON DELETE CASCADE,
    plugin_id TEXT NOT NULL,
    granted INTEGER NOT NULL CHECK(granted IN (0, 1)),
    manifest_sha256 TEXT NOT NULL,
    revision INTEGER NOT NULL CHECK(revision > 0),
    PRIMARY KEY(track_id, plugin_id)
);
