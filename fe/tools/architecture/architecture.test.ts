import { existsSync, readdirSync } from 'node:fs';
import { resolve } from 'node:path';
import { createRequire } from 'node:module';
import { cruise as dependencyCruise, type IConfiguration } from 'dependency-cruiser';
import { ESLint } from 'eslint';
import ts from 'typescript';
import { describe, expect, it } from 'vitest';
import { checkCoreNoJsx } from './check-core-no-jsx.mjs';
import { checkEslintHygiene } from './check-eslint-hygiene.mjs';
import { checkTopLevel } from './check-top-level.mjs';

const fixtures = resolve(import.meta.dirname, 'fixtures');
const config = createRequire(import.meta.url)('./fixture-config.cjs') as IConfiguration;
const cruiseOptions = config.options ?? {};

async function cruise(caseName: string, kind: 'positive' | 'negative') {
  if (caseName === 'source-layout' || caseName === 'top-level-only-main') {
    const cwd = resolve(fixtures, caseName, kind);
    const error = checkTopLevel(cwd);
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
  if (caseName === 'core-no-platform-globals' || caseName === 'core-no-node-access') {
    const filePath = resolve(fixtures, caseName, kind, 'core/case.ts');
    const eslint = new ESLint({ cwd: resolve(import.meta.dirname, '../..'), ignore: false });
    const [result] = await eslint.lintText(ts.sys.readFile(filePath) ?? '', {
      filePath: resolve(import.meta.dirname, '../../core/platform-independent.ts'),
    });
    const output = result.messages.map((message) => `${message.ruleId}: ${message.message}`).join('\n');
    return { status: result.errorCount ? 1 : 0, stdout: output ? `${caseName}: ${output}` : '', stderr: '' };
  }
  if (caseName.startsWith('eslint-')) {
    const errors = await checkEslintHygiene(resolve(fixtures, caseName, kind));
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

  it('pins the Node engine floor required by Vite', () => {
    const packageJson = JSON.parse(ts.sys.readFile(resolve(import.meta.dirname, '../../package.json')) ?? '{}') as { engines?: { node?: string } };
    expect(packageJson.engines?.node).toBe('^20.19.0 || >=22.12.0');
  });

  it('keeps the jsx-a11y preset free of redundant restatements', () => {
    const source = ts.sys.readFile(resolve(import.meta.dirname, '../../eslint.config.js')) ?? '';
    expect(source).not.toContain('plugins: jsxA11y.flatConfigs.recommended.plugins');
    expect(source).not.toContain('rules: jsxA11y.flatConfigs.recommended.rules');
  });
});
