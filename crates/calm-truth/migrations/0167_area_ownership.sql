-- Explicit one-time adoption from the released single-user contract. Profile
-- reconciliation is a startup responsibility, never an ownership repair sweep.
CREATE TABLE application_users (
    id TEXT PRIMARY KEY NOT NULL CHECK(length(id) > 0),
    display_name TEXT NOT NULL,
    created_at INTEGER NOT NULL
);
INSERT INTO application_users (id, display_name, created_at)
VALUES ('local-owner', 'Owner', CAST(strftime('%s', 'now') AS INTEGER) * 1000);

CREATE TABLE area_members (
    area_id TEXT NOT NULL REFERENCES areas(id) ON DELETE CASCADE,
    user_id TEXT NOT NULL REFERENCES application_users(id) ON DELETE RESTRICT,
    role TEXT NOT NULL CHECK(role IN ('owner', 'member')),
    joined_at INTEGER NOT NULL,
    PRIMARY KEY (area_id, user_id)
);
CREATE UNIQUE INDEX area_one_owner ON area_members(area_id) WHERE role = 'owner';
CREATE INDEX area_members_by_user ON area_members(user_id, area_id);
CREATE TRIGGER area_members_user_area_only
BEFORE INSERT ON area_members
WHEN (SELECT kind FROM areas WHERE id = NEW.area_id) != 'user'
BEGIN
    SELECT RAISE(ABORT, 'System Areas cannot have human members');
END;
CREATE TRIGGER area_members_user_area_only_update
BEFORE UPDATE OF area_id ON area_members
WHEN (SELECT kind FROM areas WHERE id = NEW.area_id) != 'user'
BEGIN
    SELECT RAISE(ABORT, 'System Areas cannot have human members');
END;
INSERT INTO area_members (area_id, user_id, role, joined_at)
SELECT id, 'local-owner', 'owner', created_at FROM areas WHERE kind = 'user';

-- Permanent identities survive deleted Areas. Preserve the original key and
-- request fingerprint byte-for-byte in the original creator's namespace.
DROP TRIGGER area_create_idempotency_no_delete;
DROP TRIGGER area_create_idempotency_no_update;
ALTER TABLE area_create_idempotency RENAME TO area_create_idempotency_legacy;
CREATE TABLE area_create_idempotency (
    creator_user_id TEXT NOT NULL REFERENCES application_users(id) ON DELETE RESTRICT,
    idempotency_key TEXT NOT NULL,
    request_fingerprint TEXT NOT NULL,
    area_id TEXT NOT NULL UNIQUE,
    PRIMARY KEY (creator_user_id, idempotency_key)
);
INSERT INTO area_create_idempotency (creator_user_id, idempotency_key, request_fingerprint, area_id)
SELECT 'local-owner', idempotency_key, request_fingerprint, area_id FROM area_create_idempotency_legacy;
DROP TABLE area_create_idempotency_legacy;
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
