import { spawnSync } from 'node:child_process';
import { mkdtempSync, readFileSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { resolve } from 'node:path';
import { describe, expect, it } from 'vitest';
import {
  byteSequencesEqual, declaredFixtureSources, judgeMutation, parseFailedTestIds, parsePatchTarget,
  trackedFixtureSetMatches, type MutationEntry, type MutationRunResult,
} from './runner';

const root = resolve(import.meta.dirname, '../..');
const fixtureRoot = resolve(import.meta.dirname, 'fixtures');
const fixtureNames = declaredFixtureSources.map((path) => path.split('/').at(-2)!).filter((name) => name !== 'valid');

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
  const check = git(['apply', '--directory=fe', '--check', patch]);
  const apply = check.status === 0 ? git(['apply', '--directory=fe', patch]) : { status: 125 };
  const changed = !byteSequencesEqual(before, readFileSync(target));
  const test = spawnSync('npx', ['vitest', 'run', 'tools/mutation/fixture-oracle.test.ts', '--reporter=json', `--outputFile=${report}`], {
    cwd: root, encoding: 'utf8', env: { ...process.env, MUTATION_FIXTURE_SOURCE: target },
  });
  expect([0, 1]).toContain(test.status);
  const failed = parseFailedTestIds(readFileSync(report, 'utf8'));
  const reverse = changed ? git(['apply', '--directory=fe', '--reverse', patch]) : null;
  const restored = byteSequencesEqual(before, readFileSync(target));
  rmSync(temporary, { recursive: true, force: true });
  return {
    failed_test_ids: failed,
    apply_check_exit_code: check.status ?? 125,
    apply_exit_code: apply.status ?? 125,
    reverse_exit_code: reverse?.status ?? null,
    target_changed_after_apply: changed,
    target_restored_after_revert: restored,
  };
}

const baseEntry: MutationEntry = {
  mutation_id: 'fixture', oracle_ids: ['GATE-FIXTURE-001'], target: declaredFixtureSources[0], patch: 'unused',
  expected_red: ['source remains guarded'], why_more_than_one: 'Fixture expects one red.',
};

describe.sequential('real git-apply failure-shape fixtures', () => {
  it.each(fixtureNames)('%s is rejected after real apply, vitest, and restoration checks', (name) => {
    const result = exerciseFixture(name);
    const verdict = judgeMutation({ ...baseEntry, target: `tools/mutation/fixtures/${name}/source.ts` }, result);
    expect(verdict.ok).toBe(false);
    expect(result.target_restored_after_revert).toBe(true);
    expect(result.failed_test_ids).toEqual([]);
    expect(result).toMatchObject({
      apply_check_exit_code: ['empty-hunk', 'illegal-context'].includes(name) ? 128 : 1,
      apply_exit_code: 125,
      reverse_exit_code: null,
      target_changed_after_apply: false,
    });
    expect(verdict.errors.map(({ code }) => code)).toEqual(['patch-check-failed', 'patch-apply-failed', 'patch-noop', 'dead-mutation']);
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
  };
  it.each([
    ['dead-mutation', { ...passing, failed_test_ids: [] }, ['dead-mutation']],
    ['under-red', { ...passing, failed_test_ids: ['different expected'] }, ['under-red', 'over-red']],
    ['over-red', { ...passing, failed_test_ids: [...passing.failed_test_ids, 'extra red'] }, ['over-red']],
    ['patch-noop', { ...passing, target_changed_after_apply: false }, ['patch-noop']],
    ['revert-drift', { ...passing, target_restored_after_revert: false }, ['revert-drift']],
  ] as const)('%s cannot pass', (_name, result, expectedCodes) => {
    expect(judgeMutation(baseEntry, result).errors.map(({ code }) => code)).toEqual(expectedCodes);
  });
});

describe('tracked fixture inventory', () => {
  it('compares the single independent catalog to the real git index in both directions', () => {
    const tracked = git(['ls-files', 'tools/mutation/fixtures/*/source.ts']);
    expect(tracked.status).toBe(0);
    expect(trackedFixtureSetMatches(tracked.stdout)).toBe(true);
  });
  it('rejects either an undeclared tracked path or a missing declared path', () => {
    const exact = declaredFixtureSources.join('\n');
    expect(trackedFixtureSetMatches(`${exact}\ntools/mutation/fixtures/extra/source.ts`)).toBe(false);
    expect(trackedFixtureSetMatches(declaredFixtureSources.slice(1).join('\n'))).toBe(false);
  });
});
