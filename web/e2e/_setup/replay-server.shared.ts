// Shared constants used by both `replay-server.setup.ts` and
// `replay-server.teardown.ts`. Single place to bump the port, the ready
// banner string, or the fixture default if any of them drift.

import { execFileSync } from 'node:child_process';
import { existsSync, readdirSync } from 'node:fs';
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

/** Sibling of `PID_FILE` — records the absolute path of a usable
 *  `codex` CLI binary that the setup step located on the developer
 *  machine, or the sentinel `__missing__` if no codex was found. The
 *  spec `a11y-177-theme-toggle-no-remount.spec.ts` (#177) reads this
 *  file synchronously at module load to decide whether to `test.skip`
 *  itself.
 *
 *  Why a file (rather than a fixture / process env): Playwright runs
 *  `replay-setup` in a different worker process from the specs that
 *  consume it (same pattern as `PID_FILE`); a module-level constant
 *  in setup wouldn't survive the worker boundary. The file lives
 *  under `node_modules/.cache/` so `npm ci` wipes it — re-resolution
 *  every fresh run, no stale "codex is here" entries after the
 *  binary is uninstalled. */
export const CODEX_BIN_FILE = resolve(
  dirname(fileURLToPath(import.meta.url)),
  '..',
  '..',
  'node_modules',
  '.cache',
  'neige-replay-codex-bin.path',
);

/** Sentinel written into `CODEX_BIN_FILE` when the setup step could
 *  not locate a `codex` CLI on disk. Pulled out as a constant so the
 *  spec and the setup agree on the exact string. */
export const CODEX_MISSING_SENTINEL = '__missing__';

/**
 * Best-effort probe for a usable `codex` CLI on this machine.
 *
 * Resolution order (first hit wins):
 *   1. `process.env.CALM_CODEX_BIN` — explicit override.
 *   2. Pinned `~/.nvm/versions/node/v24.4.1/bin/codex` — the most
 *      common hit on the maintainer's workstation.
 *   3. Shallow scan of `~/.nvm/versions/node/*\/bin/codex`,
 *      `~/.local/bin/codex`, `~/bin/codex`.
 *   4. Last resort — `bash -lc 'command -v codex'` so a user's
 *      profile-augmented `PATH` (nvm.sh / asdf / pnpm) is honored
 *      even if the parent Playwright worker inherited a stripped-
 *      down env.
 *
 * Returns the absolute path on success, or `null` if every probe
 * misses. The caller (the `replay-setup` test) decides what to do
 * with that — typically writes `__missing__` into `CODEX_BIN_FILE`
 * so the e2e spec self-skips with a clear "codex CLI not installed"
 * message.
 */
export function resolveCodexBin(): string | null {
  const fromEnv = process.env.CALM_CODEX_BIN;
  if (fromEnv && fromEnv.trim().length > 0 && existsSync(fromEnv)) {
    return fromEnv;
  }

  const home = process.env.HOME;
  if (home) {
    const pinned = resolve(home, '.nvm/versions/node/v24.4.1/bin/codex');
    if (existsSync(pinned)) return pinned;

    for (const rel of ['.local/bin/codex', 'bin/codex']) {
      const p = resolve(home, rel);
      if (existsSync(p)) return p;
    }

    try {
      const nvmRoot = resolve(home, '.nvm/versions/node');
      if (existsSync(nvmRoot)) {
        for (const entry of readdirSync(nvmRoot)) {
          const candidate = resolve(nvmRoot, entry, 'bin/codex');
          if (existsSync(candidate)) return candidate;
        }
      }
    } catch {
      /* readdir / nvm dir not there — fall through to step 4. */
    }
  }

  try {
    const stdout = execFileSync('bash', ['-lc', 'command -v codex'], {
      encoding: 'utf8',
      stdio: ['ignore', 'pipe', 'ignore'],
      timeout: 5_000,
    });
    const trimmed = stdout.trim();
    if (trimmed.length > 0 && existsSync(trimmed)) return trimmed;
  } catch {
    /* login shell errored / no codex on PATH — give up. */
  }

  return null;
}
