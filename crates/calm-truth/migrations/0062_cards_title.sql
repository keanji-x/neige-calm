-- Card titles are first-class rather than stored in `payload`: kernel-owned
-- kinds validate payload schemas and would reject a new key, while a column is
-- typed and queryable. No backfill: NULL means unnamed, so the frontend falls
-- back to its per-kind default.
ALTER TABLE cards ADD COLUMN title TEXT NULL;
