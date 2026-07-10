-- Issue #891 — bound-workflow input carried by the wave row.
--
-- Nullable + non-destructive (same pattern as 0059 workflow_id): rows that
-- existed before this migration stay NULL. Stored as TEXT (serde_json
-- serialization); the `POST /api/waves` route has already validated the
-- value against the bound descriptor's `input_schema`, so the writer stays
-- a pure mechanical insert.
ALTER TABLE waves ADD COLUMN workflow_input TEXT NULL;
