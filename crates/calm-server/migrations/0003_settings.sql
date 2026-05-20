-- App-global settings — a tiny KV table the Settings page writes through
-- and the codex spawn path reads from.
--
-- Why a dedicated table (not piggybacking on plugin_kv): settings here are
-- *kernel*-scoped, not owned by any plugin. Keeping them in their own table
-- avoids a fake plugin_id sentinel ("__kernel__") and keeps the surface
-- self-documenting.
--
-- The kernel never inspects the value semantics; it's `TEXT` and the route
-- (and the codex spawn reader) decides what each key means. First two keys
-- in use: `http_proxy`, `https_proxy`. Empty string means "delete this key"
-- on the write boundary — see `routes::settings`.

CREATE TABLE settings (
    key         TEXT    PRIMARY KEY,
    value       TEXT    NOT NULL,
    updated_at  INTEGER NOT NULL
);
