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
 *
 * See `docs/upgrade-stability.md` (Tier B — cross-process negotiation).
 */
export const WEB_COMPAT_VERSION = 2;

/**
 * Shape of the JSON document returned by `GET /api/version`. Kept here
 * (rather than reused from `generated.ts`) so the compat check has a
 * narrow, hand-maintained source of truth and isn't tangled with the
 * full OpenAPI surface during refactors.
 */
export type ServerVersionInfo = {
  kernelVersion: string;
  apiVersion: string;
  syncEventVersion: number;
  mcpProtocolVersion: string;
  minWebCompatVersion: number;
  buildSha: string | null;
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
