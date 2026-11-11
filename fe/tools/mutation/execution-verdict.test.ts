import { describe, expect, it } from 'vitest';
import { judgeMutation, type MutationEntry, type MutationRunResult } from './runner.ts';

const entry: MutationEntry = {
  mutation_id: 'execution-verdict', target: 'source.ts', patch: '', defends: ['oracle:guard'],
  expected_red: ['guard'], selection_paths: ['guard.test.ts'], why_more_than_one: 'One oracle.',
};
const executed: MutationRunResult = {
  apply_check_exit_code: 0, apply_exit_code: 0, reverse_exit_code: 0,
  target_changed_after_apply: true, target_restored_after_revert: true,
  test_run: { exit_code: 1 }, failed_test_ids: ['guard'], test_infrastructure_errors: [],
};
function codes(result: MutationRunResult) {
  return judgeMutation(entry, result).errors.map(({ code }) => code);
}

describe('mutation execution verdict', () => {
  it.each([
    ['check', { apply_check_exit_code: 1, apply_exit_code: null, reverse_exit_code: null }, 'patch-check-failed'],
    ['apply', { apply_check_exit_code: 0, apply_exit_code: 125, reverse_exit_code: null }, 'patch-apply-failed'],
    ['noop', { apply_check_exit_code: 0, apply_exit_code: 0, reverse_exit_code: 0 }, 'patch-noop'],
  ] as const)('reports only the failed %s stage for skipped tests', (_name, stage, code) => {
    expect(codes({ ...executed, ...stage, target_changed_after_apply: false,
      failed_test_ids: [], test_run: null })).toEqual([code]);
  });

  it.each([null, { exit_code: null }, { exit_code: 125 }])('rejects missing or failed execution %j for valid bytes', (test_run) => {
    expect(codes({ ...executed, test_run, failed_test_ids: [] })).toEqual(['test-run-failed']);
  });

  it('keeps red-set, duplicate and infrastructure judgments for executed mutations', () => {
    expect(codes(executed)).toEqual([]);
    expect(codes({ ...executed, failed_test_ids: [] })).toEqual(['dead-mutation']);
    expect(codes({ ...executed, failed_test_ids: ['extra'] })).toEqual(['under-red', 'over-red']);
    expect(codes({ ...executed, failed_test_ids: ['guard', 'guard'] })).toEqual(['duplicate-actual-red']);
    expect(judgeMutation({ ...entry, expected_red: ['guard', 'guard'] }, executed).errors.map(({ code }) => code))
      .toEqual(['duplicate-expected-red']);
    expect(codes({ ...executed, failed_test_ids: [], test_infrastructure_errors: ['suite-failed'] }))
      .toEqual(['test-infrastructure-failed']);
  });

  it.each([null, 125, 1])('rejects missing or failed reverse %j even after byte fallback', (reverse_exit_code) => {
    expect(codes({ ...executed, reverse_exit_code })).toEqual(['revert-failed']);
  });

  it('rejects drift even for an invalid patch with skipped tests', () => {
    expect(codes({ ...executed, apply_check_exit_code: 1, apply_exit_code: null, reverse_exit_code: null,
      test_run: null, failed_test_ids: [], target_changed_after_apply: false, target_restored_after_revert: false }))
      .toEqual(['patch-check-failed', 'revert-drift']);
  });
});
