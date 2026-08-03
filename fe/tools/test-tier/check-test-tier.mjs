import { execFileSync } from 'node:child_process';
import { readFileSync } from 'node:fs';
import { resolve } from 'node:path';
import { parse } from 'yaml';
import vitestConfig from '../../vitest.config.ts';
import { checkTestTier } from './checker.ts';
import { testProjectsFromConfig } from './project-map.ts';

const configRoot = resolve(import.meta.dirname, '../..');
const repoRoot = resolve(configRoot, '..');
const oracleFiles = execFileSync('git', ['ls-files', '--', 'docs/oracle/*.yaml'], { cwd: repoRoot, encoding: 'utf8' })
  .trim().split('\n').filter((path) => !/(?:anchor-none|anchor-unsupported|owner-aliases)\.yaml$/.test(path));
const entries = oracleFiles.flatMap((path) => {
  const value = parse(readFileSync(resolve(repoRoot, path), 'utf8'));
  return Array.isArray(value) ? value : [];
});
const violations = checkTestTier(entries, testProjectsFromConfig(vitestConfig), repoRoot, configRoot);
for (const violation of violations) console.error(`${violation.rule}: ${violation.id}: ${violation.message}`);
if (violations.length) process.exitCode = 1;
