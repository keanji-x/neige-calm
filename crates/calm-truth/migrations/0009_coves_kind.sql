-- Issue #175 — cove kind gate, hide the default-Today system cove.
--
-- `coves.kind` carries an authorization-style label that decides whether
-- a cove is part of the user's sidebar-visible workspace, or an internal
-- system-owned cove the kernel uses to host a structural entity (the
-- default Today terminal's home wave) without polluting the sidebar.
--
-- Two values today:
--
--   * 'user'   — default; every existing cove, every cove POSTed via
--                `POST /api/coves`. The kernel places no extra
--                restrictions on these; they show up in the sidebar,
--                in `GET /api/coves`, and behave as the only cove kind
--                the pre-#175 product knew about.
--   * 'system' — internal-only. The kernel mints exactly one of these
--                via `cove_create_system_tx`, accessed via the
--                singleton-style `POST /api/coves/system` upsert
--                endpoint. Hosts the default Today terminal's wave +
--                card. `GET /api/coves` filters these out by default
--                (opt-in via `?include_system=true`); the sidebar
--                consequently never renders them.
--
-- Existing rows default to 'user' (single-user app, pre-#175 history is
-- all user-created coves by construction — the issue is precisely that
-- there was no other kind before).

ALTER TABLE coves ADD COLUMN kind TEXT NOT NULL DEFAULT 'user';

-- Singleton invariant for the system cove. The upsert endpoint relies
-- on this as a backstop in case the application-level idempotency
-- races itself: at most one row with `kind = 'system'` can ever exist.
-- Partial index so the cost is proportional to the (always ≤ 1) system
-- cove count, not the whole table.
CREATE UNIQUE INDEX idx_coves_one_system
    ON coves(kind) WHERE kind = 'system';
