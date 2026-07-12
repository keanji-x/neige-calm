-- Issue #951, Slice D: cache the normalized Git origin identity at attach time.
-- Existing rows intentionally remain unresolved until explicitly refreshed.
ALTER TABLE cove_folders ADD COLUMN repo_identity TEXT;
ALTER TABLE cove_folders ADD COLUMN repo_identity_probed_at INTEGER;
