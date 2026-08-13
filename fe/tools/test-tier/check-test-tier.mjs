import { spawnSync } from 'node:child_process';
import { readFileSync } from 'node:fs';
import { resolve } from 'node:path';
import { parse } from 'yaml';
import vitestConfig from '../../vitest.config.ts';
import playwrightConfig from '../../playwright.config.ts';
import { tierGateViolations } from './checker.ts';
import { playwrightProjectFromConfig, testProjectsFromConfig } from './project-map.ts';

const configRoot = resolve(import.meta.dirname, '../..');
const repoRoot = resolve(configRoot, '..');

/** @param {string[]} patterns */
function trackedFiles(patterns) {
  const result = spawnSync('git', ['ls-files', '--', ...patterns], { cwd: repoRoot, encoding: 'utf8' });
  // Some policy sandboxes attach EPERM to a successful read-only git process; status and stdout
  // remain authoritative. A real git failure (including a signal/null status) still closes the gate.
  if (result.status !== 0) throw new Error(`git ls-files failed: ${result.stderr || result.error?.message || 'unknown error'}`);
  return result.stdout.trim().split('\n').filter(Boolean);
}

const oracleFiles = trackedFiles(['docs/oracle/*.yaml'])
  .filter((path) => !/(?:anchor-none|anchor-unsupported|owner-aliases)\.yaml$/.test(path));
const entries = oracleFiles.flatMap((path) => {
  const value = parse(readFileSync(resolve(repoRoot, path), 'utf8'));
  return Array.isArray(value) ? value : [];
});
const vitestProjects = testProjectsFromConfig(vitestConfig);
const projects = [...vitestProjects, playwrightProjectFromConfig(playwrightConfig, configRoot)];
const trackedFixtures = trackedFiles(['fe/tools/test-tier/fixtures/**'])
  .map((path) => path.slice('fe/'.length));
const trackedTests = trackedFiles(['fe'])
  .filter((path) => /\.(?:test|spec)\.(?:js|jsx|ts|tsx|mjs|mts|cjs|cts)$/.test(path))
  .filter((path) => !path.startsWith('fe/tools/architecture/fixtures/'))
  .map((path) => path.slice('fe/'.length));
const violations = tierGateViolations({ oracleEntries: entries, trackedFixtures, trackedTests, projects });
for (const violation of violations) console.error(violation);
if (violations.length) process.exitCode = 1;
