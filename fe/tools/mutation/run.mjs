import { spawnSync } from 'node:child_process';
import { mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { resolve } from 'node:path';
import { parseArgs } from 'node:util';
// @ts-ignore Node 22+ strips erasable TypeScript syntax; the build intentionally does not emit tools.
const mutationRunner = await import('./runner.ts');
const {
  byteSequencesEqual, judgeMutation, parseFailedTestIds, trackedFixtureSetMatches, validateManifest, verdictExitCode,
} = mutationRunner;

const feRoot = resolve(import.meta.dirname, '../..');
const { values } = parseArgs({ options: { base: { type: 'string' }, report: { type: 'string' } } });
const manifest = JSON.parse(readFileSync(resolve(import.meta.dirname, 'manifest.json'), 'utf8'));

/** @param {string[]} args */
function git(args) {
  return spawnSync('git', args, { cwd: feRoot, encoding: 'utf8' });
}

/** @param {string[]} args */
function checkedGit(args) {
  const result = git(args);
  if (result.status !== 0) throw new Error(`git ${args[0]} failed (${result.status}): ${result.stderr}`);
  return result.stdout.trim();
}

function validateTrackedFixtures() {
  const tracked = checkedGit(['ls-files', 'tools/mutation/fixtures/*/source.ts']);
  if (!trackedFixtureSetMatches(tracked)) throw new Error('tracked fixture source set differs from the independent fixture catalog');
}

/** @param {import('./runner.ts').MutationEntry[]} entries */
function selectedEntries(entries) {
  if (!values.base) return entries;
  const mergeBase = checkedGit(['merge-base', values.base, 'HEAD']);
  const changed = new Set(checkedGit(['diff', '--name-only', `${mergeBase}...HEAD`, '--']).split('\n').filter(Boolean));
  return entries.filter((entry) => changed.has(`fe/${entry.target}`) || changed.has(entry.target));
}

validateManifest(manifest);
validateTrackedFixtures();
if (checkedGit(['status', '--porcelain']) !== '') throw new Error('mutation runner requires a clean worktree');
const selected = selectedEntries(manifest);
const report = [];
const temporary = mkdtempSync(resolve(tmpdir(), 'neige-mutation-'));
try {
  for (const entry of selected) {
    const target = resolve(feRoot, entry.target);
    const before = readFileSync(target);
    const patchPath = resolve(temporary, `${entry.mutation_id}.diff`);
    const jsonPath = resolve(temporary, `${entry.mutation_id}.json`);
    writeFileSync(patchPath, entry.patch);
    const check = git(['apply', '--directory=fe', '--check', patchPath]);
    const apply = check.status === 0 ? git(['apply', '--directory=fe', patchPath]) : { status: 125 };
    const changed = !byteSequencesEqual(before, readFileSync(target));
    const test = spawnSync('npx', ['vitest', 'run', '--reporter=json', `--outputFile=${jsonPath}`], { cwd: feRoot, encoding: 'utf8' });
    const failed = test.status === 0 || test.status === 1 ? parseFailedTestIds(readFileSync(jsonPath, 'utf8')) : [];
    const reverse = changed ? git(['apply', '--directory=fe', '--reverse', patchPath]) : null;
    const restored = byteSequencesEqual(before, readFileSync(target));
    const verdict = judgeMutation(entry, {
      failed_test_ids: failed,
      apply_check_exit_code: check.status ?? 125,
      apply_exit_code: apply.status ?? 125,
      reverse_exit_code: reverse?.status ?? null,
      target_changed_after_apply: changed,
      target_restored_after_revert: restored,
    });
    report.push({ mutation_id: entry.mutation_id, expected_red: entry.expected_red, actual_red: failed, verdict });
  }
} finally {
  rmSync(temporary, { recursive: true, force: true });
}
if (checkedGit(['status', '--porcelain']) !== '') throw new Error('mutation runner left worktree dirty');
const output = JSON.stringify({ selected: selected.length, total: manifest.length, mutations: report }, null, 2);
if (values.report) writeFileSync(resolve(feRoot, values.report), `${output}\n`);
console.log(output);
process.exitCode = Math.max(...report.map(({ verdict }) => verdictExitCode(verdict)), 0);
