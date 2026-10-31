-- Persisted card.updated payload and track.report_edited body fields may use
-- the versioned private storage envelope. SQL metadata and public events retain
-- their existing format. Recording this migration fences startup by binaries
-- without the decoder; upgrades still require exclusive database ownership.
SELECT 1;
