import { readFileSync } from 'node:fs';
import { resolve } from 'node:path';
import { parse } from 'yaml';
import { describe, expect, it } from 'vitest';
import vitestConfig from '../../vitest.config';
import playwrightConfig from '../../playwright.config';
import { checkTestTier, type TierEntry, tierGateViolations, validateTierEntries } from './checker';
import { playwrightProjectFromConfig, testProjectsFromConfig } from './project-map';

const configRoot = resolve(import.meta.dirname, '../..');
const repoRoot = resolve(configRoot, '..');
const fixtureRoot = resolve(import.meta.dirname, 'fixtures');
const fixtureManifest = Object.freeze({
  positive: Object.freeze(['browser.browser.test.ts', 'data.yaml', 'static.test.ts']),
  negative: Object.freeze(['data.yaml', 'orphan.spec.ts', 'wrong-browser.test.ts']),
  decisions: Object.freeze([
    'browser-probe-misassigned.yaml', 'overlap.yaml', 'playwright-owned.yaml', 'unassigned.yaml',
  ]),
});

interface DecisionFixture {
  trackedTest: string;
  projects: { name: string; include: string[]; exclude: string[] }[];
  expected: string | null;
}

function decisionFixture(name: string): DecisionFixture {
  return parse(readFileSync(resolve(fixtureRoot, 'decisions', name), 'utf8')) as DecisionFixture;
}

function fixtureEntries(kind: keyof typeof fixtureManifest): TierEntry[] {
  const value: unknown = parse(readFileSync(resolve(fixtureRoot, kind, 'data.yaml'), 'utf8'));
  if (!Array.isArray(value)) throw new Error(`${kind} fixture must be a YAML sequence`);
  return value.map((entry: unknown) => entry as TierEntry);
}

