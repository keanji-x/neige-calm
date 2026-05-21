// Playwright project-setup file — spawns the Rust replay binary with a
// curated event-trace fixture preloaded. Referenced from
// `playwright.config.ts` as the body of the `replay-setup` project
// (the `a11y` project lists it as a dependency).
//
// Slice 5 of issue #56: the a11y project uses this so each run starts
// from a hermetic, in-memory server seeded with a known event sequence.
// The replay binary already supports the shape we need — `cargo run
// --bin replay -- --serve --file <path> --port <p>` — and prints
// `calm-server (replay mode) listening on http://<addr>` on stdout once
// `axum::serve` is bound (see `crates/calm-server/src/bin/replay.rs`).
// We poll that banner for readiness rather than sleeping.
//
// Lifecycle (driven by Playwright's project-dependency machinery):
//   1. `replay-setup` runs this `setup` test before any `a11y` spec.
//      It spawns cargo, waits for the ready banner, and stashes the PID
//      in a temp file on disk so `replay-server.teardown.ts` (in a
//      different worker process) can find it.
//   2. `a11y` specs run, hitting the seeded server.
//   3. `replay-teardown` runs `replay-server.teardown.ts` to SIGTERM the
//      cargo child.
//
// Why a PID-on-disk handoff: Playwright runs each project's tests in a
// separate worker process, so a module-level variable wouldn't survive
// across setup → tests → teardown. The PID file lives under
// `node_modules/.cache/` (git-ignored, wiped by `npm ci`) so a stray
// orphan after a hard crash is recoverable.

import { spawn, type ChildProcess } from 'node:child_process';
import { existsSync, mkdirSync, writeFileSync } from 'node:fs';
import { dirname, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { test as base } from '@playwright/test';
import {
  DEFAULT_FIXTURE,
  PID_FILE,
  READY_BANNER,
  READY_TIMEOUT_MS,
  REPLAY_PORT,
  REPO_ROOT,
} from './replay-server.shared';

const test = base.extend({});

test('setup', async () => {
  const fixture = process.env.NEIGE_FIXTURE ?? DEFAULT_FIXTURE;
  const fixturePath = resolve(REPO_ROOT, fixture);
  if (!existsSync(fixturePath)) {
    throw new Error(
      `replay-setup: fixture not found at ${fixturePath} (NEIGE_FIXTURE=${process.env.NEIGE_FIXTURE ?? '<unset>'})`,
    );
  }

  const args = [
    'run',
    '--quiet',
    '--bin',
    'replay',
    '--',
    '--serve',
    '--file',
    fixturePath,
    '--port',
    String(REPLAY_PORT),
  ];

  // detached:true puts the child in its own process group so teardown
  // can SIGTERM the whole group. cargo wraps its child in another
  // process; killing only the cargo PID can leave the actual replay
  // server orphaned holding the port.
  const child = spawn('cargo', args, {
    cwd: REPO_ROOT,
    env: process.env,
    stdio: ['ignore', 'pipe', 'pipe'],
    detached: true,
  });

  await waitForBanner(child);

  if (!child.pid) {
    throw new Error('replay-setup: cargo child has no PID after spawn');
  }
  mkdirSync(dirname(PID_FILE), { recursive: true });
  writeFileSync(PID_FILE, String(child.pid), 'utf8');

  // Unref the cargo child so the setup worker process can exit cleanly;
  // the replay server keeps running until teardown kills it. We don't
  // unref the stdio streams individually — once we drop our listeners
  // (which we don't, on purpose, so a late stderr line still gets
  // surfaced), they don't keep the event loop alive on their own.
  child.unref();

  // eslint-disable-next-line no-console
  console.log(
    `[replay-server] ready at http://127.0.0.1:${REPLAY_PORT}/ (fixture=${fixture}, pid=${child.pid})`,
  );
});

/** Resolve once the replay binary writes a line containing `READY_BANNER`
 *  to stdout. Rejects if the process exits before then (cargo build
 *  failure, port conflict, etc.) so the test run dies with a clear cause
 *  instead of timing out on connect later. */
function waitForBanner(child: ChildProcess): Promise<void> {
  return new Promise<void>((resolveFn, rejectFn) => {
    let settled = false;
    const timer = setTimeout(() => {
      settle(
        new Error(
          `replay-setup: server did not announce readiness within ${READY_TIMEOUT_MS}ms`,
        ),
      );
    }, READY_TIMEOUT_MS);

    const settle = (err?: Error) => {
      if (settled) return;
      settled = true;
      clearTimeout(timer);
      if (err) rejectFn(err);
      else resolveFn();
    };

    const scan = (chunk: Buffer | string) => {
      const text = typeof chunk === 'string' ? chunk : chunk.toString('utf8');
      process.stdout.write(`[replay-server stdout] ${text}`);
      if (text.includes(READY_BANNER)) settle();
    };

    child.stdout?.on('data', scan);
    child.stderr?.on('data', (chunk: Buffer | string) => {
      const text = typeof chunk === 'string' ? chunk : chunk.toString('utf8');
      process.stderr.write(`[replay-server stderr] ${text}`);
    });
    child.once('exit', (code, signal) => {
      settle(
        new Error(
          `replay-setup: replay server exited before readiness (code=${code ?? 'null'}, signal=${signal ?? 'null'})`,
        ),
      );
    });
    child.once('error', (err) => settle(err));
  });
}
