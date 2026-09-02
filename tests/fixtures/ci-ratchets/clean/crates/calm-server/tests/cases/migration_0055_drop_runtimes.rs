// Legal regression setup: migration 0055's test recreates the dropped table
// so the migration can prove that it removes it.
const OLD_SCHEMA: &str = r#"
CREATE TABLE "runtimes" (id TEXT PRIMARY KEY);
CREATE INDEX old_runtime_idx ON "runtimes"(id);
INSERT INTO "runtimes" (id) VALUES ('runtime-1');
"#;
