import { existsSync, readdirSync } from 'node:fs';
import { resolve } from 'node:path';
import { createRequire } from 'node:module';
import { cruise as dependencyCruise, type IConfiguration } from 'dependency-cruiser';
import ts from 'typescript';
import { describe, expect, it } from 'vitest';
import { checkCoreNoJsx } from './check-core-no-jsx.mjs';
import { checkEslintHygiene } from './check-eslint-hygiene.mjs';
import { checkTopLevel } from './check-top-level.mjs';

const fixtures = resolve(import.meta.dirname, 'fixtures');
const config = createRequire(import.meta.url)('./fixture-config.cjs') as IConfiguration;
const cruiseOptions = config.options ?? {};

async function cruise(caseName: string, kind: 'positive' | 'negative') {
  if (caseName === 'top-level-only-main') {
    const cwd = resolve(fixtures, caseName, kind);
    const error = checkTopLevel(resolve(cwd, 'web/src'));
    return { status: error ? 1 : 0, stdout: error, stderr: '' };
  }
  if (caseName === 'core-no-jsx') {
    const error = checkCoreNoJsx(resolve(fixtures, caseName, kind, 'core'));
    return { status: error ? 1 : 0, stdout: error, stderr: '' };
  }
  if (caseName === 'core-platform-types') {
    const cwd = resolve(fixtures, caseName, kind);
    const configPath = resolve(cwd, 'tsconfig.json');
    const parsed = ts.parseJsonConfigFileContent(ts.readConfigFile(configPath, (path) => ts.sys.readFile(path)).config, ts.sys, cwd);
    const diagnostics = ts.getPreEmitDiagnostics(ts.createProgram(parsed.fileNames, parsed.options));
    return { status: diagnostics.length ? 1 : 0, stdout: diagnostics.length ? `${caseName}: ${diagnostics.map((item) => item.code).join(',')}` : '', stderr: '' };
  }
  if (caseName.startsWith('eslint-')) {
    const errors = checkEslintHygiene(resolve(fixtures, caseName, kind));
    return { status: errors.length ? 1 : 0, stdout: errors.join('\n'), stderr: '' };
  }
  const cwd = resolve(fixtures, caseName, kind);
  const inputs = ['core', 'web/src'].filter((input) => existsSync(resolve(cwd, input)));
  const originalCwd = process.cwd();
  process.chdir(cwd);
  try {
    const result = await dependencyCruise(inputs, {
      ...cruiseOptions,
      validate: true,
      ruleSet: { forbidden: config.forbidden },
    }, cruiseOptions.enhancedResolveOptions);
    if (typeof result.output === 'string') throw new TypeError('Expected dependency-cruiser JSON output');
    return {
      status: result.output.summary.violations.length === 0 ? 0 : 1,
      stdout: JSON.stringify(result.output.summary.violations),
      stderr: '',
    };
  } finally {
    process.chdir(originalCwd);
  }
}

describe('architecture fixtures', () => {
  for (const caseName of readdirSync(fixtures)) {
    it(`${caseName}: accepts the positive and rejects the negative fixture`, async () => {
      const positive = await cruise(caseName, 'positive');
      const negative = await cruise(caseName, 'negative');
      expect(positive.status, positive.stdout + positive.stderr).toBe(0);
      expect(negative.status, negative.stdout + negative.stderr).not.toBe(0);
      const expectedRule = caseName.startsWith('no-barrel-index') ? 'no-barrel-index' : caseName;
      expect(negative.stdout + negative.stderr).toContain(expectedRule);
    });
  }
});
