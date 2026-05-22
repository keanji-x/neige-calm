// Per-test reset hook for the `a11y` Playwright project.
//
// The replay binary booted by `_setup/replay-server.setup.ts` serves
// every test in the `a11y` project from a single in-memory kernel. Without
// a between-tests reset, per-test mutations (new waves, new cards, rename
// edits, view-mode toggles, …) accumulate across the suite and cause
// previously-green specs to flake when their predicates collide with state
// seeded by an earlier spec.
//
// `POST /dev/reset` (declared in `crates/calm-server/src/bin/replay.rs`)
// wipes every row from the in-memory repo and reseeds the original
// fixture's event stream, restoring the "fresh boot" starting state. We
// call it from each `a11y` spec's `beforeEach` so every test sees the
// same starting state regardless of run order.
//
// The endpoint is dev-only — only the `replay --serve` binary mounts it,
// and that binary is itself dev-only (design doc §6.3).

import type { APIRequestContext } from '@playwright/test';

/** Port the replay binary listens on. Duplicated here (rather than imported
 *  from `_setup/replay-server.shared.ts`) because `_setup/` is a
 *  Playwright project file the test runner treats as its own compilation
 *  context — pulling it from a spec creates a circular `testMatch` /
 *  `testIgnore` dependency. Keep in sync with the constant in that file. */
export const REPLAY_PORT = 4141;

/** Hit `POST /dev/reset` on the replay binary. Throws on non-2xx so a
 *  failing reset surfaces immediately in the test that triggered it
 *  rather than producing a confusing assertion failure later. */
export async function resetReplayServer(request: APIRequestContext): Promise<void> {
  const url = `http://127.0.0.1:${REPLAY_PORT}/dev/reset`;
  const response = await request.post(url);
  if (!response.ok()) {
    const body = await response.text().catch(() => '<unreadable body>');
    throw new Error(
      `resetReplayServer: POST ${url} → ${response.status()} ${response.statusText()}: ${body}`,
    );
  }
}

/**
 * Issue #175 — mint a user-facing cove via the kernel REST API so the
 * keyboard-only tests have a stable anchor in the sidebar after the
 * pre-#175 `Scratch` auto-bootstrap moved into a hidden system cove.
 * The replay binary serves the same routes as production, so a direct
 * `POST /api/coves` call here lands a real row backed by an
 * `EventScope::System` `cove.updated` event — the live frontend picks
 * it up on the WS feed and the sidebar renders the new cove without
 * a reload.
 *
 * The default name `'Atlas'` matches the fixture sweep applied across
 * the unit-test surface (`web/src/pages/Cove.test.tsx`,
 * `web/src/app/eventBridge.test.tsx`, `web/src/api/schemas.test.ts`,
 * `web/src/api/queries.test.tsx`) — keeping the e2e suite on the same
 * sentinel makes "where did this cove come from?" greppable across the
 * codebase.
 *
 * Returns the cove id (UUID, kernel-generated).
 */
export async function createUserCove(
  request: APIRequestContext,
  name = 'Atlas',
  color = '#6a8',
): Promise<{ id: string; name: string }> {
  const url = `http://127.0.0.1:${REPLAY_PORT}/api/coves`;
  const response = await request.post(url, {
    data: { name, color },
    headers: { 'content-type': 'application/json' },
  });
  if (!response.ok()) {
    const body = await response.text().catch(() => '<unreadable body>');
    throw new Error(
      `createUserCove: POST ${url} → ${response.status()} ${response.statusText()}: ${body}`,
    );
  }
  const cove = (await response.json()) as { id: string; name: string };
  return cove;
}

/**
 * Issue #175 — mint a wave inside an existing cove. Counterpart to
 * `createUserCove`; the a11y keyboard suite uses both to set up an
 * `Atlas` cove with a `Today` wave inside it, replacing the pre-#175
 * auto-bootstrap that put the Today wave inside what is now the hidden
 * system cove.
 */
export async function createWaveInCove(
  request: APIRequestContext,
  coveId: string,
  title: string,
): Promise<{ id: string; title: string }> {
  const url = `http://127.0.0.1:${REPLAY_PORT}/api/waves`;
  const response = await request.post(url, {
    data: { cove_id: coveId, title },
    headers: { 'content-type': 'application/json' },
  });
  if (!response.ok()) {
    const body = await response.text().catch(() => '<unreadable body>');
    throw new Error(
      `createWaveInCove: POST ${url} → ${response.status()} ${response.statusText()}: ${body}`,
    );
  }
  const wave = (await response.json()) as { id: string; title: string };
  return wave;
}
