-- U13-A (#2843): schema only. Never classify, index or rewrite historical data.
-- No completion state exists until the separately reviewed atomic apply slice.
CREATE TABLE conversation_upgrade (
    singleton INTEGER PRIMARY KEY CHECK(singleton = 1),
    deployment_id TEXT NOT NULL CHECK(length(deployment_id) = 36),
    schema_sha256 TEXT NOT NULL CHECK(length(schema_sha256) = 64 AND schema_sha256 NOT GLOB '*[^0-9a-f]*'),
    protocol_version INTEGER NOT NULL CHECK(protocol_version = 1),
    state TEXT NOT NULL CHECK(state = 'required'),
    prepared_at_ms INTEGER NOT NULL CHECK(prepared_at_ms >= 0)
);
