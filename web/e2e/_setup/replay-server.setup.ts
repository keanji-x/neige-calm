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

import { spawn, spawnSync, type ChildProcess } from 'node:child_process';
import {
  existsSync,
  mkdirSync,
  openSync,
  writeFileSync,
} from 'node:fs';
import { delimiter, dirname, resolve } from 'node:path';
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
  const fixture = process.env.NEIGE_FIXTURE ?? DEFAULT_FIXTURE;
  const fixturePath = resolve(REPO_ROOT, fixture);
  if (!existsSync(fixturePath)) {
    throw new Error(
      `replay-setup: fixture not found at ${fixturePath} (NEIGE_FIXTURE=${process.env.NEIGE_FIXTURE ?? '<unset>'})`,
    );
  }

  // ----- Pre-build the daemon + codex bridge binaries ----------------------
  // #177 regression spec (`a11y-177-theme-toggle-no-remount.spec.ts`)
  // depends on the `replay` binary spawning a real `calm-session-daemon`
  // PTY when the kernel mints a wave's spec card. The daemon path
  // resolver in `crates/calm-server/src/state.rs::resolve_session_daemon_bin`
  // looks for the binary as a sibling of the running exe; the
  // `cargo run --bin replay` invocation below puts `replay` into
  // `target/debug/`, so we MUST have `calm-session-daemon` (and
  // `neige-codex-bridge`, which codex calls via its hooks.json) built
  // into the same directory beforehand. Otherwise the daemon-spawn step
  // fails with `No such file or directory`, no PTY traffic flows, no
  // RenderPatch events reach the browser, and the spec sees a
  // "wave creates but XtermView never receives bytes" surface that does
  // NOT reproduce the user's prod bug. `cargo build -p <pkg>` is
  // idempotent + cached; on a warm tree it's a sub-second no-op.
  mkdirSync(REPLAY_LOG_DIR, { recursive: true });
  buildBinIfMissing('calm-session', 'calm-session-daemon');
  buildBinIfMissing('calm-codex-bridge', 'neige-codex-bridge');

  // ----- Resolve a usable codex CLI ---------------------------------------
  // Production-shape factor #4 from `a11y-177-theme-toggle-no-remount.spec.ts`'s
  // "candidates the test stack omits" docblock: codex's PTY traffic
  // driving RenderPatch events. We try to put a real codex on PATH for
  // the cargo-spawned replay binary, and we record the resolution
  // outcome to `CODEX_BIN_FILE` so the spec can self-skip with a clear
  // message when codex isn't installed locally. The probe lives in
  // `replay-server.shared.ts::resolveCodexBin` — it covers env override,
  // pinned nvm paths, common user-bin locations, and a login-shell
  // `command -v` fallback. See that function's docblock for ordering.
  const codexBin = resolveCodexBin();
  mkdirSync(dirname(CODEX_BIN_FILE), { recursive: true });
  writeFileSync(CODEX_BIN_FILE, codexBin ?? CODEX_MISSING_SENTINEL, 'utf8');
  // eslint-disable-next-line no-console
  console.log(
    `[replay-server] codex resolution → ${codexBin ?? '(not found; #177 spec will skip)'}`,
  );

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

  // Augment PATH for the spawned cargo child so the codex binary's
  // directory is searchable even when this Playwright worker was
  // launched from a context with a stripped PATH (the common case on
  // macOS GUI sessions). The kernel spawns codex via `/bin/sh -c
  // "codex"` (see `crates/calm-server/src/routes/codex_cards.rs`), so
  // it's the cargo child's PATH that matters — not this Node
  // process's. Prepending (not appending) ensures the resolved codex
  // wins over any older system one. When codex wasn't found, we don't
  // mutate PATH at all; the cargo child boots, the kernel still
  // attempts to spawn codex, the daemon-spawn fails the same way it
  // would in any codex-less env, and the spec's `test.skip` gate
  // catches the gap before the assertion runs.
  const childEnv = { ...process.env };
  if (codexBin) {
    const codexDir = dirname(codexBin);
    childEnv.PATH = `${codexDir}${delimiter}${process.env.PATH ?? ''}`;
    // Also export `CALM_CODEX_BIN` as a belt-and-suspenders measure
    // for code paths that prefer the explicit env override (see
    // `crates/calm-server/src/config.rs`). The replay binary itself
    // uses `CodexClient::new_stub()` which hardcodes `"codex"`, so
    // PATH augmentation is the load-bearing knob — this is just
    // future-proofing if the replay stack moves to `CodexClient::new`.
    childEnv.CALM_CODEX_BIN = codexBin;
  }

  // Open the stderr log target as an append fd before spawning. The
  // kernel holds this fd open for the cargo grandchild even after
  // the Playwright setup worker exits — no pipe → no SIGPIPE
  // pathway when the replay binary's first `tracing::info!` writes
  // through `tracing-subscriber`. See debug PR #191 for the
  // root-cause investigation. (`REPLAY_LOG_DIR` was already created
  // above as part of the `buildBinIfMissing` preamble.)
  const stderrFd = openSync(REPLAY_LOG_PATH, 'a');

  // detached:true puts the child in its own process group so teardown
  // can SIGTERM the whole group. cargo wraps its child in another
  // process; killing only the cargo PID can leave the actual replay
  // server orphaned holding the port.
  //
  // stdio: stdout stays a pipe so `waitForBanner` can scan for the
  // ready line. stderr goes straight to the file fd above so the
  // cargo grandchild's `tracing-subscriber` writes never hit a dead
  // pipe — that was the SIGPIPE-kill pathway diagnosed in debug PR
  // #191. The fd in the parent is dropped on worker exit; the child
  // inherits its own copy and keeps writing to the file.
  //
  // env: `childEnv` (built above) prepends the resolved codex
  // directory to PATH so the kernel's `/bin/sh -c "codex"` spawn
  // finds it, and sets `CALM_CODEX_BIN` as a redundant override.
  // When codex wasn't located on this machine `childEnv` is a plain
  // `process.env` copy — no behavioral change vs the pre-#177 setup.
  const child = spawn('cargo', args, {
    cwd: REPO_ROOT,
    env: childEnv,
    stdio: ['ignore', 'pipe', stderrFd],
    detached: true,
  });

  await waitForBanner(child);

  if (!child.pid) {
    throw new Error('replay-setup: cargo child has no PID after spawn');
  }
  mkdirSync(dirname(PID_FILE), { recursive: true });
  writeFileSync(PID_FILE, String(child.pid), 'utf8');

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

