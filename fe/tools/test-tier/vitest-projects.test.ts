import { describe, expect, it } from 'vitest';
import vitestConfig from '../../vitest.config';
import packageJson from '../../package.json';
import { playwrightProjectFromConfig, projectNamesFromScript, projectsForPath, testProjectsFromConfig } from './project-map';

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

  it('runs every configured project in either test script', () => {
    const scripted = new Set([
      ...projectNamesFromScript(packageJson.scripts.test), ...projectNamesFromScript(packageJson.scripts['test:browser']),
    ]);
    expect(scripted).toEqual(new Set(projects.map(({ name }) => name)));
  });

  it('rejects a manifest without the real browser probe', async () => {
    const { tierGateViolations } = await import('./checker');
    expect(tierGateViolations({ oracleEntries: [], trackedFixtures: [], trackedTests: [], projects }))
      .toContain('tools/test-tier/layout.browser.test.ts must be tracked and mapped to the browser project');
  });

  it('uses the configured Playwright testDir and rejects unsupported collection options', () => {
    expect(playwrightProjectFromConfig({ testDir: './integration/' }, '/repo/fe').include[0])
      .toBe('integration/**/*.spec.{js,jsx,ts,tsx,mjs,mts,cjs,cts}');
    expect(() => playwrightProjectFromConfig({}, '/repo/fe')).toThrow(/testDir/);
    expect(() => playwrightProjectFromConfig({ testDir: './e2e', testMatch: '**/*.spec.ts' }, '/repo/fe')).toThrow(/testMatch/);
    expect(() => playwrightProjectFromConfig({ testDir: '../e2e' }, '/repo/fe')).toThrow(/inside/);
  });
});
