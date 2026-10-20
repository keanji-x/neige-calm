-- The objective log is authoritative; snapshots are disposable caches.
-- Natural keys are the only identities: no hidden rowid may bypass insert guards.
CREATE TABLE objective_events (
    track_id TEXT NOT NULL REFERENCES tracks(id) ON DELETE CASCADE,
    seq INTEGER NOT NULL CHECK(typeof(seq)='integer' AND seq>=1),
    schema_version INTEGER NOT NULL CHECK(schema_version=1),
    kind TEXT NOT NULL CHECK(kind IN('definition_changed','approval_granted','requested','check_finished','revision_delivered','settled','closed','host_idle','usage')),
    objective_key TEXT,
    payload_json TEXT NOT NULL CHECK(json_valid(payload_json)),
    source_kind TEXT NOT NULL CHECK(source_kind IN('planner_call','intent_job','harness_turn')),
    source_key TEXT NOT NULL CHECK(length(source_key)>0),
    appended_at_ms INTEGER NOT NULL,
    PRIMARY KEY(track_id,seq),
    UNIQUE(track_id,source_kind,source_key,kind),
    CHECK(kind IS json_extract(payload_json,'$.event')),
    CHECK(objective_key IS (CASE kind
        WHEN 'definition_changed' THEN json_extract(payload_json,'$.objective.key')
        WHEN 'revision_delivered' THEN NULL WHEN 'host_idle' THEN NULL WHEN 'usage' THEN NULL
        ELSE json_extract(payload_json,'$.objective') END))
) WITHOUT ROWID;
-- BEFORE INSERT runs before REPLACE's implicit deletion, even with recursive
-- triggers disabled. Protect every unique identity before FK cascades can run.
CREATE TRIGGER objective_events_no_replace BEFORE INSERT ON objective_events
WHEN EXISTS(SELECT 1 FROM objective_events WHERE track_id=NEW.track_id AND (
    seq=NEW.seq OR (source_kind=NEW.source_kind AND source_key=NEW.source_key AND kind=NEW.kind)))
BEGIN SELECT RAISE(ABORT,'objective events are append-only'); END;
CREATE TRIGGER objective_events_no_update BEFORE UPDATE ON objective_events
BEGIN SELECT RAISE(ABORT,'objective events are append-only'); END;
-- FK actions run triggers even with recursive_triggers off. A deleted parent
-- is already absent when its cascade fires; only that deletion may remove log rows.
CREATE TRIGGER objective_events_no_delete BEFORE DELETE ON objective_events
WHEN EXISTS(SELECT 1 FROM tracks WHERE id=OLD.track_id)
BEGIN SELECT RAISE(ABORT,'objective events are append-only'); END;

CREATE TABLE objective_states (
    track_id TEXT PRIMARY KEY REFERENCES tracks(id) ON DELETE CASCADE,
    seq INTEGER NOT NULL CHECK(typeof(seq)='integer' AND seq>=1),
    state_version INTEGER NOT NULL,
    state_json TEXT NOT NULL CHECK(json_valid(state_json)),
    FOREIGN KEY(track_id,seq) REFERENCES objective_events(track_id,seq) ON DELETE CASCADE
) WITHOUT ROWID;
-- Existing snapshots advance through CAS UPDATE, never replacement insertion.
CREATE TRIGGER objective_states_no_replace BEFORE INSERT ON objective_states
WHEN EXISTS(SELECT 1 FROM objective_states WHERE track_id=NEW.track_id)
BEGIN SELECT RAISE(ABORT,'objective snapshot cannot be replaced'); END;
CREATE TABLE objective_intents (
    track_id TEXT NOT NULL REFERENCES tracks(id) ON DELETE CASCADE,
    event_seq INTEGER NOT NULL,
    ordinal INTEGER NOT NULL CHECK(typeof(ordinal)='integer' AND ordinal>=0),
    kind TEXT NOT NULL CHECK(kind IN('run_check','execute','notify')),
    intent_json TEXT NOT NULL CHECK(json_valid(intent_json) AND json_extract(intent_json,'$.intent') IS kind),
    effect_key TEXT NOT NULL CHECK(json_valid(effect_key)),
    standard_hash TEXT,
    target_session_id TEXT,
    status TEXT NOT NULL DEFAULT 'pending' CHECK(status IN('pending','dispatched','settled')),
    dispatched_at_ms INTEGER,
    settled_at_ms INTEGER,
    settlement_json TEXT CHECK(settlement_json IS NULL OR json_valid(settlement_json)),
    created_at_ms INTEGER NOT NULL,
    PRIMARY KEY(track_id,event_seq,ordinal),
    FOREIGN KEY(track_id,event_seq) REFERENCES objective_events(track_id,seq) ON DELETE CASCADE,
    UNIQUE(track_id,effect_key),
    CHECK((kind='run_check')=(standard_hash IS NOT NULL)),
    CHECK((kind='execute')=(target_session_id IS NOT NULL)),
    CHECK((status='settled')=(settled_at_ms IS NOT NULL)),
    CHECK((status='settled')=(settlement_json IS NOT NULL)),
    -- Settlement may happen directly from pending or after dispatch.
    CHECK(status<>'pending' OR dispatched_at_ms IS NULL),
    CHECK(status<>'dispatched' OR dispatched_at_ms IS NOT NULL)
) WITHOUT ROWID;
CREATE INDEX objective_intents_unsettled ON objective_intents(track_id,event_seq,ordinal) WHERE status<>'settled';
CREATE TRIGGER objective_intents_no_replace BEFORE INSERT ON objective_intents
WHEN EXISTS(SELECT 1 FROM objective_intents WHERE track_id=NEW.track_id AND (
    (event_seq=NEW.event_seq AND ordinal=NEW.ordinal) OR effect_key=NEW.effect_key))
BEGIN SELECT RAISE(ABORT,'objective intent cannot be replaced'); END;
CREATE TRIGGER objective_intents_identity_immutable BEFORE UPDATE ON objective_intents
WHEN NEW.track_id IS NOT OLD.track_id OR NEW.event_seq IS NOT OLD.event_seq
  OR NEW.ordinal IS NOT OLD.ordinal OR NEW.kind IS NOT OLD.kind
  OR NEW.intent_json IS NOT OLD.intent_json OR NEW.effect_key IS NOT OLD.effect_key
  OR NEW.standard_hash IS NOT OLD.standard_hash OR NEW.target_session_id IS NOT OLD.target_session_id
  OR NEW.created_at_ms IS NOT OLD.created_at_ms
BEGIN SELECT RAISE(ABORT,'immutable objective intent'); END;
CREATE TRIGGER objective_intents_status_monotonic BEFORE UPDATE ON objective_intents
WHEN NEW.status IS NOT OLD.status AND NOT (
    (OLD.status='pending' AND NEW.status IN('dispatched','settled'))
    OR (OLD.status='dispatched' AND NEW.status='settled'))
BEGIN SELECT RAISE(ABORT,'objective intent status cannot decrease'); END;
CREATE TRIGGER objective_intents_settlement_once BEFORE UPDATE ON objective_intents
WHEN OLD.settled_at_ms IS NOT NULL AND (
    NEW.settled_at_ms IS NOT OLD.settled_at_ms OR NEW.settlement_json IS NOT OLD.settlement_json)
BEGIN SELECT RAISE(ABORT,'objective intent settlement is write-once'); END;
