import { readFileSync, readdirSync } from 'node:fs';
import { resolve } from 'node:path';
import { parse } from 'yaml';
import { describe, expect, it } from 'vitest';
import {
  byteSequencesEqual, declaredFixtureDirectories, judgeMutation, mutationRunExitCode, oracleIdsFromDocuments,
  parseFailedTestIds, parsePatchTarget, parseVitestReport, selectedEntries, trackedFixtureSetMatches, validateManifest,
  type MutationEntry, type MutationRunResult,
} from './runner';

const fixtureRoot = resolve(import.meta.dirname, 'fixtures');

const baseEntry: MutationEntry = {
  mutation_id: 'fixture', defends: ['oracle:GATE-FIXTURE-001'], target: 'tools/architecture/no-class-dom-query.mjs', patch: 'unused',
  selection_paths: ['tools/architecture/architecture-rules.test.ts'],
  expected_red: ['source remains guarded'], why_more_than_one: 'Fixture expects one red.',
};

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
    ['infra suppresses dead mutation', { ...passing, failed_test_ids: [], test_infrastructure_errors: ['broken'] }, ['test-infrastructure-failed']],
    ['revert-drift', { ...passing, target_restored_after_revert: false }, ['revert-drift']],
  ] as const)('%s cannot pass', (_name, result, expectedCodes) => {
    expect(judgeMutation(baseEntry, result).errors.map(({ code }) => code)).toEqual(expectedCodes);
  });
});

describe('tracked fixture inventory', () => {
  const exact = declaredFixtureDirectories.flatMap((path) => [`${path}/mutation.diff`, `${path}/source.ts`]);
  it('requires the exact source and patch pair for every declared fixture', () => {
    expect(trackedFixtureSetMatches(exact.join('\n'))).toBe(true);
    expect(trackedFixtureSetMatches(exact.filter((path) => !path.endsWith('valid/mutation.diff')).join('\n'))).toBe(false);
    expect(trackedFixtureSetMatches(`${exact.join('\n')}\n${declaredFixtureDirectories[0]}/README.md`)).toBe(false);
  });
});

describe('selection and manifest judgments', () => {
  it('selects independently by target, selection path, and infrastructure', () => {
    expect(selectedEntries([baseEntry], [`fe/${baseEntry.target}`])).toEqual([baseEntry]);
    expect(selectedEntries([baseEntry], [`fe/${baseEntry.selection_paths[0]}`])).toEqual([baseEntry]);
    expect(selectedEntries([baseEntry], ['fe/tools/mutation/manifest.json'])).toEqual([baseEntry]);
    expect(selectedEntries([baseEntry], ['fe/unrelated.ts'])).toEqual([]);
  });
  const structuredEntry = { ...baseEntry, patch: readFileSync(resolve(fixtureRoot, 'valid/mutation.diff'), 'utf8'),
    target: 'tools/mutation/fixtures/valid/source.ts' };
  const namespaces = { oracle: new Set(['GATE-FIXTURE-001']), 'arch-rule': new Set(['no-class-dom-query']) };
  const tracked = new Set([structuredEntry.target, ...structuredEntry.selection_paths]);
  it('accepts both known contract namespaces', () => {
    expect(() => validateManifest([structuredEntry], namespaces, tracked)).not.toThrow();
    expect(() => validateManifest([{ ...structuredEntry, defends: ['arch-rule:no-class-dom-query'] }], namespaces, tracked)).not.toThrow();
  });
  it.each([
    [[], /incomplete structured manifest entry/],
    [['oracle:GATE-NOPE'], /unknown defended contract: oracle:GATE-NOPE/],
    [['arch-rule:nope'], /unknown defended contract: arch-rule:nope/],
    [['other:anything'], /unknown defended contract: other:anything/],
  ] as const)('rejects invalid defends %j', (defends, message) => {
    expect(() => validateManifest([{ ...structuredEntry, defends: [...defends] }], namespaces, tracked)).toThrow(message);
  });
  it('rejects empty manifests and untracked target or selection paths', () => {
    expect(() => validateManifest([], namespaces, tracked)).toThrow('manifest must contain');
    expect(() => validateManifest([{ ...structuredEntry, selection_paths: ['tools/missing.ts'] }], namespaces, tracked))
      .toThrow('path is not tracked: tools/missing.ts');
  });
});

describe('report infrastructure classification', () => {
  it('classifies global unhandled errors and reporter errors independently', () => {
    expect(parseVitestReport(JSON.stringify({ unhandledErrors: [{}], testResults: [] })).infrastructureErrors)
      .toEqual(['global-unhandled-error']);
    expect(parseVitestReport(JSON.stringify({ error: {}, testResults: [] })).infrastructureErrors)
      .toEqual(['global-reporter-error']);
  });
  it('classifies failed status and nonempty message independently without duplicates', () => {
    const statusOnly = { name: 'status.test.ts', status: 'failed', message: '', assertionResults: [] };
    const messageOnly = { name: 'hook.test.ts', status: 'passed', message: 'hook failed',
      assertionResults: [{ status: 'passed', fullName: 'a' }] };
    expect(parseVitestReport(JSON.stringify({ testResults: [statusOnly, messageOnly] })))
      .toEqual({ failedTestIds: [], infrastructureErrors: ['hook.test.ts', 'status.test.ts'] });
  });
});

describe('zero-selection exit policy', () => {
  it('passes unrelated PRs and fails infrastructure PRs with zero selections', () => {
    expect(mutationRunExitCode([], false, true)).toBe(0);
    expect(mutationRunExitCode([], true, true)).toBe(1);
  });
});

describe('oracle YAML discovery', () => {
  const parseYaml = (yaml: string): unknown => parse(yaml) as unknown;
  it('discovers ids from block, quoted, and flow YAML forms', () => {
    const documents = ['- id: plain', '- id: "quoted"', '[{ id: flow }]'].map(parseYaml);
    expect([...oracleIdsFromDocuments(documents)]).toEqual(['plain', 'quoted', 'flow']);
  });
  it('discovers a real catalog sample through the production parser path', () => {
    const oracleRoot = resolve(import.meta.dirname, '../../../docs/oracle');
    const documents = readdirSync(oracleRoot).filter((name) => name.endsWith('.yaml'))
      .map((name) => parseYaml(readFileSync(resolve(oracleRoot, name), 'utf8')));
    expect(oracleIdsFromDocuments(documents).has('INV-A11Y-001')).toBe(true);
  });
});
