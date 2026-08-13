import { spawnSync } from 'node:child_process';
import { mkdtempSync, readFileSync, readdirSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { resolve } from 'node:path';
import { parseArgs } from 'node:util';
import { parse } from 'yaml';
import { architecturePlugin } from '../architecture/plugin.mjs';
// @ts-ignore Node 22+ strips erasable TypeScript syntax; the build intentionally does not emit tools.
const mutationRunner = await import('./runner.ts');
const {
  byteSequencesEqual, gitApplyDirectory, judgeMutation, mutationRunExitCode, oracleIdsFromDocuments,
  mutationProtectedPathChanged, parseShard, parseVitestReport, selectedEntries, shardEntries, trackedFixtureSetMatches, validateManifest,
} = mutationRunner;

const feRoot = resolve(import.meta.dirname, '../..');
const { values } = parseArgs({ options: { base: { type: 'string' }, report: { type: 'string' }, shard: { type: 'string' } } });
const shard = values.shard ? parseShard(values.shard) : null;
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
  const tracked = checkedGit(['ls-files', '--cached', 'tools/mutation/fixtures/']);
  if (!trackedFixtureSetMatches(tracked)) throw new Error('tracked fixture source set differs from the independent fixture catalog');
}

function changedPaths() {
  if (!values.base) return [];
  const mergeBase = checkedGit(['merge-base', values.base, 'HEAD']);
  return checkedGit(['diff', '--no-renames', '--name-only', `${mergeBase}...HEAD`, '--']).split('\n').filter(Boolean);
}

const oracleRoot = resolve(feRoot, '../docs/oracle');
const oracleIds = oracleIdsFromDocuments(readdirSync(oracleRoot).filter((name) => name.endsWith('.yaml'))
  .map((name) => parse(readFileSync(resolve(oracleRoot, name), 'utf8'))));
const architectureRuleNames = new Set(Object.keys(architecturePlugin.rules ?? {}));
const trackedPaths = new Set(checkedGit(['ls-files', '--cached']).split('\n').filter(Boolean));
validateManifest(manifest, { oracle: oracleIds, 'arch-rule': architectureRuleNames }, trackedPaths);
validateTrackedFixtures();
if (checkedGit(['status', '--porcelain']) !== '') throw new Error('mutation runner requires a clean worktree');
const changed = values.base ? changedPaths() : [];
const selected = values.base ? selectedEntries(manifest, changed) : manifest;
const entriesToRun = shardEntries(selected, shard);
const infrastructureChanged = mutationProtectedPathChanged(changed);
const report = [];
const temporary = mkdtempSync(resolve(tmpdir(), 'neige-mutation-'));
try {
  for (const entry of entriesToRun) {
    const target = resolve(feRoot, entry.target);
    const before = readFileSync(target);
    const patchPath = resolve(temporary, `${entry.mutation_id}.diff`);
    const jsonPath = resolve(temporary, `${entry.mutation_id}.json`);
    writeFileSync(patchPath, entry.patch);
    const check = git(['apply', `--directory=${gitApplyDirectory}`, '--check', patchPath]);
    const apply = check.status === 0 ? git(['apply', `--directory=${gitApplyDirectory}`, patchPath]) : { status: 125 };
    const targetChanged = !byteSequencesEqual(before, readFileSync(target));
    let test = { status: /** @type {number | null} */ (null) };
    /** @type {string[]} */
    let failed = [];
    /** @type {string[]} */
    let infrastructureErrors = [];
    let reverse = null;
    let restoreError = null;
    try {
      test = spawnSync('npx', ['vitest', 'run', '--reporter=json', `--outputFile=${jsonPath}`], { cwd: feRoot, encoding: 'utf8' });
    } finally {
      if (apply.status === 0) reverse = git(['apply', `--directory=${gitApplyDirectory}`, '--reverse', patchPath]);
      if (!byteSequencesEqual(before, readFileSync(target))) {
        try { writeFileSync(target, before); } catch (error) { restoreError = error; }
      }
    }
    const restored = byteSequencesEqual(before, readFileSync(target));
    if (restoreError) throw new Error(`${entry.mutation_id}: byte restoration failed`, { cause: restoreError });
    if ((test.status === 0 || test.status === 1)) {
      try {
        ({ failedTestIds: failed, infrastructureErrors } = parseVitestReport(readFileSync(jsonPath, 'utf8')));
      } catch (error) {
        infrastructureErrors = [`report-parse-failed: ${error instanceof Error ? error.message : String(error)}`];
      }
    }
    const verdict = judgeMutation(entry, {
      failed_test_ids: failed,
      apply_check_exit_code: check.status ?? 125,
      apply_exit_code: apply.status ?? 125,
      reverse_exit_code: reverse?.status ?? null,
      target_changed_after_apply: targetChanged,
      target_restored_after_revert: restored,
      test_run_exit_code: test.status,
      test_infrastructure_errors: infrastructureErrors,
    });
    report.push({ mutation_id: entry.mutation_id, expected_red: entry.expected_red, actual_red: failed, verdict });
  }
} finally {
  rmSync(temporary, { recursive: true, force: true });
}
if (checkedGit(['status', '--porcelain']) !== '') throw new Error('mutation runner left worktree dirty');
const output = JSON.stringify({ shard, selected: selected.length, ran: entriesToRun.length, total: manifest.length, mutations: report }, null, 2);
if (values.report) writeFileSync(resolve(feRoot, values.report), `${output}\n`);
console.log(output);
process.exitCode = mutationRunExitCode(
  report.map(({ verdict }) => verdict), infrastructureChanged, Boolean(values.base), selected.length,
);
