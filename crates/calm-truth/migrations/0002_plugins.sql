-- M3 plugin tables — Slice A.
--
-- The `plugins` table already exists from 0001_init.sql. This migration adds
-- the two sibling tables that hang off it:
--
--   * `plugin_tokens` — one row per plugin, holds the SHA-256 of the
--                       per-process auth token plus its expiry. Slice H
--                       writes here; Slice B reads on `initialize` to verify
--                       the echo. Hash is stored hex-encoded TEXT (64 chars).
--
--   * `plugin_kv`     — per-plugin namespaced key/value store, surfaced to
--                       plugins via `neige.kv.*` callbacks in Slice C.
--                       Composite PK `(plugin_id, key)` mirrors the trait's
--                       per-plugin namespacing — there is no global key path.
--
-- Both cascade on plugin delete so uninstall (Slice D) doesn't leave orphans.

CREATE TABLE plugin_tokens (
    plugin_id     TEXT    PRIMARY KEY
                          REFERENCES plugins(id) ON DELETE CASCADE,
    hashed_token  TEXT    NOT NULL,
    expires_at    INTEGER NOT NULL
);

CREATE TABLE plugin_kv (
    plugin_id   TEXT    NOT NULL
                        REFERENCES plugins(id) ON DELETE CASCADE,
    key         TEXT    NOT NULL,
    value       TEXT    NOT NULL,               -- JSON-encoded, opaque
    updated_at  INTEGER NOT NULL,
    PRIMARY KEY (plugin_id, key)
);
CREATE INDEX idx_plugin_kv_prefix ON plugin_kv(plugin_id, key);
