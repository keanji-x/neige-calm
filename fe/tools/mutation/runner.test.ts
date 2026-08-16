import { readFileSync, readdirSync } from 'node:fs';
import { resolve } from 'node:path';
import { parse } from 'yaml';
import { describe, expect, it } from 'vitest';
import {
  byteSequencesEqual, declaredFixtureDirectories, entriesPerShard, entryIdsDriftedFromBase, judgeMutation,
  manifestRelativePath, maxShards, mutationRunExitCode, oracleIdsFromDocuments,
  parseFailedTestIds, parsePatchTarget, parseShard, parseVitestReport, runnerCodeChanged, selectedEntries, shardEntries,
  shardPlan, trackedFixtureSetMatches, validateManifest,
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
  it('selects independently by target, selection path, and runner code', () => {
    expect(selectedEntries([baseEntry], [`fe/${baseEntry.target}`], [baseEntry])).toEqual([baseEntry]);
    expect(selectedEntries([baseEntry], [`fe/${baseEntry.selection_paths[0]}`], [baseEntry])).toEqual([baseEntry]);
    expect(selectedEntries([baseEntry], ['fe/tools/mutation/run.mjs'], [baseEntry])).toEqual([baseEntry]);
    expect(selectedEntries([baseEntry], ['fe/unrelated.ts'], [baseEntry])).toEqual([]);
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
  // The manifest is JSON.parse'd: the declared types are erased at runtime, so these are load-bearing.
  it.each([
    ['numeric mutation_id', { mutation_id: Number('9007199254740993') }, /non-string mutation_id/],
    ['empty mutation_id', { mutation_id: '  ' }, /non-string mutation_id/],
    ['null mutation_id', { mutation_id: null }, /non-string mutation_id/],
    ['numeric target', { target: 42 }, /target must be a non-empty string/],
    ['numeric patch', { patch: 7 }, /patch must be a string/],
    ['numeric selection path', { selection_paths: [3] }, /selection_paths must be non-empty strings/],
    ['numeric expected_red', { expected_red: [3] }, /expected_red must be non-empty strings/],
  ] as const)('rejects a %s at runtime', (_name, override, message) => {
    const entry = { ...structuredEntry, ...override } as unknown as MutationEntry;
    expect(() => validateManifest([entry], namespaces, new Set([...tracked, ...structuredEntry.selection_paths])))
      .toThrow(message);
  });
  it('accepts the same entry with a string mutation_id', () => {
    expect(() => validateManifest([{ ...structuredEntry, mutation_id: '9007199254740993' }], namespaces, tracked)).not.toThrow();
  });
});

describe('manifest is data, not runner infrastructure', () => {
  const manifestPath = `fe/${manifestRelativePath}`;
  const entry = (id: string): MutationEntry => ({
    ...baseEntry, mutation_id: id, target: `web/src/${id}.ts`, selection_paths: [`web/src/${id}.test.ts`],
  });
  const [alpha, beta, gamma] = ['alpha', 'beta', 'gamma'].map(entry);
  const ids = (entries: readonly MutationEntry[]): string[] => entries.map(({ mutation_id }) => mutation_id);

  it('selects only the added entry when the manifest is the only changed path', () => {
    expect(ids(selectedEntries([alpha, beta, gamma], [manifestPath], [alpha, beta]))).toEqual(['gamma']);
  });

  it('selects only the edited entry when an existing patch changes', () => {
    const editedBeta = { ...beta, patch: 'diff --git a/web/src/beta.ts b/web/src/beta.ts' };
    expect(ids(selectedEntries([alpha, editedBeta, gamma], [manifestPath], [alpha, beta, gamma]))).toEqual(['beta']);
  });

  it('does not select an entry identical to base whose paths are untouched', () => {
    expect(selectedEntries([alpha], [manifestPath], [alpha])).toEqual([]);
  });

  it('treats a pure object-key reorder as no drift at all', () => {
    const reordered = ['alpha', 'beta'].map((id) => {
      const { mutation_id, defends, target, patch, expected_red, selection_paths, why_more_than_one } = entry(id);
      return { why_more_than_one, selection_paths, expected_red, patch, target, defends, mutation_id };
    });
    expect(selectedEntries([alpha, beta], [manifestPath], reordered)).toEqual([]);
    expect([...entryIdsDriftedFromBase(reordered, [alpha, beta])]).toEqual([]);
  });

  it.each([
    'fe/tools/mutation/run.mjs',
    'fe/tools/mutation/runner.ts',
    'fe/tools/mutation/runner.test.ts',
    'fe/tools/mutation/fixture-e2e.mjs',
    'fe/tools/mutation/fixtures/valid/source.ts',
  ])('selects every entry when runner code %s changes', (path) => {
    expect(ids(selectedEntries([alpha, beta, gamma], [path], [alpha, beta, gamma]))).toEqual(['alpha', 'beta', 'gamma']);
  });

  // Why validateManifest insists mutation_id is a string: two distinct integers past
  // Number.MAX_SAFE_INTEGER JSON.parse to the SAME Number, so canonical comparison would call an
  // edited entry unchanged and skip it. As strings they stay distinct and the edit is caught.
  it('distinguishes ids that collide once parsed as numbers, because they are strings', () => {
    expect(Number('9007199254740992')).toBe(Number('9007199254740993'));
    const numericBase = { ...entry('x'), mutation_id: Number('9007199254740992') } as unknown as MutationEntry;
    const numericHead = { ...entry('x'), mutation_id: Number('9007199254740993') } as unknown as MutationEntry;
    expect([...entryIdsDriftedFromBase([numericBase], [numericHead])]).toEqual([]);
    const stringBase = { ...entry('x'), mutation_id: '9007199254740992' };
    const stringHead = { ...entry('x'), mutation_id: '9007199254740993' };
    expect([...entryIdsDriftedFromBase([stringBase], [stringHead])]).toEqual(['9007199254740993']);
  });

  it('fails closed to the full manifest when the base manifest is unreadable', () => {
    expect(ids(selectedEntries([alpha, beta, gamma], [manifestPath], null))).toEqual(['alpha', 'beta', 'gamma']);
  });

  it('fails closed to the full manifest when the base contains duplicate mutation ids', () => {
    const base = [alpha, alpha, beta, gamma];
    expect(ids(selectedEntries([alpha, beta, gamma], [manifestPath], base))).toEqual(['alpha', 'beta', 'gamma']);
    expect([...entryIdsDriftedFromBase(base, [alpha, beta, gamma])].sort()).toEqual(['alpha', 'beta', 'gamma']);
  });

  it('still selects purely by path intersection when the manifest is untouched', () => {
    expect(ids(selectedEntries([alpha, beta, gamma], ['fe/web/src/beta.ts'], [alpha, beta, gamma]))).toEqual(['beta']);
    expect(ids(selectedEntries([alpha, beta, gamma], ['fe/web/src/gamma.test.ts'], [alpha, beta, gamma]))).toEqual(['gamma']);
    expect(selectedEntries([alpha, beta, gamma], ['fe/web/src/unrelated.ts'], [alpha, beta, gamma])).toEqual([]);
  });

  it('unions drift and path selection without duplicates, in manifest order', () => {
    const driftedGamma = { ...gamma, patch: 'diff --git a/web/src/gamma.ts b/web/src/gamma.ts' };
    const selected = selectedEntries(
      [alpha, beta, driftedGamma], [manifestPath, 'fe/web/src/gamma.test.ts', 'fe/web/src/alpha.ts'],
      [alpha, beta, gamma],
    );
    expect(ids(selected)).toEqual(['alpha', 'gamma']);
  });

  it('ignores entries deleted from the manifest without shifting the survivors', () => {
    const removed = entry('removed');
    const driftedBeta = { ...beta, patch: 'diff --git a/web/src/beta.ts b/web/src/beta.ts' };
    const selected = selectedEntries(
      [alpha, driftedBeta, gamma], [manifestPath, 'fe/web/src/alpha.test.ts'], [removed, alpha, beta, gamma],
    );
    expect(ids(selected)).toEqual(['alpha', 'beta']);
  });
});

describe('runner code versus manifest data', () => {
  it.each([
    'tools/mutation/run.mjs', 'tools/mutation/runner.ts', 'tools/mutation/runner.test.ts',
    'tools/mutation/fixture-e2e.mjs', 'tools/mutation/fixtures/valid/source.ts',
  ])('treats %s as runner code', (path) => {
    expect(runnerCodeChanged([path])).toBe(true);
  });
  it.each([manifestRelativePath, 'web/src/app.ts', 'tools/architecture/plugin.mjs', 'tools/mutation-other/run.mjs'])(
    'does not treat %s as runner code', (path) => {
      expect(runnerCodeChanged([path])).toBe(false);
    });
  it('detects runner code alongside a manifest edit', () => {
    expect(runnerCodeChanged([manifestRelativePath, 'tools/mutation/runner.ts'])).toBe(true);
    expect(runnerCodeChanged([])).toBe(false);
  });
});

describe('dynamic shard plan', () => {
  it.each([[0, 1], [1, 1], [4, 1], [5, 2], [65, 17], [80, 20], [128, 32]])(
    '%i selected entries plan %i shards without clamping', (selectedCount, total) => {
      expect(shardPlan(selectedCount))
        .toEqual({ total, shards: Array.from({ length: total }, (_v, i) => i + 1), clamped: false });
    });
  it('never plans fewer than one shard nor more than the cap', () => {
    expect(entriesPerShard).toBe(4);
    expect(maxShards).toBe(32);
    expect(shardPlan(0).shards).toEqual([1]);
    expect(shardPlan(10_000).total).toBe(maxShards);
  });
  // Past the cap the shards stop being ~entriesPerShard each; the plan says so out loud.
  it.each([[128, false], [129, true], [400, true], [10_000, true]])(
    '%i selected entries reports clamped=%s', (selectedCount, clamped) => {
      const plan = shardPlan(selectedCount);
      expect(plan.clamped).toBe(clamped);
      expect(plan.total).toBe(clamped ? maxShards : Math.ceil(selectedCount / entriesPerShard));
    });
  it('never reports clamped below the cap', () => {
    for (let selectedCount = 0; selectedCount <= maxShards * entriesPerShard; selectedCount += 1) {
      expect(shardPlan(selectedCount).clamped).toBe(false);
    }
  });
});

describe('mutation shards', () => {
  it('parses valid shards and rejects invalid shards', () => {
    expect(parseShard('1/4')).toEqual({ index: 1, total: 4 });
    expect(parseShard('4/4')).toEqual({ index: 4, total: 4 });
    for (const value of ['0/4', '5/4', 'a/4', '1/0', '1']) {
      expect(() => parseShard(value)).toThrow('invalid shard');
    }
  });

  it('round-robin shards form a balanced, ordered, disjoint partition', () => {
    const entries = Array.from({ length: 21 }, (_value, index) => ({
      ...baseEntry, mutation_id: `mutation-${index}`,
    }));
    const shards = Array.from({ length: 4 }, (_value, index) => shardEntries(entries, { index: index + 1, total: 4 }));
    expect(shards.flat().map(({ mutation_id }) => mutation_id).sort())
      .toEqual(entries.map(({ mutation_id }) => mutation_id).sort());
    expect(new Set(shards.flat()).size).toBe(entries.length);
    for (const shard of shards) {
      expect(shard.map((entry) => entries.indexOf(entry))).toEqual([...shard.map((entry) => entries.indexOf(entry))].sort((a, b) => a - b));
    }
    const lengths = shards.map(({ length }) => length);
    expect(Math.max(...lengths) - Math.min(...lengths)).toBeLessThanOrEqual(1);
  });

  it('returns the original entries when shard is null', () => {
    const entries = [baseEntry];
    expect(shardEntries(entries, null)).toBe(entries);
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
  it('passes an empty selection and fails an actual mutation verdict', () => {
    expect(mutationRunExitCode([])).toBe(0);
    expect(mutationRunExitCode([{ ok: false, errors: [{ code: 'dead-mutation', test_ids: [] }] }])).toBe(1);
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
