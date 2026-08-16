-- Issue #1109: drop the write-only `repo_identity` subsystem on cove_folders.
--
-- Both columns were added by migration 0063 (issue #951 slice D) to cache the
-- normalized Git origin at attach time. Nothing in the workspace ever compared,
-- branched on, or matched either value -- they were written, SELECTed, and
-- mapped into the DTO, then dropped on the floor. The refresh primitive
-- (`cove_folder_refresh_repo_identity`) never gained a caller: no HTTP route,
-- no MCP tool.
--
-- Requires SQLite 3.35+ for ALTER TABLE ... DROP COLUMN. Neither column is
-- referenced by an index, view, trigger, generated column, or partial-index
-- predicate (the only index on cove_folders is idx_cove_folders_cove on
-- cove_id, from migration 0015), so the plain DROP COLUMN form applies.
ALTER TABLE cove_folders DROP COLUMN repo_identity;
ALTER TABLE cove_folders DROP COLUMN repo_identity_probed_at;
