// Frontend ↔ backend compatibility check.
//
// `WEB_COMPAT_VERSION` is the frontend's view of the negotiated wire
// contract (REST + WS) it was compiled against. The backend ships the
// same integer as `WEB_COMPAT_VERSION` in
// `crates/calm-server/src/routes/version.rs` and exposes the minimum
// frontend it still accepts as `minWebCompatVersion` on `/api/version`.
//
// Compatibility rule:
//
//   frontend.WEB_COMPAT_VERSION >= server.minWebCompatVersion  → allow
//                              else                            → refuse
//
// A frontend below the server's minimum is force-refreshed by a
// hard-block modal (see `app/providers.tsx`). A frontend ahead of the
// server is fine — the server still understands the older contract; it
// just hasn't shipped the newer one yet.
//
// Both constants are bumped in lockstep across PRs by review discipline,
// not by code generation — see `docs/upgrade-stability.md` (Tier B).

/**
 * Frontend's view of the wire contract version this bundle was compiled
 * against. Bump in lockstep with the backend's `WEB_COMPAT_VERSION` in
 * `crates/calm-server/src/routes/version.rs` whenever a REST/WS contract
 * change makes older frontends incompatible.
 *
 * Version history:
 * * `1` — initial. Terminal protocol v1.
 * * `2` — terminal protocol v2 (issue #44). XtermView speaks the new
 *   ClientHello/ServerHello + RenderSnapshot/Patch framing.
 * * `3` — dispatcher request event rename (issue #581). Wire kinds
 *   `codex.job_requested` / `terminal.job_requested` are renamed to
 *   `*.worker_requested`. Old frontends' zod schemas reject the new
 *   kinds and would silently drop invalidation frames, so bump here.
 * * `4` — scheduler wire kinds (issue #644). Adds `plan.updated` and
 *   `task.dispatched` to the WS event union (backend
 *   `SYNC_EVENT_VERSION` bumped 2 → 3 in lockstep). Older frontends'
 *   zod schemas don't know the new discriminators and would silently
 *   drop plan/dispatch invalidation frames, so bump here.
 * * `5` — gate-result wire kind (issue #644 PR-C). Adds
 *   `task.gate_result` to the WS event union (backend
 *   `SYNC_EVENT_VERSION` bumped 3 → 4 in lockstep). Older frontends'
 *   zod schemas don't know the new discriminator and would silently
 *   drop gate-result invalidation frames, so bump here.
 *
 * See `docs/upgrade-stability.md` (Tier B — cross-process negotiation).
 */
export const WEB_COMPAT_VERSION = 5;

/**
 * Shape of the JSON document returned by `GET /api/version`. Kept here
 * (rather than reused from `generated.ts`) so the compat check has a
 * narrow, hand-maintained source of truth and isn't tangled with the
 * full OpenAPI surface during refactors.
 *
 * `dbInstanceId` is a UUID v4 minted once per server-process boot. The
 * client uses it to detect when the server's underlying sqlite DB has
 * been recreated (e.g. `make dev RESET_DB=1` or a fresh-migrations branch
 * swap) and busts its persisted React Query cache + WS event cursor on
 * mismatch — see `ServerCompatGate` in `app/providers.tsx`. Additive on
 * the wire; older frontends ignore it without consequence.
 */
export type ServerVersionInfo = {
  kernelVersion: string;
  /**
   * Diagnostic-only REST contract version. NOT used for compatibility
   * gating — the frontend ↔ backend compat boundary is enforced via
   * `minWebCompatVersion` for the web bundle, and via `syncEventVersion`
   * on a per-event-frame basis. Surfaced here so operators / dashboards
   * can read "what REST contract is the kernel claiming"; do NOT add a
   * frontend check against this string. (See issue #198, concern 3.)
   */
  apiVersion: string;
  /**
   * Maximum `eventVersion` the server stamps onto envelopes on `/api/events`.
   * The client uses this as the per-frame compatibility gate: a frame with
   * `eventVersion > syncEventVersion` is from a future protocol and must
   * be dropped WITHOUT advancing the replay cursor (so a later, compatible
   * frontend can still pick it up). Bumped in lockstep with
   * `SYNC_EVENT_VERSION` on the backend.
   */
  syncEventVersion: number;
  /**
   * Diagnostic-only MCP spec date for the kernel-as-MCP-server surface.
   * PR 1 of #396 moved the plugin-host MCP date to
   * `pluginMcpProtocolVersion`; do NOT use either MCP date as a frontend
   * hard gate.
   */
  mcpProtocolVersion: string;
  /**
   * Diagnostic-only MCP spec date advertised by the plugin host to plugin
   * processes. Split from `mcpProtocolVersion` in #396 PR 1.
   */
  pluginMcpProtocolVersion: string;
  /**
   * Frontend `WEB_COMPAT_VERSION` this server was built with. This is
   * diagnostic context; `minWebCompatVersion` remains the load-bearing
   * whole-bundle compatibility gate.
   */
  webCompatVersion: number;
  /**
   * Minimum frontend `WEB_COMPAT_VERSION` the running kernel still
   * considers wire-compatible. A frontend below this value is hard-blocked
   * by `ServerCompatGate` until the user refreshes. This is the load-
   * bearing whole-bundle compatibility gate (paired with
   * `syncEventVersion` for per-frame gating).
   */
  minWebCompatVersion: number;
  /**
   * Diagnostic control-wire version between `calm-server` and
   * `calm-proc-supervisor`.
   */
  supervisorControlVersion: number;
  buildSha: string | null;
  dbInstanceId: string;
};

/** Fetch the server's `/api/version` payload. Throws on non-2xx. */
export async function fetchServerVersion(): Promise<ServerVersionInfo> {
  const res = await fetch('/api/version', { credentials: 'include' });
  if (!res.ok) {
    throw new Error(`GET /api/version failed: ${res.status} ${res.statusText}`);
  }
  return (await res.json()) as ServerVersionInfo;
}

/**
 * Compare a frontend build's compat version against the server's minimum.
 * `frontend` defaults to the constant in this module so callers in the
 * app just pass the server payload.
 */
export function isCompatible(
  server: Pick<ServerVersionInfo, 'minWebCompatVersion'>,
  frontend: number = WEB_COMPAT_VERSION,
): boolean {
  return frontend >= server.minWebCompatVersion;
}
