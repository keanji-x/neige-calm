import { spawnSync } from 'node:child_process';
import { chmodSync, mkdtempSync, mkdirSync, readdirSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { resolve } from 'node:path';

/**
 * Executable pin for `run.mjs --plan`: planning must be a READ of the worktree.
 *
 * Today that holds only because mkdtempSync and every `git apply` sit textually after
 * `process.exit(0)`; hoisting any of them would silently make the plan job mutate the tree.
 *
 * WHAT THIS PINS, exactly — two assertions per checked command, no more:
 *
 *  1. END STATE (run 1, TMPDIR redirected into a private sandbox): exit 0, exactly one JSON line
 *     with the expected keys, the worktree clean afterwards, and the sandbox empty. Scoping TMPDIR
 *     to a directory this pin owns also removes the old false-RED: the previous version scanned the
 *     shared os.tmpdir() for `neige-mutation-*`, so any unrelated concurrent runner tripped it.
 *  2. TEMP-DIR USE ROUTED THROUGH TMPDIR (run 2, TMPDIR sealed read-only): the same plan JSON must
 *     still come out. Anything reaching os.tmpdir()/mkdtempSync — which honour TMPDIR — EACCESes
 *     here even if it would have cleaned up, so create-then-delete under TMPDIR cannot hide.
 *     Empirically the legitimate path is unaffected: the git subprocesses it spawns (ls-files /
 *     status / merge-base / show / diff) need no temp dir.
 *
 * WHAT THIS DOES NOT PIN — do not read assertion 2 as "the plan cannot write anywhere". Measured by
 * stubbing run.mjs six ways in review round 3, these variants stay GREEN:
 *
 *  - a worktree write-then-delete with NO temp-dir use at all (assertion 1 only sees the end state,
 *    and assertion 2 only sees TMPDIR). This is the headline gap; the process would need an fs
 *    syscall trace (or a read-only bind mount of the worktree) to catch it, which is out of scope
 *    for a gate that has to run inside a GitHub runner.
 *  - a hardcoded `mkdtempSync('/tmp/neige-mutation-')` that bypasses TMPDIR entirely.
 *  - a write-then-delete under `$HOME` or `/dev/shm`.
 *
 * A leftover (undeleted) write into the worktree IS caught, by assertion 1.
 *
 * SELF-CHECK: `fixtures/impure-plan.mjs` is a faithful plan whose single violation is exactly the
 * transient TMPDIR + worktree touch of the round-2 stub. It is driven through the same
 * `checkPurity` used for the real command and must be REPORTED AS FAILING. Without it nothing
 * proves these assertions can go red at all — both earlier review rounds had to stub by hand.
 */

const feRoot = resolve(import.meta.dirname, '../..');
const runner = 'tools/mutation/run.mjs';
const impureFixture = 'tools/mutation/fixtures/impure-plan.mjs';
const baseRef = 'origin/main';

/** A purity property was violated by the command under test — as opposed to the harness itself being unusable. */
class PurityViolation extends Error {}

/** @param {unknown} condition @param {string} message */
function violationUnless(condition, message) {
  if (!condition) throw new PurityViolation(message);
}

/** @param {unknown} condition @param {string} message */
function assert(condition, message) {
  if (!condition) throw new Error(`plan purity: ${message}`);
}

/** @param {string[]} args @param {string} temporaryDirectory */
function run(args, temporaryDirectory) {
  return spawnSync(process.execPath, args, {
    cwd: feRoot, encoding: 'utf8', maxBuffer: 64 * 1024 * 1024,
    env: { ...process.env, TMPDIR: temporaryDirectory },
  });
}

/**
 * Runs `args` under a writable TMPDIR and then under a sealed one and asserts both purity
 * properties. Throws PurityViolation on the first property the command breaks; a plain Error means
 * the harness could not decide (and must never be mistaken for a detected violation).
 * @param {string} label @param {string[]} args
 */
function checkPurity(label, args) {
  const sandbox = mkdtempSync(resolve(tmpdir(), 'plan-purity-'));
  const writable = resolve(sandbox, 'writable');
  const sealed = resolve(sandbox, 'sealed');
  mkdirSync(writable);
  mkdirSync(sealed);
  try {
    const plan = run(args, writable);
    violationUnless(plan.status === 0, `${label} exited ${plan.status}: ${plan.stderr}`);
    const lines = plan.stdout.split('\n').filter((line) => line !== '');
    violationUnless(lines.length === 1, `${label} wrote ${lines.length} stdout lines, expected exactly one JSON line`);
    const parsed = JSON.parse(lines[0]);
    violationUnless(Object.keys(parsed).sort().join(',') === 'clamped,selected,shards,total',
      `${label} JSON keys are ${Object.keys(parsed).sort().join(',')}`);
    violationUnless(Array.isArray(parsed.shards) && parsed.shards.length === parsed.total, `${label} shards do not match total`);
    violationUnless(typeof parsed.selected === 'number' && typeof parsed.clamped === 'boolean', `${label} selected/clamped have wrong types`);

    const status = spawnSync('git', ['status', '--porcelain'], { cwd: feRoot, encoding: 'utf8', maxBuffer: 64 * 1024 * 1024 });
    assert(status.status === 0, `git status exited ${status.status}`);
    violationUnless(status.stdout.trim() === '', `${label} left the worktree dirty:\n${status.stdout}`);

    const leftover = readdirSync(writable);
    violationUnless(leftover.length === 0, `${label} left entries in its temp dir: ${leftover.join(', ')}`);

    // Control: prove the seal actually denies writes before trusting run 2. `chmod 500` is a no-op
    // for root (CAP_DAC_OVERRIDE bypasses the permission bits), so inside a root container run 2
    // would pass no matter how impure the command is. That is an undecidable harness, not a clean
    // plan, so fail loudly and fail closed rather than reporting a green nobody can rely on.
    chmodSync(sealed, 0o500);
    let sealHolds = false;
    try {
      rmSync(mkdtempSync(resolve(sealed, 'probe-')), { recursive: true, force: true });
    } catch {
      sealHolds = true;
    }
    assert(sealHolds, 'a read-only TMPDIR is still writable here (running as root? CAP_DAC_OVERRIDE ignores chmod 500), so the transient-state check cannot discriminate and this pin would pass vacuously');

    const sealedPlan = run(args, sealed);
    violationUnless(sealedPlan.status === 0,
      `${label} exited ${sealedPlan.status} with a read-only TMPDIR, so it uses a TMPDIR-routed temp dir while planning: ${sealedPlan.stderr}`);
    violationUnless(sealedPlan.stdout === plan.stdout,
      `${label} produced a different plan with a read-only TMPDIR:\n${plan.stdout}\nvs\n${sealedPlan.stdout}`);
  } finally {
    chmodSync(sealed, 0o700);
    rmSync(sandbox, { recursive: true, force: true });
  }
}

/** @param {string} label @param {string[]} args */
function expectPure(label, args) {
  checkPurity(label, args);
  console.log(`  pure: ${label}`);
}

/** The negative fixture: the purity checks must REPORT FAILURE on it, or they are decorative. @param {string} label @param {string[]} args */
function expectImpure(label, args) {
  try {
    checkPurity(label, args);
  } catch (error) {
    if (!(error instanceof PurityViolation)) throw error;
    console.log(`  detected as impure: ${label} — ${error.message.split('\n')[0]}`);
    return;
  }
  throw new Error(`plan purity self-check: the deliberately impure ${label} PASSED every purity assertion, so this pin cannot detect an impure plan`);
}

expectPure(`${runner} --plan`, [runner, '--plan']);

// --base exercises changedPaths / baseManifestAt — the whole PR-mode selection path, which the
// bare --plan above never reaches. Skipped rather than failed where the ref is absent (shallow
// clone, fork without the upstream remote); CI checks out with fetch-depth: 0, so it runs there.
const baseExists = spawnSync('git', ['rev-parse', '--verify', '--quiet', `${baseRef}^{commit}`],
  { cwd: feRoot, encoding: 'utf8' }).status === 0;
if (baseExists) expectPure(`${runner} --plan --base ${baseRef}`, [runner, '--plan', '--base', baseRef]);
else console.log(`  skipped: ${runner} --plan --base ${baseRef} (${baseRef} is not present in this clone)`);

expectImpure(impureFixture, [impureFixture]);

console.log(`mutation plan purity: ${baseExists ? 2 : 1} pure command(s) verified, impure fixture correctly reported as failing`);