describe('test-tier-project fixtures', () => {
  const projects = testProjectsFromConfig(vitestConfig);
  it('accepts the positive directory', () => {
    expect(checkTestTier(fixtureEntries('positive'), projects, repoRoot, configRoot)).toEqual([]);
  });
  it('rejects every negative source with its own rule', () => {
    const violations = checkTestTier(fixtureEntries('negative'), projects, repoRoot, configRoot);
    const negativeSources = fixtureManifest.negative.filter((path) => /\.(?:ts|tsx)$/.test(path));
    expect(violations).toHaveLength(2);
    expect(new Set(violations.map(({ rule }) => rule))).toEqual(new Set(['test-tier-project']));
    expect(new Set(violations.map(({ source }) => source.split('/').at(-1)))).toEqual(new Set(negativeSources));
  });

  it.each([
    ['browser', 'web-dom', 'probe.test.ts', false],
    ['browser', 'platform-independent', 'probe.test.ts', false],
    ['jsdom', 'platform-independent', 'probe.test.ts', false],
    ['jsdom', 'browser', 'probe.browser.test.ts', true],
    ['static', 'web-dom', 'probe.test.ts', true],
    ['static', 'platform-independent', 'probe.test.ts', true],
  ])('checks whether %s tier has sufficient capability in %s', (tier, projectName, path, accepted) => {
    const isolatedProjects = [{ name: projectName, include: [path], exclude: [] }];
    const entry = { id: 'GATE-TIER-MAP-001', migration: 'migrated', test_tier: tier, authoritative_test: `fe/${path}:1` };
    expect(checkTestTier([entry], isolatedProjects, '/repo', '/repo/fe')).toHaveLength(accepted ? 0 : 1);
  });

  it('accepts a migrated jsdom authoritative test collected by web-dom', () => {
    const webDom = [{ name: 'web-dom', include: ['web/src/app/probe.test.ts'], exclude: [] }];
    const entry = { id: 'GATE-TIER-JSDOM-POSITIVE', migration: 'migrated', test_tier: 'jsdom', authoritative_test: 'fe/web/src/app/probe.test.ts:1' };
    expect(checkTestTier([entry], webDom, '/repo', '/repo/fe')).toEqual([]);
  });

  it('rejects a migrated jsdom authoritative test collected by platform-independent', () => {
    const nodeOnly = [{ name: 'platform-independent', include: ['web/src/app/probe.test.ts'], exclude: [] }];
    const entry = { id: 'GATE-TIER-JSDOM-NEGATIVE', migration: 'migrated', test_tier: 'jsdom', authoritative_test: 'fe/web/src/app/probe.test.ts:1' };
    expect(checkTestTier([entry], nodeOnly, '/repo', '/repo/fe')).toHaveLength(1);
  });

  it('rejects a test collected by the expected project and an extra project', () => {
    const overlapping = [
      { name: 'browser', include: ['probe.browser.test.ts'], exclude: [] },
      { name: 'platform-independent', include: ['*.test.ts'], exclude: [] },
    ];
    const entry = { id: 'GATE-TIER-MAP-002', migration: 'migrated', test_tier: 'browser', authoritative_test: 'fe/probe.browser.test.ts:1' };
    expect(checkTestTier([entry], overlapping, '/repo', '/repo/fe')).toHaveLength(1);
  });

  it('rejects a wrong tier even when exactly one project collects the test', () => {
    const nodeOnly = [{ name: 'platform-independent', include: ['probe.test.ts'], exclude: [] }];
    const entry = { id: 'GATE-TIER-MAP-004', migration: 'migrated', test_tier: 'browser', authoritative_test: 'fe/probe.test.ts:1' };
    expect(checkTestTier([entry], nodeOnly, '/repo', '/repo/fe')).toHaveLength(1);
  });

  it('accepts a browser-tier test collected by the Playwright testDir', () => {
    const entry = { id: 'GATE-TIER-PLAYWRIGHT', migration: 'migrated', test_tier: 'browser', authoritative_test: 'fe/e2e/probe.spec.ts:1' };
    expect(checkTestTier([entry], [playwrightProjectFromConfig(playwrightConfig, configRoot)], '/repo', '/repo/fe')).toEqual([]);
  });

  it('enforces tier mismatch only after migration', () => {
    const nodeOnly = [{ name: 'platform-independent', include: ['probe.test.ts'], exclude: [] }];
    const mismatch = { test_tier: 'browser', authoritative_test: 'fe/probe.test.ts:1' };
    const migrated = { id: 'GATE-TIER-MIGRATED', migration: 'migrated', ...mismatch };
    const pending = { id: 'GATE-TIER-PENDING', migration: 'pending', ...mismatch };

    expect(checkTestTier([migrated], nodeOnly, '/repo', '/repo/fe')).toHaveLength(1);
    expect(checkTestTier([pending], nodeOnly, '/repo', '/repo/fe')).toEqual([]);
  });

  it('rejects migrated entries without a real authoritative test', () => {
    expect(checkTestTier([{ id: 'GATE-TIER-NONE-001', migration: 'migrated', test_tier: 'static',
      authoritative_test: 'NONE' }], [], '/repo', '/repo/fe')).toEqual([{
      id: 'GATE-TIER-NONE-001', rule: 'test-tier-project', source: 'NONE',
      message: 'migrated entries require a real authoritative_test location',
    }]);
  });

  it('rejects overlap for jsdom as well as browser', () => {
    const overlapping = [
      { name: 'web-dom', include: ['probe.test.tsx'], exclude: [] },
      { name: 'platform-independent', include: ['*.test.tsx'], exclude: [] },
    ];
    const entry = { id: 'GATE-TIER-MAP-005', migration: 'migrated', test_tier: 'jsdom', authoritative_test: 'fe/probe.test.tsx:1' };
    expect(checkTestTier([entry], overlapping, '/repo', '/repo/fe')).toHaveLength(1);
  });

  it.each([
    ['unknown migration', { migration: 'done', test_tier: 'static', authoritative_test: 'fe/probe.test.ts:1' }],
    ['unknown test tier', { migration: 'pending', test_tier: 'brower', authoritative_test: 'fe/probe.test.ts:1' }],
    ['non-string authoritative test', { migration: 'pending', test_tier: 'static', authoritative_test: 42 }],
    ['malformed authoritative location', { migration: 'pending', test_tier: 'static', authoritative_test: 'fe/probe.test.ts' }],
  ])('fails closed for %s', (_label, entry) => {
    expect(() => validateTierEntries([entry])).toThrow();
  });

  it.each([
    ['sequence migration', { migration: ['pending'], test_tier: 'static', authoritative_test: 'fe/probe.test.ts:1' }],
    ['sequence test tier', { migration: 'pending', test_tier: ['static'], authoritative_test: 'fe/probe.test.ts:1' }],
  ])('rejects a YAML %s instead of coercing it to a scalar', (_label, entry) => {
    expect(() => validateTierEntries([entry])).toThrow();
  });

  it('parses every authoritative location composition without textual path heuristics', () => {
    const entry = {
      id: 'GATE-TIER-MAP-003', migration: 'migrated', test_tier: 'browser',
      authoritative_test: 'fe/a.test.ts:1,2-3 + fe/b.test.ts:4; fe/c.test.ts:5',
    };
    const violations = checkTestTier([entry], [], '/repo', '/repo/fe');
    expect(new Set(violations.map(({ source }) => source))).toEqual(new Set(['fe/a.test.ts', 'fe/b.test.ts', 'fe/c.test.ts']));
  });
});

