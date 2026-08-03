import { execFileSync } from 'node:child_process';
import { readFileSync } from 'node:fs';
import { relative, resolve } from 'node:path';
import { parse } from 'yaml';
import { describe, expect, it } from 'vitest';
import vitestConfig from '../../vitest.config';
import { checkTestTier, type TierEntry } from './checker';
import { testProjectsFromConfig } from './project-map';

const configRoot = resolve(import.meta.dirname, '../..');
const repoRoot = resolve(configRoot, '..');
const fixtureRoot = resolve(import.meta.dirname, 'fixtures');
const fixtureManifest = Object.freeze({
  positive: Object.freeze(['browser.browser.test.ts', 'data.yaml', 'static.test.ts']),
  negative: Object.freeze(['data.yaml', 'orphan.spec.ts', 'wrong-browser.test.ts']),
});

function trackedFixtureFiles(kind: keyof typeof fixtureManifest): string[] {
  const directory = relative(repoRoot, resolve(fixtureRoot, kind)).replaceAll('\\', '/');
  return execFileSync('git', ['ls-files', '--', `${directory}/**`], { cwd: repoRoot, encoding: 'utf8' })
    .trim().split('\n').filter(Boolean).map((path) => path.slice(directory.length + 1)).sort();
}

function fixtureEntries(kind: keyof typeof fixtureManifest): TierEntry[] {
  const value: unknown = parse(readFileSync(resolve(fixtureRoot, kind, 'data.yaml'), 'utf8'));
  if (!Array.isArray(value)) throw new Error(`${kind} fixture must be a YAML sequence`);
  return value.map((entry: unknown) => entry as TierEntry);
}

describe('test-tier-project fixtures', () => {
  const projects = testProjectsFromConfig(vitestConfig);
  it.each(Object.keys(fixtureManifest) as Array<keyof typeof fixtureManifest>)(
    '%s tracked fixture set equals its independent manifest in both directions', (kind) => {
      expect(new Set(trackedFixtureFiles(kind))).toEqual(new Set(fixtureManifest[kind]));
      expect(new Set(fixtureManifest[kind])).toEqual(new Set(trackedFixtureFiles(kind)));
    },
  );
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
    ['browser', 'browser', 'probe.browser.test.ts'],
    ['jsdom', 'ui-dom', 'probe.dom.test.ts'],
    ['static', 'platform-independent', 'probe.test.ts'],
  ])('maps %s only to %s', (tier, projectName, path) => {
    const isolatedProjects = [{ name: projectName, include: [path], exclude: [] }];
    const entry = { id: 'GATE-TIER-MAP-001', migration: 'migrated', test_tier: tier, authoritative_test: `fe/${path}:1` };
    expect(checkTestTier([entry], isolatedProjects, '/repo', '/repo/fe')).toEqual([]);
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

  it('enforces tier mismatch only after migration', () => {
    const nodeOnly = [{ name: 'platform-independent', include: ['probe.test.ts'], exclude: [] }];
    const mismatch = { test_tier: 'browser', authoritative_test: 'fe/probe.test.ts:1' };
    const migrated = { id: 'GATE-TIER-MIGRATED', migration: 'migrated', ...mismatch };
    const pending = { id: 'GATE-TIER-PENDING', migration: 'pending', ...mismatch };

    expect(checkTestTier([migrated], nodeOnly, '/repo', '/repo/fe')).toHaveLength(1);
    expect(checkTestTier([pending], nodeOnly, '/repo', '/repo/fe')).toEqual([]);
  });

  it('rejects overlap for jsdom as well as browser', () => {
    const overlapping = [
      { name: 'ui-dom', include: ['probe.test.tsx'], exclude: [] },
      { name: 'platform-independent', include: ['*.test.tsx'], exclude: [] },
    ];
    const entry = { id: 'GATE-TIER-MAP-005', migration: 'migrated', test_tier: 'jsdom', authoritative_test: 'fe/probe.test.tsx:1' };
    expect(checkTestTier([entry], overlapping, '/repo', '/repo/fe')).toHaveLength(1);
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
