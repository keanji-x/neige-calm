import { spawnSync } from 'node:child_process';
import { chmodSync, mkdtempSync, mkdirSync, readdirSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { resolve } from 'node:path';

/**
 * Executable pin for `run.mjs --plan`: planning must be a READ of the worktree. Run 1 (private
 * TMPDIR) checks the end state; run 2 (TMPDIR sealed read-only) catches create-then-delete under
 * TMPDIR. Not caught: a worktree write-then-delete with no temp-dir use, or writes outside TMPDIR.
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
    /** @type {{selected: number, total: number, shards: number[], clamped: boolean,
     * matrix: Array<{shard: number, browser: boolean}>, test_scope: string}} */
    const parsed = JSON.parse(lines[0]);
    violationUnless(Object.keys(parsed).sort().join(',') === 'clamped,matrix,selected,shards,test_scope,total',
      `${label} JSON keys are ${Object.keys(parsed).sort().join(',')}`);
    violationUnless(Array.isArray(parsed.shards) && parsed.shards.length === parsed.total, `${label} shards do not match total`);
    violationUnless(Array.isArray(parsed.matrix) && parsed.matrix.length === parsed.total,
      `${label} matrix does not match total`);
    violationUnless(parsed.matrix.every((entry, index) => entry.shard === parsed.shards[index]
      && typeof entry.browser === 'boolean'), `${label} matrix entries do not match shards/browser schema`);
    violationUnless(typeof parsed.selected === 'number' && typeof parsed.clamped === 'boolean', `${label} selected/clamped have wrong types`);
    violationUnless(parsed.test_scope === 'full' || parsed.test_scope === 'witness', `${label} has invalid test_scope`);

    const status = spawnSync('git', ['status', '--porcelain'], { cwd: feRoot, encoding: 'utf8', maxBuffer: 64 * 1024 * 1024 });
    assert(status.status === 0, `git status exited ${status.status}`);
    violationUnless(status.stdout.trim() === '', `${label} left the worktree dirty:\n${status.stdout}`);

    const leftover = readdirSync(writable);
    violationUnless(leftover.length === 0, `${label} left entries in its temp dir: ${leftover.join(', ')}`);

    // Control: prove the seal denies writes. `chmod 500` is a no-op for root (CAP_DAC_OVERRIDE), so
    // inside a root container run 2 would pass vacuously — fail closed instead.
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
expectPure(`${runner} --plan --test-scope witness`, [runner, '--plan', '--test-scope', 'witness']);

// --base exercises the whole PR-mode selection path; skipped where the ref is absent (shallow clone).
const baseExists = spawnSync('git', ['rev-parse', '--verify', '--quiet', `${baseRef}^{commit}`],
  { cwd: feRoot, encoding: 'utf8' }).status === 0;
if (baseExists) expectPure(`${runner} --plan --base ${baseRef}`, [runner, '--plan', '--base', baseRef]);
else console.log(`  skipped: ${runner} --plan --base ${baseRef} (${baseRef} is not present in this clone)`);

expectImpure(impureFixture, [impureFixture]);

console.log(`mutation plan purity: ${baseExists ? 3 : 2} pure command(s) verified, impure fixture correctly reported as failing`);
