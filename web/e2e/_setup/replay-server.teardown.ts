// Playwright project-teardown file — kills the Rust replay binary that
// `replay-server.setup.ts` spawned. Referenced from `playwright.config.ts`
// as the body of the `replay-teardown` project (the `replay-setup`
// project lists it as its `teardown` so Playwright runs it reliably even
// after a failure or Ctrl-C).
//
// The setup worker recorded the PID under `node_modules/.cache/
// neige-replay-server.pid` (across-worker handoff via the filesystem,
// because Playwright runs each project in its own worker process).
// Teardown reads that file, SIGTERMs the process group, waits a grace
// period, then SIGKILLs anything still alive.

import { existsSync, readFileSync, unlinkSync } from 'node:fs';
import { test as base } from '@playwright/test';
import { PID_FILE, SHUTDOWN_GRACE_MS } from './replay-server.shared';

const test = base.extend({});

test('teardown', async () => {
  if (!existsSync(PID_FILE)) {
    // eslint-disable-next-line no-console
    console.warn('[replay-server] no PID file at teardown — server may have crashed');
    return;
  }
  const pid = Number(readFileSync(PID_FILE, 'utf8').trim());
  unlinkSync(PID_FILE);
  if (!Number.isFinite(pid) || pid <= 0) {
    // eslint-disable-next-line no-console
    console.warn(`[replay-server] PID file contained garbage: ${pid}`);
    return;
  }

  // Send SIGTERM to the process group (negative PID). cargo spawns the
  // replay binary as a child; killing only the cargo PID leaves the
  // actual server holding the port.
  try {
    process.kill(-pid, 'SIGTERM');
  } catch (e) {
    if ((e as NodeJS.ErrnoException).code !== 'ESRCH') throw e;
    // Process already gone — nothing to do.
    return;
  }

  // Give the server a grace window to shut down cleanly; if it's still
  // around afterward, escalate to SIGKILL. SIGKILL on a phantom PID is
  // a no-op (returns ESRCH); we swallow that.
  await new Promise((r) => setTimeout(r, SHUTDOWN_GRACE_MS));
  try {
    process.kill(-pid, 'SIGKILL');
  } catch (e) {
    if ((e as NodeJS.ErrnoException).code !== 'ESRCH') throw e;
  }
});
