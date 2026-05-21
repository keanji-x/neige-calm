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