/**
 * Synchronously build a workspace binary into `target/debug/` if it
 * isn't already there. Used to ensure `calm-session-daemon` and
 * `neige-codex-bridge` are siblings of the `cargo run --bin replay`
 * exe before the replay binary boots — the kernel's daemon-spawn
 * helper resolves the binary as a sibling of `current_exe()` (see
 * `crates/calm-server/src/state.rs::resolve_session_daemon_bin`), and
 * if it's missing, every wave-create / spec-card / interactive-codex
 * REST endpoint fails with `spawn calm-session-daemon: No such file or
 * directory`. The #177 regression spec needs the daemon to actually
 * spawn so codex's PTY traffic produces RenderPatch events in the
 * browser; without that, the test reproduces a stub-codex env that
 * doesn't carry the production factor the user-reported bug needs.
 *
 * `cargo build` is idempotent + content-hashed — on a warm tree this
 * is a sub-second no-op. We only re-build when the binary is genuinely
 * missing (the fast `existsSync` gate) so an in-place dev iteration
 * (`cargo run` against a hot binary) isn't slowed down by a redundant
 * build invocation here.
 *
 * Throws if cargo exits non-zero so a broken build fails the suite
 * loudly rather than silently leaving the daemon missing and reading
 * as "test stack doesn't repro #177".
 */
function buildBinIfMissing(pkg: string, binName: string): void {
  const target = resolve(REPO_ROOT, 'target', 'debug', binName);
  if (existsSync(target)) return;

  // eslint-disable-next-line no-console
  console.log(`[replay-server] building ${binName} (one-time, sibling-of-replay requirement)…`);
  const result = spawnSync(
    'cargo',
    ['build', '--quiet', '-p', pkg, '--bin', binName],
    {
      cwd: REPO_ROOT,
      env: process.env,
      stdio: ['ignore', 'inherit', 'inherit'],
    },
  );
  if (result.status !== 0) {
    throw new Error(
      `replay-setup: cargo build -p ${pkg} --bin ${binName} exited with ` +
        `status=${result.status ?? 'null'} signal=${result.signal ?? 'null'}`,
    );
  }
  if (!existsSync(target)) {
    throw new Error(
      `replay-setup: cargo build reported success but ${target} is still missing`,
    );
  }
}
