import { spawnSync } from 'node:child_process';
import { mkdtempSync, readFileSync, readdirSync, rmSync, writeFileSync, writeSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { resolve } from 'node:path';
import { parseArgs } from 'node:util';
import { parse } from 'yaml';
import { architecturePlugin } from '../architecture/plugin.mjs';
// @ts-ignore Node 22+ strips erasable TypeScript syntax; the build intentionally does not emit tools.
const mutationRunner = await import('./runner.ts');
const {
  boundedTestIdList, boundedVerdict, byteSequencesEqual, gitApplyDirectory, judgeMutation, manifestRelativePath,
  mutationRunExitCode, mutationShardMatrix, mutationWitnessTestPaths, oracleIdsFromDocuments,
  parseMutationTestScope, parseShard, parseVitestReport, selectedEntries, shardEntries, shardPlan,
  trackedFixtureSetMatches, unexpectedFailureDetails, validateManifest, validateWitnessCatalog,
} = mutationRunner;

const feRoot = resolve(import.meta.dirname, '../..');
const { values } = parseArgs({ options: {
  base: { type: 'string' }, plan: { type: 'boolean' }, report: { type: 'string' }, shard: { type: 'string' },
  'test-scope': { type: 'string', default: 'full' },
} });
if (values.plan && (values.shard || values.report)) throw new Error('--plan cannot be combined with --shard or --report');
const shard = values.shard ? parseShard(values.shard) : null;
const testScope = parseMutationTestScope(values['test-scope']);
const manifest = JSON.parse(readFileSync(resolve(import.meta.dirname, 'manifest.json'), 'utf8'));
const witnessCatalog = JSON.parse(readFileSync(
  resolve(feRoot, '../scripts/ci/mutation-witness-extra-paths.json'), 'utf8',
));

/** @param {string[]} args */
function git(args) {
  // `ls-files --cached` and `show <base>:manifest.json` are already >100 KB; past the 1 MiB default
  // maxBuffer git output truncates and status becomes null, which would silently look like "no baseline".
  return spawnSync('git', args, { cwd: feRoot, encoding: 'utf8', maxBuffer: 64 * 1024 * 1024 });
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

/** @param {string} mergeBase */
function changedPaths(mergeBase) {
  return checkedGit(['diff', '--no-renames', '--name-only', `${mergeBase}...HEAD`, '--']).split('\n').filter(Boolean);
}

/**
 * The manifest as of the merge base, or null when it cannot be read or parsed as an array.
 * null is the fail-closed signal: without a baseline, selection falls back to the full manifest.
 * @param {string} mergeBase
 */
function baseManifestAt(mergeBase) {
  const result = git(['show', `${mergeBase}:${gitApplyDirectory}/${manifestRelativePath}`]);
  if (result.status !== 0) return null;
  try {
    const parsed = JSON.parse(result.stdout);
    return Array.isArray(parsed) ? parsed : null;
  } catch {
    return null;
  }
}

const oracleRoot = resolve(feRoot, '../docs/oracle');
const oracleIds = oracleIdsFromDocuments(readdirSync(oracleRoot).filter((name) => name.endsWith('.yaml'))
  .map((name) => parse(readFileSync(resolve(oracleRoot, name), 'utf8'))));
const architectureRuleNames = new Set(Object.keys(architecturePlugin.rules ?? {}));
const trackedPaths = new Set(checkedGit(['ls-files', '--cached']).split('\n').filter(Boolean));
validateManifest(manifest, { oracle: oracleIds, 'arch-rule': architectureRuleNames }, trackedPaths);
validateWitnessCatalog(manifest, witnessCatalog, trackedPaths);
validateTrackedFixtures();
if (checkedGit(['status', '--porcelain']) !== '') throw new Error('mutation runner requires a clean worktree');
const mergeBase = values.base ? checkedGit(['merge-base', values.base, 'HEAD']) : null;
const changed = mergeBase === null ? [] : changedPaths(mergeBase);
const selected = mergeBase === null
  ? manifest
  : selectedEntries(manifest, changed, baseManifestAt(mergeBase), witnessCatalog);
// --plan shares this exact selection path, so a plan can never drift from the run it schedules.
if (values.plan) {
  const { total, shards, clamped } = shardPlan(selected.length, testScope);
  const matrix = mutationShardMatrix(selected, { total, shards }, testScope, witnessCatalog);
  // `process.exit` may truncate buffered stdout once the matrix grows beyond the old four scalars.
  // The plan is a subprocess protocol, so write its single line synchronously before exiting.
  writeSync(process.stdout.fd,
    `${JSON.stringify({ selected: selected.length, total, shards, clamped, matrix, test_scope: testScope })}\n`);
  process.exit(0);
}
const entriesToRun = shardEntries(selected, shard);
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
    /** @type {Record<string, string[]>} */
    let failureMessages = {};
    let reverse = null;
    let restoreError = null;
    const testFiles = testScope === 'witness' ? mutationWitnessTestPaths(entry, witnessCatalog) : [];
    try {
      test = spawnSync('npx', ['vitest', 'run', ...testFiles, '--reporter=json', `--outputFile=${jsonPath}`], {
        cwd: feRoot, encoding: 'utf8',
      });
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
        ({ failedTestIds: failed, infrastructureErrors, failureMessagesByTestId: failureMessages }
          = parseVitestReport(readFileSync(jsonPath, 'utf8')));
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
    // Names alone made an `over-red` verdict undiagnosable: you could not tell a timeout from an
    // unstable assertion from cross-test pollution without re-running CI and guessing (#1152).
    // Only the UNEXPECTED reds get details — the expected ones are the mutation working as designed.
    // EVERY id list in the record is bounded, not just failure_details: `actual_red` and
    // `verdict.errors[].test_ids` re-emit the same ids and dominate the size when a mutation reds the
    // whole suite. `boundedVerdict` touches neither `ok` nor the codes, so the exit code below is
    // computed on exactly the same verdict as before.
    report.push({ mutation_id: entry.mutation_id, test_files: testScope === 'witness' ? testFiles : null,
      expected_red: entry.expected_red,
      actual_red: boundedTestIdList(failed), verdict: boundedVerdict(verdict),
      failure_details: unexpectedFailureDetails(failed, entry.expected_red, failureMessages) });
  }
} finally {
  rmSync(temporary, { recursive: true, force: true });
}
if (checkedGit(['status', '--porcelain']) !== '') throw new Error('mutation runner left worktree dirty');
const output = JSON.stringify({ shard, test_scope: testScope, selected: selected.length, ran: entriesToRun.length,
  total: manifest.length, mutations: report }, null, 2);
if (values.report) writeFileSync(resolve(feRoot, values.report), `${output}\n`);
console.log(output);
process.exitCode = mutationRunExitCode(report.map(({ verdict }) => verdict));
