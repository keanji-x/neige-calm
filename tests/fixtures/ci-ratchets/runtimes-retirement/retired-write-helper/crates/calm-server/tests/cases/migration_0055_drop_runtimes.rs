// Legal regression setup excluded from both repository ratchets where needed.
const OLD_SCHEMA: &str = r#"
CREATE TABLE "runtimes" (id TEXT PRIMARY KEY);
INSERT INTO "runtimes" (id) VALUES ('runtime-1');
"#;
