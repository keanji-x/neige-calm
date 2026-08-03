import { spawnSync } from 'node:child_process';
import { existsSync, readFileSync } from 'node:fs';
import { resolve } from 'node:path';
import { parse } from 'yaml';
import vitestConfig from '../../vitest.config.ts';
import playwrightConfig from '../../playwright.config.ts';
import { checkTestTier, validateTierEntries } from './checker.ts';
import { playwrightProjectFromConfig, testAssignments, testProjectsFromConfig } from './project-map.ts';

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
validateTierEntries(entries);

const fixtureManifest = Object.freeze({
  positive: Object.freeze(['browser.browser.test.ts', 'data.yaml', 'static.test.ts']),
  negative: Object.freeze(['data.yaml', 'orphan.spec.ts', 'wrong-browser.test.ts']),
});
for (const [kind, expected] of Object.entries(fixtureManifest)) {
  const prefix = `fe/tools/test-tier/fixtures/${kind}/`;
  const actual = trackedFiles([`${prefix}**`]).map((path) => path.slice(prefix.length)).sort();
  if (actual.join('\n') !== [...expected].sort().join('\n')) {
    throw new Error(`${kind} tracked fixture set differs from its manifest: ${actual.join(', ')}`);
  }
}

const vitestProjects = testProjectsFromConfig(vitestConfig);
const projects = [...vitestProjects, playwrightProjectFromConfig(playwrightConfig)];
const trackedTests = trackedFiles(['fe'])
  .filter((path) => /\.test\.tsx?$/.test(path)).map((path) => path.slice('fe/'.length));
const assignments = testAssignments(trackedTests, vitestProjects);
const unassigned = assignments.filter(({ projects: owners }) => owners.length === 0);
const overlapping = assignments.filter(({ projects: owners }) => owners.length > 1);
const browserTests = assignments.filter(({ path, projects: owners }) =>
  !path.startsWith('tools/test-tier/fixtures/')
  && owners.includes('browser') && existsSync(resolve(configRoot, path)));
if (unassigned.length) throw new Error(`tracked tests outside every vitest project: ${unassigned.map(({ path }) => path).join(', ')}`);
if (overlapping.length) throw new Error(`tracked tests in multiple vitest projects: ${overlapping.map(({ path }) => path).join(', ')}`);
if (browserTests.length === 0) throw new Error('browser vitest project has no tracked non-fixture test files');

const violations = checkTestTier(entries, projects, repoRoot, configRoot);
for (const violation of violations) console.error(`${violation.rule}: ${violation.id}: ${violation.message}`);
if (violations.length) process.exitCode = 1;
