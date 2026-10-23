-- Evaluator identity belongs to the intent; Report continues to mint block ids.
CREATE TABLE objective_check_jobs (
    track_id TEXT NOT NULL,
    event_seq INTEGER NOT NULL,
    ordinal INTEGER NOT NULL,
    task_key TEXT NOT NULL,
    block_payload_hash TEXT NOT NULL,
    PRIMARY KEY(track_id,event_seq,ordinal),
    FOREIGN KEY(track_id,event_seq,ordinal)
        REFERENCES objective_intents(track_id,event_seq,ordinal) ON DELETE CASCADE,
    UNIQUE(track_id,task_key)
) WITHOUT ROWID;
CREATE TRIGGER objective_check_jobs_no_replace BEFORE INSERT ON objective_check_jobs
WHEN EXISTS(SELECT 1 FROM objective_check_jobs WHERE track_id=NEW.track_id AND (
    (event_seq=NEW.event_seq AND ordinal=NEW.ordinal) OR task_key=NEW.task_key))
BEGIN SELECT RAISE(ABORT,'objective check jobs are append-only'); END;
CREATE TRIGGER objective_check_jobs_no_update BEFORE UPDATE ON objective_check_jobs
BEGIN SELECT RAISE(ABORT,'objective check jobs are append-only'); END;
CREATE TRIGGER objective_check_jobs_no_delete BEFORE DELETE ON objective_check_jobs
WHEN EXISTS(SELECT 1 FROM tracks WHERE id=OLD.track_id)
BEGIN SELECT RAISE(ABORT,'objective check jobs are append-only'); END;
