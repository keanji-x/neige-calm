import { relative, resolve, sep } from 'node:path';
import type { TestProject } from './project-map.ts';
import { projectsForPath } from './project-map.ts';

export interface TierEntry { id?: unknown; migration?: unknown; test_tier?: unknown; authoritative_test?: unknown }
export interface TierViolation { id: string; rule: 'test-tier-project'; source: string; message: string }

const LOCATION = /^([^\s:]+):(\d+)(?:-(\d+))?$/;
const EXPECTED_PROJECT = Object.freeze({ browser: 'browser', jsdom: 'ui-dom', static: 'platform-independent' } as const);

function referencedPaths(value: string): string[] {
  return value.trim().split(/\s*;\s*|\s+\+\s+|\s+/).flatMap((group) => {
    const [first, ...ranges] = group.split(',');
    const match = LOCATION.exec(first);
    return match ? [match[1], ...ranges.map(() => match[1])] : [];
  });
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
    const expected = EXPECTED_PROJECT[entry.test_tier as keyof typeof EXPECTED_PROJECT];
    if (!expected || entry.authoritative_test === 'NONE') continue;
    for (const source of new Set(referencedPaths(entry.authoritative_test))) {
      const testPath = configRelativePath(repoRoot, configRoot, source);
      const actual = testPath === null ? [] : projectsForPath(testPath, projects);
      if (actual.length !== 1 || actual[0] !== expected) violations.push({
        id: entry.id, rule: 'test-tier-project', source,
        message: `test_tier ${String(entry.test_tier)} requires ${expected}; ${source} belongs to ${actual.length ? actual.join(', ') : 'no vitest project'}`,
      });
    }
  }
  return violations;
}
