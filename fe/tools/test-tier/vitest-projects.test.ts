import { execFileSync } from 'node:child_process';
import { relative, resolve } from 'node:path';
import { describe, expect, it } from 'vitest';
import vitestConfig from '../../vitest.config';
import { projectsForPath, testProjectsFromConfig } from './project-map';

function trackedTests(): string[] {
  const root = resolve(import.meta.dirname, '../..');
  return execFileSync('git', ['ls-files', '--', '*.test.ts', '*.test.tsx'], { cwd: root, encoding: 'utf8' })
    .trim().split('\n').filter(Boolean)
    .map((path) => relative(root, resolve(root, path)).replaceAll('\\', '/'));
}

describe('vitest project partition', () => {
  const projects = testProjectsFromConfig(vitestConfig);

  it('exports exactly the three named projects', () => {
    expect(projects.map(({ name }) => name)).toEqual(['platform-independent', 'ui-dom', 'browser']);
  });

  it.each([
    ['tools/probe.test.ts', ['platform-independent']],
    ['web/src/ui/probe.test.tsx', ['ui-dom']],
    ['tools/probe.browser.test.ts', ['browser']],
    ['web/src/ui/probe.browser.test.tsx', ['browser']],
  ])('assigns representative path %s exactly once', (path, expected) => {
    expect(projectsForPath(path, projects)).toEqual(expected);
  });

  it('assigns every tracked test to exactly one project', () => {
    const assignments = trackedTests().map((path) => ({ path, projects: projectsForPath(path, projects) }));
    expect(assignments.filter(({ projects: owners }) => owners.length === 0)).toEqual([]);
    expect(assignments.filter(({ projects: owners }) => owners.length > 1)).toEqual([]);
  });
});
