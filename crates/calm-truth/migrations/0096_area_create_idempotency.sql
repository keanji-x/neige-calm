-- #1500 A2: a creation identity outlives its Area, so a retry after deletion
-- fails closed instead of minting a second Area. No foreign-key cascade.
CREATE TABLE area_create_idempotency (
    idempotency_key TEXT PRIMARY KEY NOT NULL,
    request_fingerprint TEXT NOT NULL,
    area_id TEXT NOT NULL UNIQUE
);

-- These rows are permanent identity evidence, never retention candidates.
CREATE TRIGGER area_create_idempotency_no_delete
BEFORE DELETE ON area_create_idempotency
BEGIN
    SELECT RAISE(ABORT, 'Area creation identities are permanent');
END;

CREATE TRIGGER area_create_idempotency_no_update
BEFORE UPDATE ON area_create_idempotency
BEGIN
    SELECT RAISE(ABORT, 'Area creation identities are immutable');
END;
