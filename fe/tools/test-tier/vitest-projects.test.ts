import { describe, expect, it } from 'vitest';
import vitestConfig from '../../vitest.config';
import packageJson from '../../package.json';
import { playwrightProjectFromConfig, projectNamesFromScript, projectsForPath, testProjectsFromConfig } from './project-map';

describe('vitest project partition', () => {
  const projects = testProjectsFromConfig(vitestConfig);

  it('exports exactly the four named projects', () => {
    expect(projects.map(({ name }) => name))
      .toEqual(['platform-independent', 'web-dom', 'browser', 'browser-coarse']);
  });

  it.each([
    ['tools/probe.test.ts', ['platform-independent']],
    ['web/src/app/probe.test.ts', ['web-dom']],
    ['web/src/features/probe.test.tsx', ['web-dom']],
    ['web/src/systems/probe.test.ts', ['web-dom']],
    ['web/src/ui/probe.test.tsx', ['web-dom']],
    ['tools/probe.browser.test.ts', ['browser']],
    ['web/src/ui/probe.browser.test.tsx', ['browser']],
    /*
     * The coarse suffix is a `.browser.test.` too, so `browser`'s own include
     * collects it and only that project's exclude keeps this a partition —
     * which is exactly the kind of overlap `projectsForPath` is here to decide
     * instead of anyone reading two globs side by side.
     */
    ['tools/probe.coarse.browser.test.ts', ['browser-coarse']],
    ['web/src/features/chat/thread/thread.coarse.browser.test.tsx', ['browser-coarse']],
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
