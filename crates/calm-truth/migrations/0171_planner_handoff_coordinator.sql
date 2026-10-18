-- A domain reservation only. OperationRuntime owns work, leases and progress.
CREATE TABLE planner_handoff_coordinators (
    operation_id TEXT PRIMARY KEY REFERENCES operations(id),
    track_id TEXT NOT NULL REFERENCES tracks(id) ON DELETE CASCADE,
    request_hash TEXT NOT NULL CHECK(length(request_hash)>0),
    active INTEGER NOT NULL DEFAULT 1 CHECK(active IN (0,1)),
    cancel_requested INTEGER NOT NULL DEFAULT 0 CHECK(cancel_requested IN (0,1))
);
CREATE UNIQUE INDEX planner_handoff_one_active_track
ON planner_handoff_coordinators(track_id) WHERE active=1;
CREATE TRIGGER planner_handoff_coordinator_identity
BEFORE UPDATE OF operation_id,track_id,request_hash ON planner_handoff_coordinators
BEGIN SELECT RAISE(ABORT,'Handoff reservation identity is immutable'); END;
CREATE TRIGGER planner_handoff_coordinator_no_rearm
BEFORE UPDATE OF active ON planner_handoff_coordinators
WHEN OLD.active=0 AND NEW.active=1
BEGIN SELECT RAISE(ABORT,'Handoff reservation cannot be rearmed'); END;
CREATE TRIGGER planner_handoff_coordinator_no_forget
BEFORE DELETE ON planner_handoff_coordinators
WHEN OLD.active=1
BEGIN SELECT RAISE(ABORT,'Handoff requires explicit recovery before deletion'); END;
CREATE TRIGGER planner_handoff_coordinator_track_delete
BEFORE DELETE ON tracks
WHEN EXISTS(SELECT 1 FROM planner_handoff_coordinators WHERE track_id=OLD.id AND active=1)
BEGIN SELECT RAISE(ABORT,'Track has an unresolved handoff'); END;
CREATE TRIGGER planner_handoff_coordinator_track_close
BEFORE UPDATE OF closed_at ON tracks
WHEN NEW.closed_at IS NOT NULL AND EXISTS(
    SELECT 1 FROM planner_handoff_coordinators WHERE track_id=OLD.id AND active=1)
BEGIN SELECT RAISE(ABORT,'Track has an unresolved handoff'); END;

-- Existing start compensation can restore an old session pointer. Bind original
-- approval to a durable pointer generation, never a wall-clock timestamp.
CREATE TABLE planner_handoff_card_generations (
    card_id TEXT PRIMARY KEY REFERENCES cards(id) ON DELETE CASCADE,
    generation INTEGER NOT NULL DEFAULT 0 CHECK(generation>=0)
);
INSERT INTO planner_handoff_card_generations(card_id) SELECT id FROM cards;
CREATE TRIGGER planner_handoff_card_generation_insert AFTER INSERT ON cards
BEGIN INSERT INTO planner_handoff_card_generations(card_id) VALUES(NEW.id); END;
CREATE TRIGGER planner_handoff_card_generation_update AFTER UPDATE OF session_id,role ON cards
WHEN NEW.session_id IS NOT OLD.session_id OR NEW.role IS NOT OLD.role
BEGIN UPDATE planner_handoff_card_generations SET generation=generation+1 WHERE card_id=NEW.id; END;
CREATE TABLE planner_handoff_track_generations (
    track_id TEXT PRIMARY KEY REFERENCES tracks(id) ON DELETE CASCADE,
    generation INTEGER NOT NULL DEFAULT 0 CHECK(generation>=0)
);
INSERT INTO planner_handoff_track_generations(track_id) SELECT id FROM tracks;
CREATE TRIGGER planner_handoff_track_generation_insert AFTER INSERT ON tracks
BEGIN INSERT INTO planner_handoff_track_generations(track_id) VALUES(NEW.id); END;
CREATE TRIGGER planner_handoff_track_generation_update AFTER UPDATE OF root_session_id,closed_at,purpose ON tracks
WHEN NEW.root_session_id IS NOT OLD.root_session_id OR NEW.closed_at IS NOT OLD.closed_at OR NEW.purpose IS NOT OLD.purpose
BEGIN UPDATE planner_handoff_track_generations SET generation=generation+1 WHERE track_id=NEW.id; END;
CREATE TRIGGER planner_handoff_coordinator_cancel_monotonic
BEFORE UPDATE OF cancel_requested ON planner_handoff_coordinators
WHEN NEW.cancel_requested<OLD.cancel_requested
BEGIN SELECT RAISE(ABORT,'Handoff cancellation cannot be withdrawn'); END;
