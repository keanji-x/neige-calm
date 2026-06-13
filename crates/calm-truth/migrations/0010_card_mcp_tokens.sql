-- Wave-as-Actor PR7a (#136): per-card MCP tokens.
--
-- The raw 64-char hex token is held only in the codex daemon's env as
-- `NEIGE_MCP_TOKEN` (passed at child-spawn time by the kernel). The kernel
-- stores SHA-256(token) here; verification compares via
-- `subtle::ConstantTimeEq` to avoid timing leaks.
--
-- Lifecycle:
--   * Row minted inside `card_with_codex_create_tx` when the new card's
--     role is Spec or Worker. Plain cards skip this entirely.
--   * FK ON DELETE CASCADE so card deletion drops the token row in the
--     same statement.
--   * The raw token is unrecoverable from the hash — a kernel restart
--     does NOT re-issue tokens; the codex daemon must restart too so it
--     receives a fresh one. (Same security boundary as plugin tokens.)

CREATE TABLE card_mcp_tokens (
    card_id      TEXT    PRIMARY KEY REFERENCES cards(id) ON DELETE CASCADE,
    hashed_token TEXT    NOT NULL,
    created_at   INTEGER NOT NULL
);

-- Lookup-by-hash for the MCP server's handshake auth. The presented token
-- is hashed first, then we SELECT by `hashed_token` to recover the binding
-- card id. Indexed because every initialize hits this path.
CREATE INDEX idx_card_mcp_tokens_hashed
    ON card_mcp_tokens(hashed_token);
