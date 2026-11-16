-- Older attached rows carried no explicit opt-out from on-demand isolation.
-- Set the flag only when creation chooses a source-only workspace.
ALTER TABLE tracks ADD COLUMN workspace_source_only INTEGER NOT NULL DEFAULT 0
    CHECK (workspace_source_only IN (0, 1));
