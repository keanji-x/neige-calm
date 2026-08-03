import { relative, resolve, sep } from 'node:path';
import type { TestProject } from './project-map.ts';
import { projectsForPath } from './project-map.ts';

export interface TierEntry { id?: unknown; migration?: unknown; test_tier?: unknown; authoritative_test?: unknown }
export interface TierViolation { id: string; rule: 'test-tier-project'; source: string; message: string }
export interface TierGateInput {
  oracleEntries: readonly TierEntry[];
  trackedFixtures: readonly string[];
  trackedTests: readonly string[];
  projects: readonly TestProject[];
}

const LOCATION = /^([^\s:]+):(\d+)(?:-(\d+))?$/;
const EXPECTED_PROJECTS = Object.freeze({
  browser: Object.freeze(['browser', 'playwright']),
  jsdom: Object.freeze(['ui-dom']),
  static: Object.freeze(['platform-independent']),
} as const);
const MIGRATIONS = new Set(['pending', 'skipped', 'migrated']);
const TEST_TIERS = new Set(['browser', 'jsdom', 'static', 'none']);

export function referencedPaths(value: string): string[] {
  if (value === 'NONE') return [];
  const groups = value.trim().split(/\s*;\s*|\s+\+\s+|\s+/).filter(Boolean);
  if (groups.length === 0) throw new Error('authoritative_test must be NONE or one or more source locations');
  return groups.flatMap((group) => {
    const [first, ...ranges] = group.split(',');
    const match = LOCATION.exec(first);
    if (!match || ranges.some((range) => !/^\d+(?:-\d+)?$/.test(range))) {
      throw new Error(`authoritative_test location is invalid: ${group}`);
    }
    return [match[1], ...ranges.map(() => match[1])];
  });
}

export function validateTierEntries(entries: readonly TierEntry[]): void {
  for (const [index, entry] of entries.entries()) {
    if (typeof entry.migration !== 'string' || !MIGRATIONS.has(entry.migration)) throw new Error(`entry ${index} has invalid migration: ${String(entry.migration)}`);
    if (typeof entry.test_tier !== 'string' || !TEST_TIERS.has(entry.test_tier)) throw new Error(`entry ${index} has invalid test_tier: ${String(entry.test_tier)}`);
    if (typeof entry.authoritative_test !== 'string') throw new Error(`entry ${index} authoritative_test must be a string`);
    referencedPaths(entry.authoritative_test);
  }
}

const FIXTURE_MANIFEST = Object.freeze([
  'tools/test-tier/fixtures/negative/data.yaml',
  'tools/test-tier/fixtures/negative/orphan.spec.ts',
  'tools/test-tier/fixtures/negative/wrong-browser.test.ts',
  'tools/test-tier/fixtures/positive/browser.browser.test.ts',
  'tools/test-tier/fixtures/positive/data.yaml',
  'tools/test-tier/fixtures/positive/static.test.ts',
]);
const BROWSER_PROBE = 'tools/test-tier/layout.browser.test.ts';

export function tierGateViolations(input: TierGateInput): string[] {
  validateTierEntries(input.oracleEntries);
  const violations: string[] = [];
  const expectedFixtures = new Set(FIXTURE_MANIFEST);
  const actualFixtures = new Set(input.trackedFixtures);
  const missingFixtures = FIXTURE_MANIFEST.filter((path) => !actualFixtures.has(path));
  const extraFixtures = input.trackedFixtures.filter((path) => !expectedFixtures.has(path));
  if (missingFixtures.length || extraFixtures.length) {
    violations.push(`tracked fixture set differs from its manifest: missing ${missingFixtures.join(', ') || 'none'}; extra ${extraFixtures.join(', ') || 'none'}`);
  }
  const vitestProjects = input.projects.filter(({ name }) => name !== 'playwright');
  const assignments = input.trackedTests
    .filter((path) => !expectedFixtures.has(path))
    .map((path) => ({ path, projects: projectsForPath(path, vitestProjects) }));
  const unassigned = assignments.filter(({ projects }) => projects.length === 0);
  if (unassigned.length) violations.push(`tracked tests outside every vitest project: ${unassigned.map(({ path }) => path).join(', ')}`);
  const overlapping = assignments.filter(({ projects }) => projects.length > 1);
  if (overlapping.length) violations.push(`tracked tests in multiple vitest projects: ${overlapping.map(({ path }) => path).join(', ')}`);
  const browserProbe = assignments.find(({ path }) => path === BROWSER_PROBE);
  if (!browserProbe || !browserProbe.projects.includes('browser')) {
    violations.push(`${BROWSER_PROBE} must be tracked and mapped to the browser project`);
  }
  return [...violations, ...checkTestTier(input.oracleEntries, input.projects, '/repo', '/repo/fe')
    .map(({ id, message }) => `test-tier-project: ${id}: ${message}`)];
}

function configRelativePath(repoRoot: string, configRoot: string, path: string): string | null {
  const fromConfig = relative(configRoot, resolve(repoRoot, path));
  if (fromConfig === '..' || fromConfig.startsWith(`..${sep}`)) return null;
  return fromConfig.replaceAll('\\', '/');
}

export function checkTestTier(entries: readonly TierEntry[], projects: readonly TestProject[],
  repoRoot: string, configRoot: string): TierViolation[] {
  const violations: TierViolation[] = [];
  for (const entry of entries) {
    if (entry.migration !== 'migrated' || typeof entry.id !== 'string' || typeof entry.authoritative_test !== 'string') continue;
    const expected = EXPECTED_PROJECTS[entry.test_tier as keyof typeof EXPECTED_PROJECTS];
    if (!expected || entry.authoritative_test === 'NONE') continue;
    for (const source of new Set(referencedPaths(entry.authoritative_test))) {
      const testPath = configRelativePath(repoRoot, configRoot, source);
      const actual = testPath === null ? [] : projectsForPath(testPath, projects);
      if (actual.length !== 1 || !expected.some((project) => project === actual[0])) violations.push({
        id: entry.id, rule: 'test-tier-project', source,
        message: `test_tier ${String(entry.test_tier)} requires ${expected.join(' or ')}; ${source} belongs to ${actual.length ? actual.join(', ') : 'no test project'}`,
      });
    }
  }
  return violations;
}
