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
import { checkDuplicationManifest } from './check-duplication-manifest.mjs';

const fixtures = resolve(import.meta.dirname, 'fixtures');
const config = createRequire(import.meta.url)('./fixture-config.cjs') as IConfiguration;
const cruiseOptions = config.options ?? {};

async function cruise(caseName: string, kind: 'positive' | 'negative') {
  if (caseName.startsWith('dup-inv-')) {
    const errors = checkDuplicationManifest(resolve(fixtures, caseName, kind));
    return { status: errors.length ? 1 : 0, stdout: errors.join('\n'), stderr: '' };
  }
  if (caseName === 'test-module-runtime-state-exemption') {
    const fixtureDir = resolve(fixtures, caseName, kind, 'web/src');
    const eslint = new ESLint({ cwd: resolve(import.meta.dirname, '../..'), ignore: false });
    const fixtureTargets = kind === 'positive'
      ? [['mock-test.ts.fixture', 'ui/state/public.contract.test.ts']]
      : [['mock.ts.fixture', 'ui/state/public.ts'], ['context-test.ts.fixture', 'ui/state/public.test.ts']];
    const results = await Promise.all(fixtureTargets.map(async ([fixtureName, targetName]) => {
      const source = ts.sys.readFile(resolve(fixtureDir, fixtureName)) ?? '';
      return eslint.lintText(source, { filePath: resolve(import.meta.dirname, '../../web/src', targetName) });
    }));
    const messages = results.flatMap(([result]) => result.messages)
      .filter((message) => message.ruleId?.startsWith('architecture/'));
    const ruleIds = messages.map((message) => message.ruleId);
    if (kind === 'positive') {
      return { status: messages.length ? 1 : 0, stdout: ruleIds.join('\n'), stderr: '' };
    }
    const expectedRules = ['architecture/no-module-runtime-state', 'architecture/no-create-context-outside-allowlist'];
    const missingRules = expectedRules.filter((ruleId) => !ruleIds.includes(ruleId));
    return { status: missingRules.length ? 0 : 1, stdout: missingRules.length ? missingRules.join('\n') : caseName, stderr: '' };
  }
  if (caseName.startsWith('source-layout') || caseName === 'top-level-only-main') {
    const cwd = resolve(fixtures, caseName, kind);
    const error = checkTopLevel(cwd);
    return { status: error ? 1 : 0, stdout: error, stderr: '' };
  }
  if (caseName === 'core-no-jsx' || caseName === 'cards-registry-no-jsx') {
    const error = checkCoreNoJsx(
      resolve(fixtures, caseName, kind, 'core'),
      resolve(fixtures, caseName, kind, 'web/src/systems/cards'),
    );
    return { status: error ? 1 : 0, stdout: error, stderr: '' };
  }
  if (caseName === 'core-markdown-node-import') {
    const filePath = resolve(fixtures, caseName, kind, 'core/markdown/case.js');
    const eslint = new ESLint({
      cwd: resolve(import.meta.dirname, '../..'),
      ignore: false,
    });
    const [result] = await eslint.lintText(ts.sys.readFile(filePath) ?? '', {
      filePath: resolve(import.meta.dirname, '../../core/markdown/case.js'),
    });
    const messages = result.messages.filter((message) => message.ruleId === 'no-restricted-imports');
    const output = messages.map((message) => `${message.ruleId}: ${message.message}`).join('\n');
    return { status: messages.length ? 1 : 0, stdout: output, stderr: '' };
  }
  if (caseName === 'markdown-micromark-import') {
    const filePath = resolve(fixtures, caseName, kind, 'web/src/case.ts');
    const eslint = new ESLint({ cwd: resolve(import.meta.dirname, '../..'), ignore: false });
    const [result] = await eslint.lintText(ts.sys.readFile(filePath) ?? '', {
      filePath: resolve(import.meta.dirname, '../../web/src/main.tsx'),
    });
    const messages = result.messages.filter((message) => message.ruleId === 'no-restricted-imports');
    return { status: messages.length ? 1 : 0, stdout: messages.map((message) => `${message.ruleId}: ${message.message}`).join('\n'), stderr: '' };
  }
  if (caseName === 'react-state-hook-import') {
    const filePath = resolve(fixtures, caseName, kind, 'web/src/ui/case.ts');
    const eslint = new ESLint({ cwd: resolve(import.meta.dirname, '../..'), ignore: false });
    const [result] = await eslint.lintText(ts.sys.readFile(filePath) ?? '', {
      filePath: resolve(import.meta.dirname, '../../web/src/ui/state/public.contract.test.ts'),
    });
    const messages = result.messages.filter((message) => message.ruleId === 'no-restricted-imports');
    const output = messages.map((message) => `${message.ruleId}: ${message.message}`).join('\n');
    return { status: messages.length ? 1 : 0, stdout: output, stderr: '' };
  }
  if (caseName.startsWith('core-platform-')) {
    const cwd = resolve(fixtures, caseName, kind);
    const configPath = resolve(cwd, 'tsconfig.json');
    const parsed = ts.parseJsonConfigFileContent(ts.readConfigFile(configPath, (path) => ts.sys.readFile(path)).config, ts.sys, cwd);
    const diagnostics = ts.getPreEmitDiagnostics(ts.createProgram(parsed.fileNames, parsed.options));
    return { status: diagnostics.length ? 1 : 0, stdout: diagnostics.length ? `${caseName}: ${diagnostics.map((item) => item.code).join(',')}` : '', stderr: '' };
  }
  if (caseName === 'core-no-platform-globals' || caseName.startsWith('core-no-platform-global-') || caseName.startsWith('core-no-node-')) {
    const filePath = resolve(fixtures, caseName, kind, 'core/case.ts');
    const eslint = new ESLint({
      cwd: resolve(import.meta.dirname, '../..'),
      ignore: false,
    });
    const [result] = await eslint.lintText(ts.sys.readFile(filePath) ?? '', {
      filePath: resolve(import.meta.dirname, '../../core/platform-independent.ts'),
    });
    const target = caseName === 'core-no-platform-globals' || caseName.startsWith('core-no-platform-global-') || caseName.startsWith('core-no-node-global-')
      ? 'no-restricted-globals' : 'no-restricted-imports';
    const messages = result.messages.filter((message) => message.ruleId === target);
    const output = messages.map((message) => `${message.ruleId}: ${message.message}`).join('\n');
    return { status: messages.length ? 1 : 0, stdout: output, stderr: '' };
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
  const expectedViolation = new Map<string, string>([
    ['dup-inv-001', 'INV-DUP-001'],
    ['dup-inv-002', 'INV-DUP-002'],
    ['dup-inv-003', 'INV-DUP-003'],
    ['dup-inv-004', 'INV-DUP-004'],
    ['dup-inv-005', 'INV-DUP-005'],
    ['dup-inv-006', 'INV-DUP-006'],
    ['dup-inv-007', 'INV-DUP-007'],
    ['dup-inv-008', 'INV-DUP-008'],
    ['dup-inv-009', 'INV-DUP-009'],
    ['dup-inv-010', 'INV-DUP-010'],
    ['core-markdown-node-import', 'node:fs'],
    ['markdown-micromark-import', 'micromark-extension-gfm'],
    ['core-no-node-access', 'node:fs'],
    ['core-no-node-bare-import', "'fs'"],
    ['core-no-node-global-buffer', 'Buffer'],
    ['core-no-node-global-process', 'process'],
    ['core-no-node-global-require', 'require'],
    ['core-no-platform-globals', 'WebSocket'],
    ['core-no-platform-global-fetch', 'fetch'],
    ['core-no-platform-global-location', 'location'],
    ['core-no-web-styles', 'web/src/styles'],
    ['core-platform-node-types', '2591'],
    ['core-platform-types', '2584'],
    ['core-no-jsx', 'bad.tsx'],
    ['cards-registry-no-jsx', 'registry.tsx'],
    ['react-state-hook-import', 'web/src/ui/state/public.ts'],
    ['eslint-config-root-only', 'nested/eslint.config.js'],
    ['eslint-no-off-shims', 'example/rule'],
    ['source-layout', 'core/helpers.js'],
    ['source-layout-dir', 'web/src/features/inbox/shared'],
    ['top-level-only-main', 'web/src/loose.js'],
  ]);

  for (const caseName of readdirSync(fixtures)) {
    it(`${caseName}: accepts the positive and rejects the negative fixture`, async () => {
      const positive = await cruise(caseName, 'positive');
      const negative = await cruise(caseName, 'negative');
      expect(positive.status, positive.stdout + positive.stderr).toBe(0);
      expect(negative.status, negative.stdout + negative.stderr).not.toBe(0);
      const expected = expectedViolation.get(caseName) ?? (caseName.startsWith('no-barrel-index') ? 'no-barrel-index' : caseName);
      expect(negative.stdout + negative.stderr).toContain(expected);
    });
  }
});

describe('duplication manifest on the application tree', () => {
  it('keeps INV-DUP-001..010 implementations and consumers canonical', () => {
    expect(checkDuplicationManifest(resolve(import.meta.dirname, '../..'))).toEqual([]);
  });
});
