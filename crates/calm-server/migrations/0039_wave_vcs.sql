-- Wave VCS PR1 — content-addressed virtual wave tree snapshots.
--
-- The Rust backfill runs immediately after the sqlx migrator finishes because
-- the root commits need BLAKE3 hashes and the same canonical projection used
-- by the write hook.

CREATE TABLE wave_vcs_objects (
    hash       TEXT    PRIMARY KEY,
    kind       TEXT    NOT NULL,
    bytes      BLOB    NOT NULL,
    created_at INTEGER NOT NULL
);

CREATE TABLE wave_vcs_commits (
    hash                    TEXT PRIMARY KEY,
    wave_id                 TEXT NOT NULL,
    parent_hash             TEXT,
    tree_hash               TEXT NOT NULL,
    manifest_schema_version INTEGER NOT NULL,
    author                  TEXT,
    message                 TEXT,
    lifecycle               TEXT NOT NULL,
    event_id                INTEGER,
    created_at              INTEGER NOT NULL
);

CREATE TABLE wave_vcs_refs (
    wave_id          TEXT PRIMARY KEY,
    head_hash        TEXT NOT NULL,
    updated_event_id INTEGER,
    FOREIGN KEY (head_hash) REFERENCES wave_vcs_commits(hash)
);

CREATE INDEX idx_wave_vcs_commits_wave_id
    ON wave_vcs_commits(wave_id);

CREATE INDEX idx_wave_vcs_commits_event_id
    ON wave_vcs_commits(event_id) WHERE event_id IS NOT NULL;

CREATE INDEX idx_wave_vcs_objects_created_at
    ON wave_vcs_objects(created_at);
