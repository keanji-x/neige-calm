// Shared constants used by both `replay-server.setup.ts` and
// `replay-server.teardown.ts`. Single place to bump the port, the ready
// banner string, or the fixture default if any of them drift.

import { dirname, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

/** Path to the repo root (one level up from `web/`). Computed relative to
 *  this file so a `cd anywhere && npx playwright test` invocation still
 *  resolves the right cargo manifest. */
export const REPO_ROOT = resolve(dirname(fileURLToPath(import.meta.url)), '..', '..', '..');

/** Default fixture used when `NEIGE_FIXTURE` is unset. The wave-grid layout
 *  trace is the only curated fixture in tree as of slice 5; if more land,
 *  this default stays put so existing tests don't silently re-target. */
export const DEFAULT_FIXTURE =
  'crates/calm-server/tests/fixtures/events/wave-grid-layout-trace.events.json';

/** Port the replay server binds. Hard-coded to a non-standard value so it
 *  doesn't collide with a developer's `make dev` (which owns 4040). */
export const REPLAY_PORT = 4141;

/** Substring of the readiness banner printed by `bin/replay.rs` once
 *  `axum::serve` is bound. The exact line is `calm-server (replay mode)
 *  listening on http://<addr>`. */
export const READY_BANNER = 'listening on http://';

/** Hard upper bound on cargo build + bind time. A cold `cargo run` on
 *  this workspace takes ~25s on a quiet machine; 60s gives headroom for
 *  a busy CI runner without masking a genuinely stuck process. */
export const READY_TIMEOUT_MS = 60_000;

/** Grace period between SIGTERM and SIGKILL during teardown. */
export const SHUTDOWN_GRACE_MS = 5_000;

/** Location of the PID handoff file. Lives under `node_modules/.cache/`
 *  so it's git-ignored by default and disappears on `npm ci`. */
export const PID_FILE = resolve(
  dirname(fileURLToPath(import.meta.url)),
  '..',
  '..',
  'node_modules',
  '.cache',
  'neige-replay-server.pid',
);
