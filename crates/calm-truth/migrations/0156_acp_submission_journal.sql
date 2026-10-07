-- #2343: write once before prompt dispatch. Unknown submissions fence their session.
-- Only this backend can register managed ownership; provider identity alone is insufficient.
CREATE TABLE acp_managed_sessions (
  worker_session_id TEXT PRIMARY KEY REFERENCES worker_sessions(id) ON DELETE CASCADE,
  registration_digest TEXT NOT NULL
);
CREATE TABLE acp_submissions (
  worker_session_id TEXT NOT NULL REFERENCES worker_sessions(id) ON DELETE CASCADE,
  client_id TEXT NOT NULL,
  turn_id TEXT NOT NULL UNIQUE,
  thread_id TEXT NOT NULL,
  native_session_id TEXT NOT NULL,
  input_json TEXT NOT NULL CHECK(json_valid(input_json)),
  state TEXT NOT NULL CHECK(state IN ('sending','completed','unknown')),
  outcome_json TEXT CHECK(outcome_json IS NULL OR json_valid(outcome_json)),
  created_at_ms INTEGER NOT NULL,
  PRIMARY KEY(worker_session_id, client_id),
  CHECK((state = 'completed') = (outcome_json IS NOT NULL))
);
CREATE UNIQUE INDEX acp_one_unresolved_submission ON acp_submissions(worker_session_id)
  WHERE state IN ('sending','unknown');
