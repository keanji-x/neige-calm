-- Sync engine phase 1 — event log.
--
-- Append-only persistent record of every kernel write. Drives the WS replay
-- protocol (Scope D — not yet implemented), audit queries, and replay-based
-- regression fixtures. See `docs/sync-engine-design.md` §1.1.
--
-- Design notes:
--
--   * `INTEGER PRIMARY KEY AUTOINCREMENT` — strict monotonicity. Without
--     AUTOINCREMENT, sqlite reuses `rowid` after deletion; the cursor
--     protocol depends on never reusing an id. The small `sqlite_sequence`
--     bookkeeping cost is worth it.
--   * `payload TEXT` — same convention as `cards.payload` / `overlays.payload`.
--     Avoids depending on `jsonb` sqlite builds.
--   * `actor` is a string (`"user"`, `"kernel"`, `"plugin:<id>"`, `"ai:<id>"`),
--     declared, not authenticated (see design doc §1.1 disclaimer).
--   * `at` is the wall-clock timestamp; `id` is the ordering / cursor key.
--     Never mix the two.
--   * `correlation` threads multi-step mutations across events; populated
--     for plugin tool-call writes per design §9.
--   * No FK to entity tables — events outlive the rows they describe.

CREATE TABLE events (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    kind        TEXT    NOT NULL,            -- mirrors Event::serde tag, e.g. "wave.updated"
    payload     TEXT    NOT NULL,            -- JSON, the `data` field of the wire envelope
    actor       TEXT    NOT NULL,            -- "user", "ai:<agent_id>", "kernel", "plugin:<id>"
    at          INTEGER NOT NULL,            -- unix ms, matches model::now_ms()
    correlation TEXT                          -- optional request id for tracing/replay grouping
);
CREATE INDEX idx_events_kind ON events(kind);
CREATE INDEX idx_events_at   ON events(at);
