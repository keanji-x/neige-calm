import { readFileSync, readdirSync } from 'node:fs';
import { resolve } from 'node:path';
import { parse } from 'yaml';
import { describe, expect, it } from 'vitest';
import {
  boundedInfrastructureDiagnostics, boundedTestIdList, boundedVerdict,
  byteSequencesEqual, declaredFixtureDirectories, entriesPerShard, entryIdsDriftedFromBase,
  evidenceInvalidatingInfraChanged, evidenceInvalidatingRepoPathChanged, failureDetailMessageChars,
  failureDetailMessageHeadFraction,
  failureDetailMessagesPerTest, failureDetailOmittedIdLimit, failureDetailTestIdChars,
  failureDetailTestLimit, infrastructureDiagnosticBytes, judgeMutation,
  manifestRelativePath, maxShards, mutationIdPattern, mutationRunExitCode, oracleIdsFromDocuments,
  parseFailedTestIds, parsePatchTarget, parseShard, parseVitestReport, reportTestIdLimit,
  selectedEntries, shardEntries,
  shardPlan, trackedFixtureSetMatches, truncateFailureMessage, unexpectedFailureDetails, validateManifest,
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
  // run.mjs interpolates mutation_id into `resolve(temporary, `${id}.diff`)`, so it is a path
  // component: `../escaped` writes outside the temp dir and `sub/id` ENOENTs the whole shard.
  it.each([
    ['parent escape', '../escaped'], ['nested path', 'sub/id'], ['self', '.'], ['parent', '..'],
    ['backslash', 'sub\\id'], ['leading dash', '-lead'], ['trailing dash', 'trail-'],
    ['double dash', 'a--b'], ['uppercase', 'Alpha-Beta'], ['underscore', 'alpha_beta'],
    ['dotted', 'alpha.beta'], ['spaced', 'alpha beta'], ['tilde', '~/id'], ['nul-ish', 'id%00'],
  ] as const)('rejects a %s mutation_id', (_name, mutationId) => {
    expect(() => validateManifest([{ ...structuredEntry, mutation_id: mutationId }], namespaces, tracked))
      .toThrow(/non-slug mutation_id/);
  });
  it('rejects an empty mutation_id before it can reach the slug check', () => {
    expect(() => validateManifest([{ ...structuredEntry, mutation_id: '' }], namespaces, tracked))
      .toThrow(/non-string mutation_id/);
  });
  it.each(['login-success-drop-reload', 'app-cursor-accept-bare-number', 'a', '9007199254740993', 'a1-b2'])(
    'accepts the real-shaped mutation_id %s', (mutationId) => {
      expect(() => validateManifest([{ ...structuredEntry, mutation_id: mutationId }], namespaces, tracked)).not.toThrow();
    });
  it('accepts every id in the real manifest', () => {
    const manifest = JSON.parse(readFileSync(resolve(import.meta.dirname, 'manifest.json'), 'utf8')) as MutationEntry[];
    expect(manifest.length).toBeGreaterThan(0);
    expect(manifest.filter(({ mutation_id }) => !mutationIdPattern.test(mutation_id))).toEqual([]);
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

describe('evidence-invalidating infrastructure versus manifest data', () => {
  // Every path here governs which tests exist or how vitest runs them, so every recorded
  // expected_red set becomes unverifiable and selection must fail closed to the whole manifest.
  it.each([
    'tools/mutation/run.mjs', 'tools/mutation/runner.ts', 'tools/mutation/runner.test.ts',
    'tools/mutation/fixture-e2e.mjs', 'tools/mutation/fixtures/valid/source.ts', 'tools/mutation/plan-purity.mjs',
    'tools/mutation/fixtures/impure-plan.mjs',
    'vitest.config.ts', 'tools/vitest/build-constants.ts',
    'package.json', 'package-lock.json',
    'tsconfig.json', 'tsconfig.app.json', 'tsconfig.node.json', 'tsconfig.core.json',
    // run.mjs imports plugin.mjs to build the `arch-rule` namespace that validateManifest checks
    // every entry's `defends` against.
    'tools/architecture/plugin.mjs',
  ])('treats %s as evidence-invalidating infrastructure', (path) => {
    expect(evidenceInvalidatingInfraChanged([path])).toBe(true);
  });
  // Neighbours that must NOT trigger the sweep. `tools/vitestfoo.ts` / `tools/vitest-helpers/`
  // guard the startsWith prefix bug; `web/src/tsconfig.json` guards the fe-ROOT-only tsconfig rule;
  // `tools/architecture/other.mjs` guards against widening the one named architecture module into
  // the whole directory, and `allowlists.mjs` is the concrete file that widening cost ~10min a PR
  // (#1125) — it IS loaded every run via eslint.config.js, but only to feed two rules' `ignores`,
  // so a bad edit can only add reds (loud) on the three `arch-rule:` entries that own it.
  it.each([
    manifestRelativePath, 'web/src/app.ts', 'tools/architecture/other.mjs', 'tools/mutation-other/run.mjs',
    'tools/architecture/allowlists.mjs',
    'tools/architecture/no-class-dom-query.mjs', 'tools/architecture/plugin.mjs.bak',
    'tools/vitestfoo.ts', 'tools/vitest-helpers/x.ts', 'tools/vitest.ts',
    'web/src/tsconfig.json', 'core/tsconfig.build.json', 'web/package.json', 'web/package-lock.json',
    'web/vitest.config.ts', 'tools/vitest.config.ts', 'tsconfig.json.bak', 'package.json5',
  ])('does not treat %s as evidence-invalidating infrastructure', (path) => {
    expect(evidenceInvalidatingInfraChanged([path])).toBe(false);
  });
  it('detects infrastructure alongside a manifest edit', () => {
    expect(evidenceInvalidatingInfraChanged([manifestRelativePath, 'tools/mutation/runner.ts'])).toBe(true);
    expect(evidenceInvalidatingInfraChanged([manifestRelativePath, 'package-lock.json'])).toBe(true);
    expect(evidenceInvalidatingInfraChanged([])).toBe(false);
  });
  // The whole point: the sweep must reach selectedEntries, not just the predicate.
  it.each([
    'fe/tools/mutation/runner.ts', 'fe/vitest.config.ts', 'fe/tools/vitest/build-constants.ts',
    'fe/package.json', 'fe/package-lock.json', 'fe/tsconfig.json', 'fe/tsconfig.app.json',
    'fe/tools/architecture/plugin.mjs',
    // Repo-root-relative, i.e. dropped by the `fe/` filter unless it is matched before it.
    '.github/workflows/ci.yml',
  ])('selects the full manifest when %s changes', (path) => {
    const entries = ['alpha', 'beta', 'gamma'].map((id) => ({ ...baseEntry, mutation_id: id }));
    expect(selectedEntries(entries, [path], entries)).toEqual(entries);
    // …and with the manifest ALSO edited, where per-entry diff would otherwise have narrowed it.
    expect(selectedEntries(entries, [path, `fe/${manifestRelativePath}`], entries)).toEqual(entries);
  });
  it.each([
    'fe/tools/vitest-helpers/x.ts', 'fe/tools/vitestfoo.ts', 'fe/web/src/tsconfig.json',
    'fe/tools/architecture/other.mjs',
    // Only ci.yml governs how vitest runs; sibling GitHub config does not, and neither does an
    // arbitrary repo-root path — all three must stay a no-op, not become a free full sweep.
    '.github/workflows/other.yml', '.github/dependabot.yml', 'calm-server/src/main.rs',
  ])('does not sweep the manifest when the neighbouring path %s changes', (path) => {
    const entries = ['alpha', 'beta', 'gamma'].map((id) => ({ ...baseEntry, mutation_id: id }));
    expect(selectedEntries(entries, [path], entries)).toEqual([]);
  });
  // The `fe/` stripping must survive the repo-root check placed in front of it: a genuine fe path
  // alongside a repo-root path still selects by intersection, and `fe/` is stripped exactly once.
  it('keeps fe-relative selection working beside repo-root paths', () => {
    const entries = ['alpha', 'beta'].map((id) => ({
      ...baseEntry, mutation_id: id, target: `web/src/${id}.ts`, selection_paths: [`web/src/${id}.test.ts`],
    }));
    expect(selectedEntries(entries, ['.github/dependabot.yml', 'fe/web/src/beta.ts'], entries)
      .map(({ mutation_id }) => mutation_id)).toEqual(['beta']);
    expect(selectedEntries(entries, ['fe/fe/web/src/beta.ts'], entries)).toEqual([]);
  });
  it.each(['.github/workflows/ci.yml'])('treats the repo-root path %s as evidence-invalidating', (path) => {
    expect(evidenceInvalidatingRepoPathChanged([path])).toBe(true);
  });
  it.each(['.github/workflows/other.yml', '.github/dependabot.yml', 'fe/.github/workflows/ci.yml',
    'docs/.github/workflows/ci.yml', 'workflows/ci.yml'])(
    'does not treat the repo-root path %s as evidence-invalidating', (path) => {
      expect(evidenceInvalidatingRepoPathChanged([path])).toBe(false);
    });
});

// The manifest's selection_paths is a HAND-MAINTAINED list, not a computed dependency closure. These
// pin the shared test-harness modules that were measured selecting ZERO entries: a change to any of
// them can flip the recorded expected_red of an entry whose own files did not move.
describe('shared test-harness dependencies reach the entries that need them', () => {
  const manifest = JSON.parse(readFileSync(resolve(import.meta.dirname, 'manifest.json'), 'utf8')) as MutationEntry[];
  const select = (path: string): string[] =>
    selectedEntries(manifest, [`fe/${path}`], manifest).map(({ mutation_id }) => mutation_id);

  it.each([
    ['web/src/app/router/test-card-runtime.ts',
      ['session-gate-drop-query-cache-clear', 'cards-headless-filter-display-index']],
    ['web/src/systems/cards/builtins/register.ts', ['cards-headless-filter-display-index']],
    ['web/src/systems/cards/registry.ts', ['cards-headless-filter-display-index']],
  ] as const)('%s selects exactly %j', (path, expected) => {
    expect(select(path).sort()).toEqual([...expected].sort());
  });

  // Negative control against widening selection_paths to production modules generally.
  it.each(['web/src/app/router/other.ts', 'web/src/systems/cards/builtins/other.ts'])(
    'the neighbouring production path %s still selects nothing', (path) => {
      expect(select(path)).toEqual([]);
    });

  // #1125: allowlists.mjs used to sit in the global fail-closed infra set, so a routine allowlist
  // append swept all 66 entries (~10min). It is NOT unreachable from a mutation run —
  // architecture.test.ts builds `new ESLint({ cwd: <fe root> })`, which loads it through
  // eslint.config.js:9 — but there it only feeds two rules' `ignores`, and the allowlist self-checks
  // in architecture-rules.test.ts run every time, so a bad edit surfaces as over-red (fail-closed),
  // never as a silently flipped expected_red. architecture-rules.test.ts is the selection_paths file
  // of exactly these three entries — so it must select THEM, not zero (which would leave their rule
  // verdicts unverified) and not the whole manifest (which is the regression this pins).
  it('the architecture allowlist selects exactly the three arch-rule entries', () => {
    expect(select('tools/architecture/allowlists.mjs').sort()).toEqual([
      'no-class-dom-query-drop-classname-api',
      'no-class-dom-query-drop-closest',
      'no-class-dom-query-drop-module-const-resolution',
    ]);
  });

  // Its neighbour in the same directory that the RUNNER itself imports (run.mjs:7) stays global.
  it('the architecture plugin still selects the whole manifest', () => {
    expect(select('tools/architecture/plugin.mjs')).toHaveLength(manifest.length);
  });

  // Negative control for the pair above: a third file in that directory is neither, so it is a
  // no-op — the demotion must not have widened the directory into selection_paths wholesale.
  it('an unrelated architecture module selects nothing', () => {
    expect(select('tools/architecture/other.mjs')).toEqual([]);
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
      .toEqual({ failedTestIds: [], infrastructureErrors: ['hook.test.ts', 'status.test.ts'], failureMessagesByTestId: {} });
  });
});

describe('over-red failure detail evidence', () => {
  const reportOf = (assertionResults: unknown[]): string => JSON.stringify({
    testResults: [{ name: 'flake.test.ts', status: 'failed', message: '', assertionResults }],
  });
  const failing = (fullName: string, failureMessages: unknown) => ({ status: 'failed', fullName, failureMessages });

  it('keeps failureMessages from the authentic vitest reporter sample alongside the ids', () => {
    const sample = readFileSync(resolve(fixtureRoot, 'vitest-report.json'), 'utf8');
    expect(parseVitestReport(sample).failureMessagesByTestId).toEqual({
      'source remains guarded': [
        "AssertionError: expected 'export const guarded = false;' to contain 'export const guarded = true;'",
      ],
    });
  });
  it('collects nothing for passing tests and tolerates a missing failureMessages array', () => {
    const parsed = parseVitestReport(reportOf([
      { status: 'passed', fullName: 'green', failureMessages: ['ignored'] },
      { status: 'failed', fullName: 'bare' },
    ]));
    expect(parsed.failedTestIds).toEqual(['bare']);
    expect(parsed.failureMessagesByTestId).toEqual({ bare: [] });
  });
  // judgeMutation has a `duplicate-actual-red` code precisely because fullName is not unique across
  // files: overwriting would throw away the only copy of one of the two errors.
  it('accumulates messages for colliding test ids instead of overwriting', () => {
    expect(parseVitestReport(reportOf([failing('same', ['first']), failing('same', ['second'])])).failureMessagesByTestId)
      .toEqual({ same: ['first', 'second'] });
  });

  const messageHeadChars = Math.floor(failureDetailMessageChars * failureDetailMessageHeadFraction);
  const messageTailChars = failureDetailMessageChars - messageHeadChars;

  const messages = { expected: ['expected boom'], surprise: ['surprise boom'], other: ['other boom'] };
  it('reports only the unexpected reds, sorted, and never the expected ones', () => {
    expect(unexpectedFailureDetails(['surprise', 'expected', 'other'], ['expected'], messages)).toEqual({
      tests: [{ test_id: 'other', messages: ['other boom'] }, { test_id: 'surprise', messages: ['surprise boom'] }],
      omitted_test_ids: [], note: null,
    });
  });
  it('says so in the value when a test has no messages in the report', () => {
    expect(unexpectedFailureDetails(['ghost'], [], {}).tests)
      .toEqual([{ test_id: 'ghost', messages: ['[no failureMessages for this test in the vitest JSON report]'] }]);
  });
  // A vitest/TestingLibrary failure message is "verdict line, giant dump, stack": head-only
  // truncation kept 2000 characters of sidebar boilerplate on the real captured flake (#1152,
  // public.test.tsx:85) and threw away the part that would have closed the diagnosis. So both ends
  // are kept, out of the SAME budget, with the cut announced at the seam.
  it('keeps a head AND a tail, and announces both halves at the seam', () => {
    const long = `${'h'.repeat(messageHeadChars)}${'z'.repeat(37)}${'t'.repeat(messageTailChars)}`;
    const [only] = unexpectedFailureDetails(['big'], [], { big: [long] }).tests;
    expect(only.messages[0]).toBe(
      `${'h'.repeat(messageHeadChars)}`
      + `\n[truncated: kept ${messageHeadChars} head + ${messageTailChars} tail of ${long.length} characters]\n`
      + `${'t'.repeat(messageTailChars)}`,
    );
    // Same budget as head-only truncation spent: the kept CONTENT is exactly `messageChars` (the
    // exact-string assertion above is what pins that), and the only thing on top is the one-line
    // notice.
    expect(only.messages[0].length - failureDetailMessageChars).toBeLessThan(80);
  });
  // The property the head+tail split exists for, on a realistically SHAPED message rather than a run
  // of 'x': a short verdict line, a very long middle, and the discriminating detail at the very end.
  // A test that only checked lengths would have passed on the head-only version that lost it.
  it('keeps both the verdict line and the end of a TestingLibrary-shaped roles dump', () => {
    const verdict = 'TestingLibraryElementError: Unable to find an accessible element with the '
      + 'role "button" and name "Create wave"';
    const endMarker = 'dialog:\n  Name "Create wave": <div role="dialog" aria-label="Create wave" />';
    const boilerplate = Array.from({ length: 400 },
      (_v, index) => `  Name "sidebar item ${index}": <button />`).join('\n');
    const message = `${verdict}\n\nHere are the accessible roles:\n\n  button:\n${boilerplate}\n\n${endMarker}`;
    expect(message.length).toBeGreaterThan(failureDetailMessageChars * 5);
    const [emitted] = unexpectedFailureDetails(['flake'], [], { flake: [message] }).tests[0].messages;
    expect(emitted.startsWith(verdict), emitted.slice(0, 200)).toBe(true);
    expect(emitted.endsWith(endMarker), emitted.slice(-200)).toBe(true);
    expect(emitted).toContain(`[truncated: kept ${messageHeadChars} head + ${messageTailChars} tail `
      + `of ${message.length} characters]`);
    expect(emitted.length).toBeLessThan(failureDetailMessageChars + 100);
  });
  // A per-MESSAGE bound bounds nothing on its own: parseVitestReport accumulates messages across
  // colliding fullNames, so N x the char cap is unbounded in N. Measured before the per-test cap
  // existed: one test id with 200 messages of 5000 chars emitted 409,274 bytes with `note: null`.
  // So assert the bound that actually holds — on the WHOLE emitted block, not one message.
  it('bounds the whole emitted block no matter how many huge messages one test collected', () => {
    const huge = 'x'.repeat(failureDetailMessageChars * 50);
    const details = unexpectedFailureDetails(['big'], [], { big: Array.from({ length: 200 }, () => huge) });
    const emitted = JSON.stringify(details);
    expect(emitted.length)
      .toBeLessThan((failureDetailMessageChars + 200) * failureDetailMessagesPerTest + 500);
    // And it is FLAT in the message count: 6 vs 200 differ only by the digits in the notice.
    const fewer = JSON.stringify(unexpectedFailureDetails(['big'], [], { big: Array.from({ length: 6 }, () => huge) }));
    expect(emitted.length - fewer.length).toBeLessThan(10);
  });
  it('announces the dropped messages inline and in the note', () => {
    const details = unexpectedFailureDetails(['big'], [], { big: Array.from({ length: 200 }, (_v, i) => `boom ${i}`) });
    expect(details.tests[0].messages).toEqual([
      ...Array.from({ length: failureDetailMessagesPerTest }, (_v, i) => `boom ${i}`),
      `[capped: kept ${failureDetailMessagesPerTest} of 200 failure messages for this test]`,
    ]);
    expect(details.note)
      .toBe(`capped the failure messages of 1 of 1 reported test(s) at ${failureDetailMessagesPerTest} each`);
  });
  it('leaves a message exactly at the cap untouched', () => {
    const exact = 'x'.repeat(failureDetailMessageChars);
    expect(unexpectedFailureDetails(['fits'], [], { fits: [exact] }).tests[0].messages).toEqual([exact]);
  });
  it('truncates at the very first character over the cap', () => {
    const over = `${'h'.repeat(messageHeadChars)}z${'t'.repeat(messageTailChars)}`;
    expect(over.length).toBe(failureDetailMessageChars + 1);
    expect(unexpectedFailureDetails(['edge'], [], { edge: [over] }).tests[0].messages[0]).toBe(
      `${'h'.repeat(messageHeadChars)}`
      + `\n[truncated: kept ${messageHeadChars} head + ${messageTailChars} tail of ${over.length} characters]\n`
      + `${'t'.repeat(messageTailChars)}`,
    );
  });
  // The `-0` guard in truncateFailureMessage, pinned. `slice(-tail)` is the natural spelling and is
  // what any cleanup pass would reach for, but at tail === 0 the negative form is `-0` and
  // `slice(-0) === slice(0)`, which appends the WHOLE message to a zero budget. Verified by mutation:
  // restoring `slice(-tail)` reds the limit === 0 row of this table and nothing else in the suite.
  // tail === 0 needs limit === 0, reachable only through an explicit `limits` budget and never from
  // the production defaults — the invariant is what is pinned here, not a production path. The
  // neighbouring small budgets are in the table so an off-by-one in EITHER slice reds too, and the
  // head/tail strings are spelled out rather than recomputed from the fraction so that the expected
  // values do not silently follow the code they are checking.
  it('emits no message content on a zero budget, and exactly the split content just above it', () => {
    const message = 'ABCDEFGHIJ';
    const notice = (head: number, tail: number): string =>
      `\n[truncated: kept ${head} head + ${tail} tail of ${message.length} characters]\n`;
    // limit 0 (and any negative, which `Math.max(0, limit)` clamps to it) is fraction-independent:
    // floor(0 * anything) === 0, so head and tail are both empty whatever the split is.
    expect(truncateFailureMessage(message, 0)).toBe(notice(0, 0));
    expect(truncateFailureMessage(message, -7)).toBe(notice(0, 0));
    // The rest of the table is written for the 1/4 : 3/4 split; re-aiming the split re-measures it.
    expect(failureDetailMessageHeadFraction).toBe(0.25);
    const table: Array<[limit: number, head: string, tail: string]> = [
      [1, '', 'J'], [2, '', 'IJ'], [3, '', 'HIJ'],
      [4, 'A', 'HIJ'], [5, 'A', 'GHIJ'], [8, 'AB', 'EFGHIJ'],
    ];
    for (const [limit, head, tail] of table) {
      expect(truncateFailureMessage(message, limit), `limit ${limit}`)
        .toBe(`${head}${notice(head.length, tail.length)}${tail}`);
    }
  });
  // omitted_test_ids is the OVERFLOW list, so it grows exactly when the block is already at its
  // worst; and a vitest fullName is an unbounded concatenation of describe titles.
  it('caps the omitted id list itself and announces that second cap too', () => {
    const ids = Array.from({ length: failureDetailTestLimit + failureDetailOmittedIdLimit + 40 },
      (_v, index) => `t${String(index).padStart(4, '0')}`);
    const details = unexpectedFailureDetails(ids, [], {});
    const omittedCount = ids.length - failureDetailTestLimit;
    expect(details.omitted_test_ids).toHaveLength(failureDetailOmittedIdLimit);
    expect(details.omitted_test_ids)
      .toEqual(ids.slice(failureDetailTestLimit, failureDetailTestLimit + failureDetailOmittedIdLimit));
    expect(details.note).toBe(
      `capped at ${failureDetailTestLimit} of ${ids.length} unexpected-red tests; ${omittedCount} omitted (ids in omitted_test_ids). `
      + `omitted_test_ids itself capped at ${failureDetailOmittedIdLimit} of ${omittedCount} ids`,
    );
  });
  it('bounds an unbounded test name in both the reported and the omitted list', () => {
    const long = 'n'.repeat(failureDetailTestIdChars + 11);
    const capped = `${'n'.repeat(failureDetailTestIdChars)}[truncated: kept ${failureDetailTestIdChars} of ${long.length} characters]`;
    expect(unexpectedFailureDetails([long], [], { [long]: ['boom'] }).tests[0].test_id).toBe(capped);
    const ids = [...Array.from({ length: failureDetailTestLimit }, (_v, index) => `a${index}`), long];
    expect(unexpectedFailureDetails(ids, [], {}).omitted_test_ids).toEqual([capped]);
  });
  it('caps the test count, lists the omitted ids, and notes the cap', () => {
    const ids = ['t1', 't2', 't3', 't4', 't5', 't6', 't7'];
    const details = unexpectedFailureDetails(ids, [], Object.fromEntries(ids.map((id) => [id, [`${id} boom`]])));
    expect(details.tests.map(({ test_id }) => test_id)).toEqual(ids.slice(0, failureDetailTestLimit));
    expect(details.omitted_test_ids).toEqual(ids.slice(failureDetailTestLimit));
    expect(details.note).toBe(`capped at ${failureDetailTestLimit} of ${ids.length} unexpected-red tests; `
      + `${ids.length - failureDetailTestLimit} omitted (ids in omitted_test_ids)`);
  });
  it('emits an empty block when every red was expected', () => {
    expect(unexpectedFailureDetails(['expected'], ['expected'], messages))
      .toEqual({ tests: [], omitted_test_ids: [], note: null });
  });
});

// Capping `failure_details` alone did not bound the REPORT: run.mjs re-emits the same ids in
// `actual_red` and in `verdict.errors[].test_ids`, both uncapped, and both an order of magnitude
// bigger than the block that was capped when a mutation reds the whole suite (#1152).
describe('every id list in a report record is bounded', () => {
  const longId = (prefix: string, index: number): string =>
    `${prefix}${String(index).padStart(6, '0')}${'z'.repeat(failureDetailTestIdChars * 4)}`;
  const baseResult: MutationRunResult = {
    failed_test_ids: [], apply_check_exit_code: 0, apply_exit_code: 0, reverse_exit_code: 0,
    target_changed_after_apply: true, target_restored_after_revert: true,
    test_run_exit_code: 1, test_infrastructure_errors: [],
  };

  it('caps actual_red and announces the drop inside the emitted list', () => {
    const ids = Array.from({ length: reportTestIdLimit + 17 }, (_v, index) => `t${String(index).padStart(4, '0')}`);
    expect(boundedTestIdList(ids)).toEqual([
      ...ids.slice(0, reportTestIdLimit),
      `[capped: kept ${reportTestIdLimit} of ${ids.length} test ids]`,
    ]);
    // No notice at all when nothing was dropped — a notice on a complete list would be a lie.
    expect(boundedTestIdList(ids.slice(0, reportTestIdLimit))).toEqual(ids.slice(0, reportTestIdLimit));
  });

  it('truncates each surviving id, because a vitest fullName is itself unbounded', () => {
    const long = longId('red', 0);
    expect(boundedTestIdList([long])).toEqual([
      `${long.slice(0, failureDetailTestIdChars)}[truncated: kept ${failureDetailTestIdChars} of ${long.length} characters]`,
    ]);
  });

  // Through judgeMutation, not a hand-built verdict: the over-red / under-red / duplicate-* lists
  // are produced there, and capping must not disturb what the exit code is computed from.
  it('caps the verdict error id lists while leaving ok and the codes untouched', () => {
    const reds = Array.from({ length: reportTestIdLimit + 200 }, (_v, index) => `red${index}`);
    const verdict = judgeMutation({ ...baseEntry, expected_red: ['gone', 'gone'] }, {
      ...baseResult, failed_test_ids: [...reds, ...reds], test_run_exit_code: 1,
    });
    const bounded = boundedVerdict(verdict);
    expect(bounded.ok).toBe(verdict.ok);
    expect(bounded.errors.map(({ code }) => code)).toEqual(verdict.errors.map(({ code }) => code));
    expect(mutationRunExitCode([bounded])).toBe(mutationRunExitCode([verdict]));
    for (const { code, test_ids } of bounded.errors) {
      expect(test_ids.length, code).toBeLessThanOrEqual(reportTestIdLimit + 1);
      const original = verdict.errors.find((error) => error.code === code)!.test_ids;
      if (original.length > reportTestIdLimit) {
        expect(test_ids.at(-1)).toBe(`[capped: kept ${reportTestIdLimit} of ${original.length} test ids]`);
      }
    }
  });

  // The bound that actually matters, and the one every OTHER size assertion in this file misses:
  // each of those is written in terms of the constant it guards, so `failureDetailMessageChars` and
  // friends could all be raised to 10^6 with the suite green. This one is a HARD-CODED number over a
  // worst-case record assembled exactly the way run.mjs:141 assembles the real one.
  //
  // Where the number comes from: the worst case built below measures 111,844 bytes today
  // (failure_details ~45 KB + verdict ~32 KB + expected_red ~21 KB + actual_red ~12 KB). The budget
  // has to sit under the CHEAPEST single doubling, or that axis is unguarded. Measured, one at a
  // time, against this exact record:
  //
  //   baseline                                111,844
  //   failureDetailOmittedIdLimit  50 -> 100  124,295   <- the cheapest doubling
  //   failureDetailOmittedIdLimit  50 -> 150  136,745
  //   failureDetailMessageChars  2000 -> 4000 141,859
  //   failureDetailMessagesPerTest    3 -> 6  142,984
  //   failureDetailTestLimit          5 -> 10 144,877
  //   reportTestIdLimit            50 -> 100  149,497
  //   failureDetailTestIdChars    200 -> 400  179,075
  //
  // 118_000 is therefore the budget: every doubling above reds it. The earlier 131_072 did NOT —
  // `failureDetailOmittedIdLimit` could be doubled with this test still green, so the comment that
  // claimed "red if any single cap is doubled" was over-claiming on that axis. The price of the
  // tighter number is honest and stated here: ~6.2 KB of headroom (5.2%) for the wording of the
  // truncation notices, which move the total by tens of bytes, not kilobytes. A change that needs
  // more than that is a change to how much this record emits, and should re-measure the table.
  //
  // `failureDetailMessageHeadFraction` is a WEAK axis of this table, not a free one, and the earlier
  // "not an axis" claim over-stated it. What is true by construction is head + tail === messageChars,
  // i.e. the kept CONTENT is the same COUNT the head-only version kept — but that count is in UTF-16
  // code units, not bytes, and this budget is in bytes. Head-only truncation had the same property, so
  // the gap is pre-existing and not what this PR introduced: 2000 kept code units of ASCII head plus
  // CJK tail is ~5000 bytes, and the whole table above is measured against the ASCII fixtures built
  // below, where a code unit is a byte. Within those fixtures the fraction still moves the record
  // slightly, because the notice embeds the DECIMAL DIGITS of `head` and `tail`: measured, 0.25 ->
  // 111,844, 0.5 -> 111,859, 0.999 -> 111,814. That is tens of bytes, the same order as the
  // notice-wording caveat above, and it is why the headroom is stated rather than spent. Splitting
  // the budget at all moved the baseline by exactly the extra wording: 111,574 -> 111,844, i.e.
  // +18 bytes on each of the 5 tests x 3 messages the record truncates.
  //
  // Nothing clamps the fraction to [0, 1], and out of range it does buy characters rather than re-aim
  // them: measured, 1.5 -> 126,874 and -0.5 -> 411,874. So this hard-coded byte budget is the only
  // thing that reds an out-of-range split — the code that computes head/tail does not check, and every
  // other size assertion in this file is written in terms of the constants. Load-bearing for more than
  // the doubling table it was written for.
  //
  // `expected_red` is the one axis NOT capped: it is manifest data, authored by hand and gated by
  // validateManifest (today at most 13 ids of at most 124 characters, ~1.7 KB). The worst case below
  // still feeds it 26 ids of 800 characters, an order of magnitude over the real manifest, so the
  // bound holds even if that axis grows a lot.
  //
  // `infrastructureDiagnosticBytes` is invisible to THIS record on purpose: judgeMutation suppresses
  // over-red / under-red once `test-infrastructure-failed` is present, so the two record shapes are
  // mutually exclusive. That axis gets its own budgeted worst case in the next test.
  it('keeps a worst-case report record under a hard-coded byte budget', () => {
    const reds = Array.from({ length: 2000 }, (_v, index) => longId('red', index));
    const failed = [...reds, ...reds.slice(0, 500)];
    const expectedRed = Array.from({ length: 13 }, (_v, index) => longId('exp', index));
    const declared = [...expectedRed, ...expectedRed];
    const huge = 'm'.repeat(failureDetailMessageChars * 10);
    const failureMessages = Object.fromEntries(reds.slice(0, 60)
      .map((id) => [id, Array.from({ length: 200 }, () => huge)]));
    const verdict = judgeMutation({ ...baseEntry, expected_red: declared }, {
      ...baseResult, failed_test_ids: failed, test_run_exit_code: 1,
    });
    // Every error code that carries a non-empty id list is present, so nothing is under-counted.
    expect(verdict.errors.map(({ code }) => code))
      .toEqual(['duplicate-expected-red', 'duplicate-actual-red', 'under-red', 'over-red']);
    const record = {
      mutation_id: baseEntry.mutation_id,
      expected_red: declared,
      actual_red: boundedTestIdList(failed),
      verdict: boundedVerdict(verdict),
      failure_details: unexpectedFailureDetails(failed, declared, failureMessages),
    };
    expect(Buffer.byteLength(JSON.stringify(record, null, 2))).toBeLessThan(118_000);
  });

  // `test-infrastructure-failed` is the one code whose `test_ids` are NOT test ids — judgeMutation
  // puts `test_infrastructure_errors` there. Two things have to hold at once, and they pull against
  // each other, so both are pinned.
  it('leaves an infrastructure diagnostic uncapped and unmangled', () => {
    // The real shape from run.mjs:118. It is one message, not a list of ids: capping it at
    // `failureDetailTestIdChars` (200) and labelling the cut "kept N of M test ids" destroyed the
    // exact evidence this record exists to carry.
    const parseError = `report-parse-failed: Unexpected token '<', "${'x'.repeat(400)}"... is not valid JSON`;
    expect(parseError.length).toBeGreaterThan(failureDetailTestIdChars);
    const verdict = judgeMutation(baseEntry, { ...baseResult, test_infrastructure_errors: [
      'global-unhandled-error', 'src/app/shell/drawer-seam.browser.test.tsx', parseError] });
    const bounded = boundedVerdict(verdict);
    const infrastructure = bounded.errors.find(({ code }) => code === 'test-infrastructure-failed')!;
    expect(infrastructure.test_ids).toEqual([
      'global-unhandled-error', 'src/app/shell/drawer-seam.browser.test.tsx', parseError]);
    // Not just "the prefix is there": nothing was appended either, so no `[capped: ...]` /
    // `[truncated: ...]` notice claims a cut that did not happen.
    expect(infrastructure.test_ids.join('')).not.toContain('test ids');
    expect(infrastructure.test_ids.join('')).not.toContain('truncated');
  });

  // Both notices `boundedInfrastructureDiagnostics` can emit are pinned here, for the reason
  // `boundedTestIdList` pins its own: deleting BOTH notice branches left the whole suite green while
  // 272 of 403 diagnostics vanished silently, which is precisely what the contract above forbids.
  it('announces a truncated diagnostic in characters, and keeps the head that survived', () => {
    const huge = `report-parse-failed: ${'p'.repeat(infrastructureDiagnosticBytes * 3)}`;
    const bounded = boundedInfrastructureDiagnostics([huge]);
    expect(bounded).toHaveLength(1);
    const [only] = bounded;
    const announcement = /\n\[truncated: kept (\d+) of (\d+) characters\]$/.exec(only);
    expect(announcement, only.slice(0, 120)).not.toBeNull();
    const [notice, keptChars, totalChars] = announcement!;
    expect(Number(totalChars)).toBe(huge.length);
    // The number in the notice is the number of characters actually emitted — not a round constant
    // that happens to look plausible — and what precedes it is that exact prefix, byte for byte.
    expect(only).toBe(`${huge.slice(0, Number(keptChars))}${notice}`);
    expect(Number(keptChars)).toBeGreaterThan('report-parse-failed: '.length);
    expect(Number(keptChars)).toBeLessThan(huge.length);
    // ...and the announced entry itself is what the budget was spent on, quotes and escapes included.
    expect(Buffer.byteLength(JSON.stringify(only))).toBeLessThanOrEqual(infrastructureDiagnosticBytes);
  });

  it('announces how many diagnostics the byte budget dropped', () => {
    // The many-SHORT-entries worst case. Charging only `diagnostic.length` made the quotes, the
    // comma and six spaces of indentation free, so all 900 of these were admitted on an 8000-char
    // budget while costing ~18 KB on the wire.
    const diagnostics = Array.from({ length: 900 }, (_v, index) => `${index}.test.ts`);
    const bounded = boundedInfrastructureDiagnostics(diagnostics);
    const keptCount = bounded.length - 1;
    expect(keptCount).toBeLessThan(diagnostics.length);
    expect(bounded.slice(0, keptCount)).toEqual(diagnostics.slice(0, keptCount));
    expect(bounded.at(-1)).toBe(`[capped: kept ${keptCount} of ${diagnostics.length} infrastructure `
      + `diagnostics; the ${infrastructureDiagnosticBytes}-byte budget ran out]`);
  });

  // An empty diagnostic used to cost nothing, so the contract admitted an unbounded list of them.
  // judgeMutation dedupes, so production never had more than one — charging the per-element JSON
  // overhead closes it anyway, and closes it by the same rule as everything else.
  it('charges an empty diagnostic the per-element cost instead of nothing', () => {
    const bounded = boundedInfrastructureDiagnostics(Array.from({ length: 5000 }, () => ''));
    expect(bounded.length - 1).toBeLessThanOrEqual(infrastructureDiagnosticBytes / 10);
    expect(bounded.at(-1)).toContain('infrastructure diagnostics');
  });

  // Two diagnostic fixtures, one budget: ONE long parse error (what actually happens) and many short
  // legitimate filenames (what the char-only accounting let through). The second is the fixture that
  // was missing — the first one's 400 entries of ~80 chars exhaust the budget after ~100 of them, so
  // the per-element structural cost never mattered to it.
  it.each<[string, string[]]>([
    ['one huge parse error', ['global-unhandled-error', 'global-reporter-error',
      ...Array.from({ length: 400 }, (_v, index) =>
        `src/app/some/deeply/nested/module-${String(index).padStart(4, '0')}/feature.browser.test.ts`),
      `report-parse-failed: ${'p'.repeat(50_000)}`]],
    ['800 short filenames', Array.from({ length: 800 }, (_v, index) => `${index}.test.ts`)],
  ])('keeps an infrastructure-shaped record (%s) under its own hard-coded byte budget', (_label, diagnostics) => {
    // The other shape run.mjs can emit: no over-red / under-red (judgeMutation suppresses them), but
    // one diagnostic per broken test FILE plus a parse error that could itself be huge.
    const reds = Array.from({ length: 2000 }, (_v, index) => longId('red', index));
    const failed = [...reds, ...reds.slice(0, 500)];
    const expectedRed = Array.from({ length: 13 }, (_v, index) => longId('exp', index));
    const declared = [...expectedRed, ...expectedRed];
    const huge = 'm'.repeat(failureDetailMessageChars * 10);
    const failureMessages = Object.fromEntries(reds.slice(0, 60)
      .map((id) => [id, Array.from({ length: 200 }, () => huge)]));
    const verdict = judgeMutation({ ...baseEntry, expected_red: declared }, {
      ...baseResult, failed_test_ids: failed, test_infrastructure_errors: diagnostics });
    expect(verdict.errors.map(({ code }) => code))
      .toEqual(['duplicate-expected-red', 'duplicate-actual-red', 'test-infrastructure-failed']);
    const record = {
      mutation_id: baseEntry.mutation_id,
      expected_red: declared,
      actual_red: boundedTestIdList(failed),
      verdict: boundedVerdict(verdict),
      failure_details: unexpectedFailureDetails(failed, declared, failureMessages),
    };
    // Same discipline as the test above, re-measured over BOTH fixtures (parse-error / filenames):
    //
    //   baseline                                 104,325 / 105,434
    //   infrastructureDiagnosticBytes 8k -> 16k  112,762 / 114,960   <- the cheapest doubling
    //   failureDetailOmittedIdLimit    50 -> 100 116,776 / 117,885
    //   failureDetailMessageChars    2000 -> 4000 134,340 / 135,449
    //   failureDetailMessagesPerTest      3 -> 6  135,465 / 136,574
    //   failureDetailTestLimit            5 -> 10 137,358 / 138,467
    //   reportTestIdLimit              50 -> 100 129,327 / 130,436
    //   failureDetailTestIdChars      200 -> 400 158,893 / 160,002
    //
    // 110_000 sits above the worse baseline (105,434) and below the cheaper of the two cheapest
    // doublings (112,762), so every axis reds it on both fixtures. Same ~4% wording headroom caveat
    // as the test above (the head+tail split spent 270 bytes of it: 104,055 -> 104,325). Mutation-
    // verified: revert `boundedInfrastructureDiagnostics` to charging `diagnostic.length` and the
    // filenames fixture measures 114,209 — OVER this budget — because all 800 entries are admitted
    // for 8000 raw characters while costing ~18 KB on the wire.
    expect(Buffer.byteLength(JSON.stringify(record, null, 2))).toBeLessThan(110_000);
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
    expect(oracleIdsFromDocuments(documents).size).toBeGreaterThan(0);
  });
});