describe('tier gate decisions', () => {
  const manifest = [
    'tools/test-tier/fixtures/decisions/browser-probe-misassigned.yaml',
    'tools/test-tier/fixtures/decisions/overlap.yaml',
    'tools/test-tier/fixtures/decisions/playwright-owned.yaml',
    'tools/test-tier/fixtures/decisions/unassigned.yaml',
    'tools/test-tier/fixtures/negative/data.yaml', 'tools/test-tier/fixtures/negative/orphan.spec.ts',
    'tools/test-tier/fixtures/negative/wrong-browser.test.ts', 'tools/test-tier/fixtures/positive/browser.browser.test.ts',
    'tools/test-tier/fixtures/positive/data.yaml', 'tools/test-tier/fixtures/positive/static.test.ts',
  ];
  const projects = [
    { name: 'platform-independent', include: ['**/*.test.{js,ts,mts}'], exclude: ['**/*.browser.test.ts'] },
    { name: 'browser', include: ['**/*.browser.test.ts'], exclude: [] },
  ];
  const valid = {
    oracleEntries: [], trackedFixtures: manifest,
    trackedTests: ['tools/test-tier/layout.browser.test.ts', 'tools/test-tier/fixtures/negative/orphan.spec.ts'], projects,
  };

  it('excludes the declared fixtures from project coverage', () => {
    expect(tierGateViolations(valid)).toEqual([]);
  });

  it.each(fixtureManifest.decisions)('proves the %s decision fixture independently', (name) => {
    const fixture = decisionFixture(name);
    const trackedTests = fixture.trackedTest === 'tools/test-tier/layout.browser.test.ts'
      ? [fixture.trackedTest]
      : ['tools/test-tier/layout.browser.test.ts', fixture.trackedTest];
    const violations = tierGateViolations({ ...valid, trackedTests, projects: fixture.projects });
    expect(violations).toEqual(fixture.expected === null ? [] : [expect.stringContaining(fixture.expected)]);
  });

  it.each([
    ['missing fixture', { trackedFixtures: manifest.slice(1) }, 'tracked fixture set differs'],
    ['extra fixture', { trackedFixtures: [...manifest, 'tools/test-tier/fixtures/positive/extra.ts'] }, 'tracked fixture set differs'],
    ['unassigned test suffix', { trackedTests: [...valid.trackedTests, 'probe.test.mts'], projects: projects.slice(1) }, 'outside every test project'],
    ['overlapping test', { trackedTests: [...valid.trackedTests, 'probe.test.ts'], projects: [...projects, { name: 'other', include: ['probe.test.ts'], exclude: [] }] }, 'multiple test projects'],
    ['missing browser probe', { trackedTests: ['other.browser.test.ts'] }, 'must be tracked and mapped'],
    ['misassigned browser probe', { projects: projects.slice(0, 1) }, 'must be tracked and mapped'],
  ])('reports %s', (_label, change, message) => {
    expect(tierGateViolations({ ...valid, ...change })).toEqual(expect.arrayContaining([expect.stringContaining(message)]));
  });

  it('reports an oracle tier/project mismatch', () => {
    const oracleEntries = [{ id: 'GATE', migration: 'migrated', test_tier: 'browser', authoritative_test: 'fe/probe.test.ts:1' }];
    expect(tierGateViolations({ ...valid, oracleEntries })).toEqual(expect.arrayContaining([expect.stringContaining('test-tier-project')]));
  });
});
