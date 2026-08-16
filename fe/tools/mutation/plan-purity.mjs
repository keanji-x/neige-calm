import { spawnSync } from 'node:child_process';
import { chmodSync, mkdtempSync, mkdirSync, readdirSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { resolve } from 'node:path';

/**
 * Executable pin for `run.mjs --plan`: planning must be a pure READ of the worktree.
 *
 * Today that holds only because mkdtempSync and every `git apply` sit textually after
 * `process.exit(0)`; hoisting any of them would silently make the plan job mutate the tree.
 *
 * Two complementary assertions, because end-state checks alone are a POST-STATE oracle, not a
 * purity oracle: a `--plan` that creates a temp dir and/or writes into the worktree and then
 * cleans up before exiting passes every after-the-fact check (verified in review round 2).
 *
 *  1. END STATE (run 1, TMPDIR redirected into a private sandbox): exit 0, exactly one JSON line
 *     with the expected keys, worktree clean, and the sandbox empty afterwards. Scoping TMPDIR to
 *     a directory this pin owns also removes the old false-RED: the previous version scanned the
 *     shared os.tmpdir() for `neige-mutation-*`, so any unrelated concurrent runner tripped it.
 *  2. TRANSIENT STATE (run 2, TMPDIR sealed read-only): the same plan JSON must still come out.
 *     A `--plan` that so much as calls mkdtempSync EACCESes here even if it would have cleaned up,
 *     so create-then-delete can no longer hide. Empirically the legitimate path is unaffected —
 *     the git subprocesses it spawns (ls-files / status / merge-base / show) need no temp dir.
 */

const feRoot = resolve(import.meta.dirname, '../..');

/** @param {unknown} condition @param {string} message */
function assert(condition, message) {
  if (!condition) throw new Error(`plan purity: ${message}`);
}

/** @param {string} temporaryDirectory */
function runPlan(temporaryDirectory) {
  return spawnSync(process.execPath, ['tools/mutation/run.mjs', '--plan'], {
    cwd: feRoot, encoding: 'utf8', maxBuffer: 64 * 1024 * 1024,
    env: { ...process.env, TMPDIR: temporaryDirectory },
  });
}

const sandbox = mkdtempSync(resolve(tmpdir(), 'plan-purity-'));
const writable = resolve(sandbox, 'writable');
const sealed = resolve(sandbox, 'sealed');
mkdirSync(writable);
mkdirSync(sealed);

try {
  const plan = runPlan(writable);
  assert(plan.status === 0, `--plan exited ${plan.status}: ${plan.stderr}`);
  const lines = plan.stdout.split('\n').filter((line) => line !== '');
  assert(lines.length === 1, `--plan wrote ${lines.length} stdout lines, expected exactly one JSON line`);
  const parsed = JSON.parse(lines[0]);
  assert(Object.keys(parsed).sort().join(',') === 'clamped,selected,shards,total',
    `--plan JSON keys are ${Object.keys(parsed).sort().join(',')}`);
  assert(Array.isArray(parsed.shards) && parsed.shards.length === parsed.total, '--plan shards do not match total');
  assert(typeof parsed.selected === 'number' && typeof parsed.clamped === 'boolean', '--plan selected/clamped have wrong types');

  const status = spawnSync('git', ['status', '--porcelain'], { cwd: feRoot, encoding: 'utf8', maxBuffer: 64 * 1024 * 1024 });
  assert(status.status === 0, `git status exited ${status.status}`);
  assert(status.stdout.trim() === '', `--plan left the worktree dirty:\n${status.stdout}`);

  const leftover = readdirSync(writable);
  assert(leftover.length === 0, `--plan left entries in its temp dir: ${leftover.join(', ')}`);

  // Control: prove the seal actually denies writes before trusting run 2. As root chmod 500 is a
  // no-op, which would turn the transient-state assertion into a silent pass — fail loudly instead.
  chmodSync(sealed, 0o500);
  let sealHolds = false;
  try {
    rmSync(mkdtempSync(resolve(sealed, 'probe-')), { recursive: true, force: true });
  } catch {
    sealHolds = true;
  }
  assert(sealHolds, 'a read-only TMPDIR is still writable here (running as root?), so the transient-state check cannot discriminate');

  const sealedPlan = runPlan(sealed);
  assert(sealedPlan.status === 0,
    `--plan exited ${sealedPlan.status} with a read-only TMPDIR, so it touches the filesystem while planning: ${sealedPlan.stderr}`);
  assert(sealedPlan.stdout === plan.stdout,
    `--plan produced a different plan with a read-only TMPDIR:\n${plan.stdout}\nvs\n${sealedPlan.stdout}`);
} finally {
  chmodSync(sealed, 0o700);
  rmSync(sandbox, { recursive: true, force: true });
}

console.log('mutation plan purity: clean worktree, no temp dir, read-only TMPDIR unchanged, plan verified');
