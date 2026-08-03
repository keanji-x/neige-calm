import { spawnSync } from 'node:child_process';
import { mkdtempSync, readFileSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { resolve } from 'node:path';
import { describe, expect, it } from 'vitest';
import {
  byteSequencesEqual, declaredFixtureDirectories, gitApplyDirectory, judgeMutation, parseFailedTestIds, parsePatchTarget,
  parseVitestReport, selectedEntries, trackedFixtureSetMatches, validateManifest, type MutationEntry, type MutationRunResult,
} from './runner';

const root = resolve(import.meta.dirname, '../..');
const fixtureRoot = resolve(import.meta.dirname, 'fixtures');
const fixtureNames = declaredFixtureDirectories.map((path) => path.split('/').at(-1)!).filter((name) => name !== 'valid');

function git(args: string[]) {
  return spawnSync('git', args, { cwd: root, encoding: 'utf8' });
}

function exerciseFixture(name: string): MutationRunResult {
  const directory = resolve(fixtureRoot, name);
  const target = resolve(directory, 'source.ts');
  const patch = resolve(directory, 'mutation.diff');
  const before = readFileSync(target);
  const temporary = mkdtempSync(resolve(tmpdir(), `mutation-fixture-${name}-`));
  const report = resolve(temporary, 'vitest.json');
  const check = git(['apply', `--directory=${gitApplyDirectory}`, '--check', patch]);
  const apply = check.status === 0 ? git(['apply', `--directory=${gitApplyDirectory}`, patch]) : { status: 125 };
  const changed = !byteSequencesEqual(before, readFileSync(target));
  const test = spawnSync('npx', ['vitest', 'run', 'tools/mutation/fixture-oracle.test.ts', '--reporter=json', `--outputFile=${report}`], {
    cwd: root, encoding: 'utf8', env: { ...process.env, MUTATION_FIXTURE_SOURCE: target },
  });
  expect([0, 1]).toContain(test.status);
  const failed = parseFailedTestIds(readFileSync(report, 'utf8'));
  const reverse = apply.status === 0 ? git(['apply', `--directory=${gitApplyDirectory}`, '--reverse', patch]) : null;
  const restored = byteSequencesEqual(before, readFileSync(target));
  rmSync(temporary, { recursive: true, force: true });
  return {
    failed_test_ids: failed,
    apply_check_exit_code: check.status ?? 125,
    apply_exit_code: apply.status ?? 125,
    reverse_exit_code: reverse?.status ?? null,
    target_changed_after_apply: changed,
    target_restored_after_revert: restored,
    test_run_exit_code: test.status,
    test_infrastructure_errors: [],
  };
}

const baseEntry: MutationEntry = {
  mutation_id: 'fixture', defends: ['oracle:GATE-FIXTURE-001'], target: `${declaredFixtureDirectories[0]}/source.ts`, patch: 'unused',
  selection_paths: ['fe/tools/mutation/fixture-oracle.test.ts'],
  expected_red: ['source remains guarded'], why_more_than_one: 'Fixture expects one red.',
};

describe.sequential('real git-apply failure-shape fixtures', () => {
  it.each(fixtureNames)('%s is rejected after real apply, vitest, and restoration checks', (name) => {
    const result = exerciseFixture(name);
    const verdict = judgeMutation({ ...baseEntry, target: `tools/mutation/fixtures/${name}/source.ts` }, result);
    expect(verdict.ok).toBe(false);
    expect(result.target_restored_after_revert).toBe(true);
    expect(result.failed_test_ids).toEqual([]);
    expect(result.apply_check_exit_code === 0).toBe(name === 'mode-only');
    expect(result).toMatchObject({
      apply_exit_code: name === 'mode-only' ? 0 : 125,
      reverse_exit_code: name === 'mode-only' ? 0 : null,
      target_changed_after_apply: false,
    });
    expect(verdict.errors.map(({ code }) => code)).toEqual(name === 'mode-only'
      ? ['patch-noop', 'dead-mutation']
      : ['patch-check-failed', 'patch-apply-failed', 'patch-noop', 'dead-mutation']);
  }, 30_000);

  it('valid control really changes bytes, turns the oracle red, and reverses cleanly', () => {
    const result = exerciseFixture('valid');
    const verdict = judgeMutation({ ...baseEntry, target: 'tools/mutation/fixtures/valid/source.ts' }, result);
    expect(result).toEqual({
      failed_test_ids: ['source remains guarded'],
      apply_check_exit_code: 0,
      apply_exit_code: 0,
      reverse_exit_code: 0,
      target_changed_after_apply: true,
      target_restored_after_revert: true,
      test_run_exit_code: 1,
      test_infrastructure_errors: [],
    });
    expect(verdict).toEqual({ ok: true, errors: [] });
  }, 30_000);
});

describe('tested parsing and byte verdict inputs', () => {
  it('parses an authentic vitest JSON reporter sample', () => {
    expect(parseFailedTestIds(readFileSync(resolve(import.meta.dirname, 'fixtures/vitest-report.json'), 'utf8')))
      .toEqual(['source remains guarded']);
  });
  it('parses a git-generated diff with an index header', () => {
    const patch = readFileSync(resolve(fixtureRoot, 'context-mismatch/mutation.diff'), 'utf8');
    expect(parsePatchTarget(patch)).toBe('tools/mutation/fixtures/context-mismatch/source.ts');
  });
  it('detects same-length byte drift', () => {
    expect(byteSequencesEqual(new Uint8Array([1, 2]), new Uint8Array([1, 3]))).toBe(false);
  });
});

describe('pure verdict edge cases', () => {
  const passing: MutationRunResult = {
    failed_test_ids: ['source remains guarded'], apply_check_exit_code: 0, apply_exit_code: 0, reverse_exit_code: 0,
    target_changed_after_apply: true, target_restored_after_revert: true,
    test_run_exit_code: 1, test_infrastructure_errors: [],
  };
  it.each([
    ['dead-mutation', { ...passing, failed_test_ids: [] }, ['dead-mutation']],
    ['under-red', { ...passing, failed_test_ids: ['different expected'] }, ['under-red', 'over-red']],
    ['over-red', { ...passing, failed_test_ids: [...passing.failed_test_ids, 'extra red'] }, ['over-red']],
    ['patch-noop', { ...passing, target_changed_after_apply: false }, ['patch-noop']],
    ['patch-check-failed', { ...passing, apply_check_exit_code: 1 }, ['patch-check-failed']],
    ['patch-apply-failed', { ...passing, apply_exit_code: 1 }, ['patch-apply-failed']],
    ['revert-failed', { ...passing, reverse_exit_code: 1 }, ['revert-failed']],
    ['test-run-failed', { ...passing, failed_test_ids: [], test_run_exit_code: null }, ['test-run-failed']],
    ['test-infrastructure-failed', { ...passing, test_infrastructure_errors: ['broken suite'] }, ['test-infrastructure-failed']],
    ['revert-drift', { ...passing, target_restored_after_revert: false }, ['revert-drift']],
  ] as const)('%s cannot pass', (_name, result, expectedCodes) => {
    expect(judgeMutation(baseEntry, result).errors.map(({ code }) => code)).toEqual(expectedCodes);
  });
});

describe('tracked fixture inventory', () => {
  it('compares the single independent catalog to the real git index in both directions', () => {
    const tracked = git(['ls-files', '--cached', '--others', '--exclude-standard', 'tools/mutation/fixtures/']);
    expect(tracked.status).toBe(0);
    expect(trackedFixtureSetMatches(tracked.stdout)).toBe(true);
  });
  it('rejects either an undeclared tracked path or a missing declared path', () => {
    const exact = declaredFixtureDirectories.map((path) => `${path}/source.ts`).join('\n');
    expect(trackedFixtureSetMatches(`${exact}\ntools/mutation/fixtures/extra/source.ts`)).toBe(false);
    expect(trackedFixtureSetMatches(declaredFixtureDirectories.slice(1).map((path) => `${path}/source.ts`).join('\n'))).toBe(false);
  });
});

describe('selection and manifest judgments', () => {
  it('locks the repository-root plus --directory layout assumption', () => {
    expect(resolve(root, '..', gitApplyDirectory, baseEntry.target)).toBe(resolve(fixtureRoot, 'already-applied/source.ts'));
  });
  it('selects by target, oracle dependency, infrastructure, and both rename paths', () => {
    expect(selectedEntries([baseEntry], [`fe/${baseEntry.target}`])).toEqual([baseEntry]);
    expect(selectedEntries([baseEntry], [baseEntry.selection_paths[0]])).toEqual([baseEntry]);
    expect(selectedEntries([baseEntry], ['fe/tools/mutation/manifest.json'])).toEqual([baseEntry]);
    expect(selectedEntries([baseEntry], ['fe/unrelated.ts'])).toEqual([]);
  });
  const structuredEntry = { ...baseEntry, patch: readFileSync(resolve(fixtureRoot, 'valid/mutation.diff'), 'utf8'),
    target: 'tools/mutation/fixtures/valid/source.ts' };
  const namespaces = { oracle: new Set(['GATE-FIXTURE-001']), 'arch-rule': new Set(['no-class-dom-query']) };
  it('accepts both known contract namespaces', () => {
    expect(() => validateManifest([structuredEntry], namespaces)).not.toThrow();
    expect(() => validateManifest([{ ...structuredEntry, defends: ['arch-rule:no-class-dom-query'] }], namespaces)).not.toThrow();
  });
  it.each([
    [[], /incomplete structured manifest entry/],
    [['oracle:GATE-NOPE'], /unknown defended contract: oracle:GATE-NOPE/],
    [['arch-rule:nope'], /unknown defended contract: arch-rule:nope/],
    [['other:anything'], /unknown defended contract: other:anything/],
  ] as const)('rejects invalid defends %j', (defends, message) => {
    expect(() => validateManifest([{ ...structuredEntry, defends: [...defends] }], namespaces)).toThrow(message);
  });
  it('classifies a failed file without a failed assertion as infrastructure failure', () => {
    const report = JSON.stringify({ testResults: [{ name: 'broken.test.ts', status: 'failed', message: 'import failed', assertionResults: [] }] });
    expect(parseVitestReport(report)).toEqual({ failedTestIds: [], infrastructureErrors: ['broken.test.ts'] });
  });
});
