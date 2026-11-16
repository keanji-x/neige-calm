-- An ACP generation may execute in more than one Track. Keep each attachment's
-- generation reachable until the execution owner has confirmed descendant exit.
CREATE TABLE acp_resident_workspace_bindings (
    execution_id TEXT NOT NULL REFERENCES workspace_executions(execution_id),
    worker_session_id TEXT NOT NULL,
    track_id TEXT NOT NULL,
    cwd TEXT NOT NULL,
    PRIMARY KEY (execution_id, worker_session_id)
);
CREATE INDEX acp_resident_workspace_worker
    ON acp_resident_workspace_bindings(worker_session_id);
CREATE INDEX acp_resident_workspace_track
    ON acp_resident_workspace_bindings(track_id);
