-- Part of #2666, slice C4.0c: current durable provider-owner claim, not history.
ALTER TABLE worker_sessions ADD COLUMN provider_execution_json TEXT;
