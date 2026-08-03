import { spawnSync } from 'node:child_process';
import { mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { resolve } from 'node:path';
// @ts-ignore Node 22+ strips erasable TypeScript syntax; the build intentionally does not emit tools.
const { byteSequencesEqual, declaredFixtureDirectories, gitApplyDirectory, parseFailedTestIds } = await import('./runner.ts');

const feRoot = resolve(import.meta.dirname, '../..');

/** @param {string[]} args */
function git(args) {
  return spawnSync('git', args, { cwd: feRoot, encoding: 'utf8' });
}

/** @param {unknown} condition @param {string} message */
function assert(condition, message) {
  if (!condition) throw new Error(message);
}

/** @param {string} name */
function exerciseFixture(name) {
  const directory = resolve(import.meta.dirname, 'fixtures', name);
  const target = resolve(directory, 'source.ts');
  const patch = resolve(directory, 'mutation.diff');
  const before = readFileSync(target);
  const temporary = mkdtempSync(resolve(tmpdir(), `mutation-fixture-${name}-`));
  const report = resolve(temporary, 'vitest.json');
  const check = git(['apply', `--directory=${gitApplyDirectory}`, '--check', patch]);
  const apply = check.status === 0 ? git(['apply', `--directory=${gitApplyDirectory}`, patch]) : { status: 125 };
  let reverse = null;
  try {
    const changed = !byteSequencesEqual(before, readFileSync(target));
    const test = spawnSync('npx', ['vitest', 'run', 'tools/mutation/fixture-oracle.test.ts', '--reporter=json', `--outputFile=${report}`], {
      cwd: feRoot, encoding: 'utf8', env: { ...process.env, MUTATION_FIXTURE_SOURCE: target },
    });
    assert(test.status === 0 || test.status === 1, `${name}: vitest exited ${test.status}`);
    const failed = parseFailedTestIds(readFileSync(report, 'utf8'));
    if (name === 'valid') {
      assert(check.status === 0 && apply.status === 0 && changed, 'valid: mutation did not apply and change bytes');
      assert(failed.length === 1 && failed[0] === 'source remains guarded', 'valid: oracle did not turn red exactly once');
    } else if (name === 'mode-only') {
      assert(check.status === 0 && apply.status === 0 && !changed && failed.length === 0, 'mode-only: unexpected result');
    } else {
      assert(check.status !== 0 && apply.status === 125 && !changed && failed.length === 0, `${name}: invalid patch was not rejected`);
    }
  } finally {
    if (apply.status === 0) reverse = git(['apply', `--directory=${gitApplyDirectory}`, '--reverse', patch]);
    if (!byteSequencesEqual(before, readFileSync(target))) writeFileSync(target, before);
    rmSync(temporary, { recursive: true, force: true });
  }
  assert(byteSequencesEqual(before, readFileSync(target)), `${name}: target bytes were not restored`);
  if (apply.status === 0) assert(reverse?.status === 0, `${name}: reverse apply failed`);
}

for (const directory of declaredFixtureDirectories) {
  const name = directory.split('/').at(-1);
  if (name === undefined) throw new Error(`invalid fixture directory: ${directory}`);
  exerciseFixture(name);
}
console.log(`mutation fixture e2e: ${declaredFixtureDirectories.length}/${declaredFixtureDirectories.length} passed`);
