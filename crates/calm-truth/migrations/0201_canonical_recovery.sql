-- Recovery evidence is independent of delivery and never supplies a native END.
CREATE TABLE acp_normal_joins (
    worker_session_id TEXT NOT NULL REFERENCES worker_sessions(id) ON DELETE CASCADE,
    client_id TEXT NOT NULL,
    evidence_json TEXT NOT NULL CHECK(json_valid(evidence_json)),
    PRIMARY KEY(worker_session_id, client_id),
    FOREIGN KEY(worker_session_id,client_id) REFERENCES acp_submissions(worker_session_id,client_id) ON DELETE CASCADE
);
CREATE TABLE conversation_abandonments (
    card_id TEXT NOT NULL REFERENCES cards(id) ON DELETE CASCADE,
    principal TEXT NOT NULL,
    request_key TEXT NOT NULL,
    request_json TEXT NOT NULL CHECK(json_valid(request_json)),
    old_worker TEXT NOT NULL REFERENCES worker_sessions(id) ON DELETE CASCADE,
    new_worker TEXT NOT NULL REFERENCES worker_sessions(id) ON DELETE CASCADE,
    created_at_ms INTEGER NOT NULL,
    PRIMARY KEY(card_id,principal,request_key),
    UNIQUE(old_worker)
);
