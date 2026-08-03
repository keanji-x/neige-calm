import { matchesGlob } from 'node:path';

export interface TestProject {
  name: string;
  include: readonly string[];
  exclude: readonly string[];
}

export interface TestAssignment { path: string; projects: string[] }

interface VitestProjectShape {
  test?: {
    name?: unknown;
    include?: unknown;
    exclude?: unknown;
  };
}

interface VitestConfigShape {
  test?: { projects?: unknown };
}

function stringArray(value: unknown): string[] {
  return Array.isArray(value) && value.every((item) => typeof item === 'string') ? value : [];
}

export function testProjectsFromConfig(config: unknown): TestProject[] {
  const projects = (config as VitestConfigShape | undefined)?.test?.projects;
  if (!Array.isArray(projects)) throw new Error('vitest config must export test.projects as an array');
  return projects.map((raw, index) => {
    const project = raw as VitestProjectShape;
    if (typeof project.test?.name !== 'string') throw new Error(`vitest project ${index} must have a string name`);
    const include = stringArray(project.test.include);
    if (include.length === 0) throw new Error(`vitest project ${project.test.name} must have include globs`);
    return Object.freeze({
      name: project.test.name,
      include: Object.freeze(include),
      exclude: Object.freeze(stringArray(project.test.exclude)),
    });
  });
}

export function projectsForPath(path: string, projects: readonly TestProject[]): string[] {
  // node:path matchesGlob only approximates Vitest/tinyglobby (notably defaults and dot directories);
  // callers therefore feed it tracked repository paths and model default exclusions explicitly.
  return projects
    .filter((project) => project.include.some((glob) => matchesGlob(path, glob))
      && !project.exclude.some((glob) => matchesGlob(path, glob)))
    .map((project) => project.name);
}

export function testAssignments(paths: readonly string[], projects: readonly TestProject[]): TestAssignment[] {
  return paths.map((path) => ({ path, projects: projectsForPath(path, projects) }));
}

export function playwrightProjectFromConfig(config: unknown): TestProject {
  const testDir = (config as { testDir?: unknown } | undefined)?.testDir;
  const directory = (typeof testDir === 'string' ? testDir : './e2e').replace(/^\.\//, '').replace(/\/$/, '');
  return Object.freeze({
    name: 'playwright',
    include: Object.freeze([
      `${directory}/**/*.spec.{js,jsx,ts,tsx,mjs,mts,cjs,cts}`,
      `${directory}/**/*.test.{js,jsx,ts,tsx,mjs,mts,cjs,cts}`,
    ]),
    exclude: Object.freeze([]),
  });
}
