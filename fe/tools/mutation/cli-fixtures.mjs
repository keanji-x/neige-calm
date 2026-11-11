import { spawnSync } from 'node:child_process';
import { performance } from 'node:perf_hooks';
import { chmodSync, cpSync, mkdirSync, mkdtempSync, readFileSync, readdirSync, rmSync, statSync, symlinkSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { resolve } from 'node:path';

// Exercise the production CLI in one private Git repository. Dependencies are linked individually:
// Vite's .vite-temp/.vite and npm's cache stay private, never inside the shared dependency tree.
const started = performance.now();
const sourceFe = resolve(import.meta.dirname, '../..');
const sandbox = mkdtempSync(resolve(tmpdir(), 'mutation-cli-'));
const root = resolve(sandbox, 'repo');
const fe = resolve(root, 'fe');
const bin = resolve(sandbox, 'bin');
const ledger = resolve(sandbox, 'spawns.jsonl');
const targetPath = 'tools/mutation/fixtures/valid/source.ts';
const target = resolve(fe, targetPath);
const oracle = 'tools/mutation/fixture-oracle.test.ts';
const realGit = spawnSync('which', ['git'], { encoding: 'utf8' }).stdout.trim();
const realNpx = spawnSync('which', ['npx'], { encoding: 'utf8' }).stdout.trim();
/** @param {unknown} value @param {string} message */
function assert(value, message) { if (!value) throw new Error(message); }
/** @param {string[]} args */
function git(args) {
  const result = spawnSync(realGit, args, { cwd: root, encoding: 'utf8' });
  assert(result.status === 0, `fixture git ${args[0]}: ${result.stderr}`);
  return result.stdout.trim();
}
/** @param {string} path @param {string} content */
function write(path, content) { writeFileSync(resolve(root, path), content); }
/** @param {string} name @param {string} body */
function wrapper(name, body) {
  const path = resolve(bin, name);
  writeFileSync(path, `#!${process.execPath}\n${body}\n`);
  chmodSync(path, 0o755);
}
const env = { ...process.env, PATH: `${bin}:${process.env.PATH}`, npm_config_cache: resolve(sandbox, 'npm-cache'),
  MUTATION_FIXTURE_SOURCE: target };
let failures = 0;
try {
  mkdirSync(resolve(fe, 'tools'), { recursive: true });
  mkdirSync(resolve(root, 'docs'), { recursive: true });
  mkdirSync(resolve(root, 'scripts/ci'), { recursive: true });
  mkdirSync(bin);
  cpSync(resolve(sourceFe, 'tools/mutation'), resolve(fe, 'tools/mutation'), { recursive: true });
  cpSync(resolve(sourceFe, 'tools/architecture'), resolve(fe, 'tools/architecture'), { recursive: true });
  cpSync(resolve(sourceFe, '../docs/oracle'), resolve(root, 'docs/oracle'), { recursive: true });
  const modules = resolve(fe, 'node_modules');
  mkdirSync(modules);
  for (const name of readdirSync(resolve(sourceFe, 'node_modules'))) {
    if (!name.startsWith('.') || name === '.bin') symlinkSync(resolve(sourceFe, 'node_modules', name), resolve(modules, name));
  }
  write('fe/package.json', '{"type":"module"}\n');
  write('.gitignore', 'node_modules/\n.cache/\n');
  write('scripts/ci/mutation-witness-extra-paths.json', '{}\n');
  // Only the existing real oracle is needed; no product code or judgment is replaced.
  write('fe/vitest.config.mjs', `export default { cacheDir: '.cache', test: { include: [${JSON.stringify(oracle)}], maxWorkers: 1 } };\n`);
  wrapper('npx', `const {appendFileSync}=require('node:fs'); const {spawnSync}=require('node:child_process');
const result=spawnSync(${JSON.stringify(realNpx)},process.argv.slice(2),{stdio:'inherit'}); appendFileSync(${JSON.stringify(ledger)},JSON.stringify({args:process.argv.slice(2),status:result.status})+'\\n'); process.exit(result.status ?? 125);`);
  wrapper('git', `const {spawnSync}=require('node:child_process'); const args=process.argv.slice(2);
const result=spawnSync(${JSON.stringify(realGit)},args,{stdio:'inherit'});
process.exit(process.env.MUTATION_CLI_APPLY_FAILURE==='1' && args[0]==='apply' && !args.includes('--check') && !args.includes('--reverse') ? 125 : result.status ?? 125);`);
  git(['init', '-q']);
  git(['config', 'user.name', 'Mutation fixture']);
  git(['config', 'user.email', 'mutation-fixture@example.invalid']);
  const validPatch = readFileSync(resolve(fe, 'tools/mutation/fixtures/valid/mutation.diff'), 'utf8');
  const before = readFileSync(target);
  const beforeMode = statSync(target).mode;
  const entry = { mutation_id: 'cli-check-failure', defends: ['arch-rule:no-direct-persistence'], target: targetPath,
    patch: validPatch.replace('-export const guarded = true;', '-export const guarded = missing;'),
    expected_red: ['source remains guarded'], selection_paths: [oracle], why_more_than_one: 'One independent oracle.' };
  const preparedMs = performance.now() - started;
  const controlStarted = performance.now();
  // Successful unmutated oracle proves the observer forwards a real Vitest invocation transparently.
  const controlReport = resolve(sandbox, 'control.json');
  writeFileSync(ledger, '');
  const control = spawnSync('npx', ['vitest', 'run', '--reporter=json', `--outputFile=${controlReport}`], {
    cwd: fe, encoding: 'utf8', env,
  });
  assert(control.status === 0, `real Vitest positive control failed: ${control.stderr}`);
  const controlJson = JSON.parse(readFileSync(controlReport, 'utf8'));
  assert(controlJson.numPassedTests === 1 && controlJson.numFailedTests === 0, 'positive oracle did not execute');
  const observed = JSON.parse(readFileSync(ledger, 'utf8'));
  assert(observed.status === 0, 'observer did not forward the successful exit');
  console.log(JSON.stringify({ name: 'mutation production entry / transparent real Vitest positive control', ok: true, spawns: 1, prepared_ms: preparedMs, execution_ms: performance.now() - controlStarted }));
  /** @param {string} name @param {typeof entry[]} entries @param {number} expectedSpawns @param {string[]} codes @param {boolean} [applyFailure] */
  function exercise(name, entries, expectedSpawns, codes, applyFailure = false) {
    const began = performance.now();
    try {
      write('fe/tools/mutation/manifest.json', `${JSON.stringify(entries)}\n`);
      git(['add', '.']);
      git(['commit', '-qm', 'Prepare private CLI fixture']);
      writeFileSync(ledger, '');
      const preparationMs = performance.now() - began;
      const executing = performance.now();
      const result = spawnSync(process.execPath, ['tools/mutation/run.mjs', ...(codes.length ? [] : ['--test-scope', 'witness'])], {
        cwd: fe, encoding: 'utf8', env: { ...env, MUTATION_CLI_APPLY_FAILURE: applyFailure ? '1' : '0' },
      });
      const attempts = readFileSync(ledger, 'utf8').trim().split('\n').filter(Boolean).map((line) => JSON.parse(line));
      const spawns = attempts.length;
      assert(result.status === (codes.length ? 1 : 0), `expected exit 1, got ${result.status}: ${result.stderr}`);
      assert(spawns === expectedSpawns, `expected ${expectedSpawns} Vitest spawns, got ${spawns}`);
      const report = JSON.parse(result.stdout);
      assert(report.ran === entries.length && report.mutations.length === entries.length, 'ran must count reported entries');
      if (!codes.length) assert(JSON.stringify(report.mutations[0].test_files) === JSON.stringify([oracle]), 'witness scope lost oracle');
      assert(JSON.stringify(report.mutations[0].verdict.errors.map((/** @type {{code: string}} */ e) => e.code)) === JSON.stringify(codes), 'wrong failed stage');
      for (const mutation of report.mutations) {
        if (mutation.verdict.ok) {
          assert(mutation.test_run?.exit_code === 1, 'valid mutation must run real Vitest');
          assert(JSON.stringify(mutation.actual_red) === JSON.stringify(entry.expected_red), 'oracle red set differs');
        } else assert(mutation.test_run === null, 'invalid patch must report unattempted tests');
      }
      assert(attempts.every((attempt) => attempt.status === 1), 'mutated oracle must really exit red');
      console.log(JSON.stringify({ name, ok: true, spawns, case_preparation_ms: preparationMs, execution_ms: performance.now() - executing }));
    } catch (error) {
      failures++;
      console.error(JSON.stringify({ name, ok: false, error: String(error) }));
    } finally {
      assert(statSync(target).mode === beforeMode, `${name}: target mode drifted`);
      assert(before.equals(readFileSync(target)), `${name}: target bytes drifted`);
      assert(git(['status', '--porcelain']) === '', `${name}: Git worktree drifted`);
    }
  }
  exercise('mutation production entry / skips Vitest after patch check failure', [entry], 0, ['patch-check-failed']);
  const valid = { ...entry, mutation_id: 'cli-valid', patch: validPatch };
  exercise('mutation production entry / restores changed bytes after apply failure without Vitest', [valid], 0, ['patch-apply-failed'], true);
  exercise('mutation production entry / skips Vitest for a byte-identical patch',
    [{ ...valid, patch: validPatch.replace('+export const guarded = false;', '+export const guarded = true;') }], 0, ['patch-noop']);
  exercise('mutation production entry / runs the real witness oracle and restores bytes', [valid], 1, []);
  exercise('mutation production entry / continues invalid to valid and keeps aggregate failure', [entry, valid], 1, ['patch-check-failed']);
} finally {
  rmSync(sandbox, { recursive: true, force: true });
}
if (failures) process.exitCode = 1;
