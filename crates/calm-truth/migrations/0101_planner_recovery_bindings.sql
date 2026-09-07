-- #1501 F2-B: provider registration and immutable decision/call provenance.
-- Session/event IDs remain historical facts even after their rows disappear.
CREATE TABLE planner_recovery_threads (
  thread_id TEXT PRIMARY KEY NOT NULL CHECK(length(thread_id) BETWEEN 1 AND 512),
  track_id TEXT NOT NULL REFERENCES tracks(id) ON DELETE CASCADE,
  card_id TEXT NOT NULL,
  registered_at_ms INTEGER NOT NULL,
  UNIQUE(thread_id,track_id)
);
CREATE TABLE planner_recovery_issuances (
  id TEXT PRIMARY KEY NOT NULL,
  track_id TEXT NOT NULL REFERENCES tracks(id) ON DELETE CASCADE,
  session_id TEXT NOT NULL CHECK(length(session_id) BETWEEN 1 AND 512),
  thread_id TEXT NOT NULL,
  input_json TEXT NOT NULL CHECK(json_valid(input_json) AND length(input_json)<=4194304),
  actions_json TEXT NOT NULL CHECK(json_valid(actions_json) AND json_type(actions_json)='array' AND json_array_length(actions_json) BETWEEN 1 AND 128),
  created_at_ms INTEGER NOT NULL,
  UNIQUE(id,track_id,session_id,thread_id),
  FOREIGN KEY(thread_id,track_id) REFERENCES planner_recovery_threads(thread_id,track_id) ON DELETE CASCADE
);
CREATE TABLE planner_recovery_turns (
  thread_id TEXT NOT NULL,
  turn_id TEXT NOT NULL CHECK(length(turn_id) BETWEEN 1 AND 512),
  session_id TEXT NOT NULL,
  track_id TEXT NOT NULL REFERENCES tracks(id) ON DELETE CASCADE,
  issuance_id TEXT NOT NULL,
  confirmed_at_ms INTEGER NOT NULL,
  PRIMARY KEY(thread_id,turn_id),
  UNIQUE(thread_id,turn_id,session_id,track_id),
  FOREIGN KEY(issuance_id,track_id,session_id,thread_id) REFERENCES planner_recovery_issuances(id,track_id,session_id,thread_id) ON DELETE CASCADE
);
CREATE TABLE planner_recovery_calls (
  thread_id TEXT NOT NULL,
  turn_id TEXT NOT NULL,
  call_id TEXT NOT NULL CHECK(length(call_id) BETWEEN 1 AND 512),
  session_id TEXT NOT NULL,
  track_id TEXT NOT NULL REFERENCES tracks(id) ON DELETE CASCADE,
  tool TEXT NOT NULL,
  arguments_sha256 TEXT NOT NULL CHECK(length(arguments_sha256)=64),
  received_at_ms INTEGER NOT NULL,
  PRIMARY KEY(thread_id,turn_id,call_id),
  FOREIGN KEY(thread_id,turn_id,session_id,track_id) REFERENCES planner_recovery_turns(thread_id,turn_id,session_id,track_id) ON DELETE CASCADE
);
