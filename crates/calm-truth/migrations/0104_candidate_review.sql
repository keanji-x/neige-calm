-- Acceptance is the existing Planner decision, not a second accepted flag.
-- Only the verdict transaction writes this provenance receipt with the actual event ID.
CREATE TABLE task_candidate_decisions (
  event_id INTEGER PRIMARY KEY NOT NULL REFERENCES events(id),
  track_id TEXT NOT NULL REFERENCES tracks(id) ON DELETE CASCADE,
  producer_attempt_id TEXT NOT NULL,
  event_json TEXT NOT NULL CHECK(json_valid(event_json))
);
CREATE TRIGGER task_candidate_decision_immutable BEFORE UPDATE ON task_candidate_decisions
BEGIN SELECT RAISE(ABORT, 'candidate decision identity is immutable'); END;
CREATE TABLE task_candidate_decision_bindings (
  attempt_id TEXT PRIMARY KEY NOT NULL REFERENCES task_candidate_input_bindings(attempt_id) ON DELETE CASCADE,
  decision_event_id INTEGER NOT NULL REFERENCES task_candidate_decisions(event_id)
);
CREATE TRIGGER task_candidate_decision_binding_immutable BEFORE UPDATE ON task_candidate_decision_bindings
BEGIN SELECT RAISE(ABORT, 'candidate decision binding is immutable'); END;
