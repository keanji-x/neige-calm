import { describe, expect, it } from 'vitest';
import vitestConfig from '../../vitest.config';
import { projectsForPath, testAssignments, testProjectsFromConfig } from './project-map';

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

  it('audits a supplied test manifest and requires a non-empty browser project', () => {
    const assignments = testAssignments(
      ['tools/probe.test.ts', 'web/src/ui/probe.test.tsx', 'tools/probe.browser.test.ts'], projects,
    );
    expect(assignments.filter(({ projects: owners }) => owners.length === 0)).toEqual([]);
    expect(assignments.filter(({ projects: owners }) => owners.length > 1)).toEqual([]);
    expect(assignments.filter(({ projects: owners }) => owners.includes('browser')).length).toBeGreaterThan(0);
  });
});
