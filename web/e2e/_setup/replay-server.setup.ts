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
import {
  existsSync,
  mkdirSync,
  openSync,
  writeFileSync,
} from 'node:fs';
import { dirname, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { test as base } from '@playwright/test';
import {
  CODEX_BIN_FILE,
  CODEX_MISSING_SENTINEL,
  DEFAULT_FIXTURE,
  PID_FILE,
  READY_BANNER,
  READY_TIMEOUT_MS,
  REPLAY_PORT,
  REPO_ROOT,
  resolveCodexBin,
} from './replay-server.shared';

// On-disk log target for the replay binary's stderr. Routing stderr
// to a real file fd (rather than a pipe) means the kernel keeps the
// write end alive after the Playwright setup worker exits — no
// dangling pipe, no SIGPIPE on first `tracing::info!` write inside
// the cargo grandchild (root cause of the `socket hang up` flake
// investigated in debug PR #191). The file also doubles as a CI
// artifact: Playwright's `upload-artifact` step (see `.github/
// workflows/ci.yml`) picks up `web/test-results/replay-server.log`
// on a11y-job failure so diagnostics survive past worker exit.
const REPLAY_LOG_DIR = resolve(REPO_ROOT, 'web/test-results');
const REPLAY_LOG_PATH = resolve(REPLAY_LOG_DIR, 'replay-server.log');

const test = base.extend({});

test('setup', async () => {
  test.setTimeout(READY_TIMEOUT_MS + 10_000);

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
    // #682 — the replay [[bin]] declares `required-features = ["fixtures"]`
    // (its `/dev/force-spec-phase` hook reaches a fixtures-gated harness
    // seam); cargo refuses to build it without the flag. Keep in sync with
    // the `cargo build --bin replay` step in `.github/workflows/ci.yml`.
    '--features',
    'fixtures',
    '--',
    '--serve',
    '--file',
    fixturePath,
    '--port',
    String(REPLAY_PORT),
  ];

  // Open the stderr log target as an append fd before spawning. The
  // kernel holds this fd open for the cargo grandchild even after
  // the Playwright setup worker exits — no pipe → no SIGPIPE
  // pathway when the replay binary's first `tracing::info!` writes
  // through `tracing-subscriber`. See debug PR #191 for the
  // root-cause investigation.
  mkdirSync(REPLAY_LOG_DIR, { recursive: true });
  const stderrFd = openSync(REPLAY_LOG_PATH, 'a');

  // detached:true puts the child in its own process group so teardown
  // can SIGTERM the whole group. cargo wraps its child in another
  // process; killing only the cargo PID can leave the actual replay
  // server orphaned holding the port. Since PR #630 made the replay
  // grandchild survive stdout EPIPE, a setup failure can also leave
  // that orphan alive after Playwright kills the worker; the failure
  // path below SIGKILLs this process group before any PID handoff.
  //
  // stdio: stdout stays a pipe so `waitForBanner` can scan for the
  // ready line. stderr goes straight to the file fd above so the
  // cargo grandchild's `tracing-subscriber` writes never hit a dead
  // pipe — that was the SIGPIPE-kill pathway diagnosed in debug PR
  // #191. The fd in the parent is dropped on worker exit; the child
  // inherits its own copy and keeps writing to the file.
  const child = spawn('cargo', args, {
    cwd: REPO_ROOT,
    env: process.env,
    stdio: ['ignore', 'pipe', stderrFd],
    detached: true,
  });

  try {
    await waitForBanner(child);

    if (!child.pid) {
      throw new Error('replay-setup: cargo child has no PID after spawn');
    }
    mkdirSync(dirname(PID_FILE), { recursive: true });
    writeFileSync(PID_FILE, String(child.pid), 'utf8');
  } catch (e) {
    killProcessGroupOnSetupFailure(child);
    throw e;
  }

  // Unref the cargo child so the setup worker process can exit cleanly;
  // the replay server keeps running until teardown kills it. stderr
  // now goes to a file fd (not a pipe), so there's no stderr stream
  // listener keeping the event loop alive — only the stdout pipe
  // remains, and the banner-scan listener is implicitly dropped once
  // `waitForBanner` resolves.
  child.unref();

  // eslint-disable-next-line no-console
  console.log(
    `[replay-server] ready at http://127.0.0.1:${REPLAY_PORT}/ (fixture=${fixture}, pid=${child.pid}, stderr=${REPLAY_LOG_PATH})`,
  );

  // #177 — probe for a usable codex CLI on this machine and write the
  // result to CODEX_BIN_FILE. The theme-toggle-no-remount spec reads
  // this marker synchronously at module load and `test.skip`s itself
  // if codex isn't available. We do this AFTER the server is ready
  // so a codex-resolution hiccup doesn't gate the rest of the a11y
  // suite (only the spec that depends on codex).
  const codexBin = resolveCodexBin();
  mkdirSync(dirname(CODEX_BIN_FILE), { recursive: true });
  writeFileSync(
    CODEX_BIN_FILE,
    codexBin ?? CODEX_MISSING_SENTINEL,
    'utf8',
  );
  // eslint-disable-next-line no-console
  console.log(
    `[replay-server] codex resolution: ${codexBin ?? '<missing — codex-dependent specs will skip>'}`,
  );
});

function killProcessGroupOnSetupFailure(child: ChildProcess): void {
  if (!child.pid) return;

  try {
    process.kill(-child.pid, 'SIGKILL');
  } catch (e) {
    if ((e as NodeJS.ErrnoException).code !== 'ESRCH') throw e;
  }
}

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
    // stderr is routed to a file fd at spawn (see REPLAY_LOG_PATH);
    // no in-process stream to attach a listener to. The file is
    // uploaded as a CI artifact on a11y-job failure and is also
    // tail-able locally for diagnostics.
    child.once('exit', (code, signal) => {
      settle(
        new Error(
          `replay-setup: replay server exited before readiness (code=${code ?? 'null'}, signal=${signal ?? 'null'}); see ${REPLAY_LOG_PATH} for server logs`,
        ),
      );
    });
    child.once('error', (err) => settle(err));
  });
}
