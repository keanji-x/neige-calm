-- Session ingress ownership augments the existing execution lease lifecycle.
CREATE TABLE native_session_ingresses (
 session_execution_id TEXT PRIMARY KEY REFERENCES workspace_leases(lease_id),
 terminal_id TEXT NOT NULL,
 card_id TEXT NOT NULL,
 track_id TEXT NOT NULL,
 canonical_cwd TEXT NOT NULL,
 socket_path TEXT NOT NULL UNIQUE,
 provider_socket_path TEXT NOT NULL,
 permissions_json TEXT NOT NULL,
 created_at_ms INTEGER NOT NULL
);
CREATE TABLE native_session_threads (
 session_execution_id TEXT NOT NULL REFERENCES native_session_ingresses(session_execution_id),
 thread_id TEXT NOT NULL,
 PRIMARY KEY(session_execution_id,thread_id)
);
CREATE TABLE native_session_executions (
 session_execution_id TEXT NOT NULL REFERENCES native_session_ingresses(session_execution_id),
 execution_id TEXT NOT NULL UNIQUE REFERENCES workspace_leases(lease_id),
 PRIMARY KEY(session_execution_id,execution_id)
);
CREATE TABLE native_session_unmatched_replies (
 session_execution_id TEXT NOT NULL REFERENCES native_session_ingresses(session_execution_id),
 frame_json TEXT NOT NULL,
 received_at_ms INTEGER NOT NULL
);
CREATE TABLE native_session_controls (
 id INTEGER PRIMARY KEY,
 session_execution_id TEXT NOT NULL REFERENCES native_session_ingresses(session_execution_id),
 execution_id TEXT NOT NULL REFERENCES workspace_leases(lease_id),
 method TEXT NOT NULL,
 params_json TEXT NOT NULL,
 reply_json TEXT NULL
);
CREATE TABLE native_session_thread_requests (
 id INTEGER PRIMARY KEY,
 session_execution_id TEXT NOT NULL REFERENCES native_session_ingresses(session_execution_id),
 request_json TEXT NOT NULL,
 reply_json TEXT NULL
);
-- Positive backend evidence for a client that never reached process launch.
CREATE TABLE native_session_client_observations (
 session_execution_id TEXT PRIMARY KEY REFERENCES native_session_ingresses(session_execution_id),
 proof TEXT NOT NULL CHECK(proof IN ('not-issued','unmanaged-client'))
);
