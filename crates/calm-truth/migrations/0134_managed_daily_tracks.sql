-- Kernel-owned creation identities and explicit workspace report read grants.
CREATE TABLE managed_track_identities (
    owner TEXT NOT NULL,
    identity TEXT NOT NULL,
    track_id TEXT NOT NULL UNIQUE REFERENCES tracks(id) ON DELETE RESTRICT,
    report_read_scope TEXT NOT NULL CHECK (report_read_scope IN ('area', 'workspace')),
    report_time_zone TEXT NOT NULL,
    tool_policy TEXT NOT NULL CHECK (tool_policy IN ('standard', 'reports')),
    kernel_controls_lifecycle INTEGER NOT NULL CHECK (kernel_controls_lifecycle IN (0, 1)),
    PRIMARY KEY (owner, identity)
);
CREATE INDEX idx_events_report_day ON events(kind, at, id)
    WHERE kind = 'track.report_edited';
