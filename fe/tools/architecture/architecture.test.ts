import { existsSync, readdirSync } from 'node:fs';
import { resolve } from 'node:path';
import { spawnSync } from 'node:child_process';
import { createRequire } from 'node:module';
import { cruise as dependencyCruise } from 'dependency-cruiser';
import { describe, expect, it } from 'vitest';

const fixtures = resolve(import.meta.dirname, 'fixtures');
const config = createRequire(import.meta.url)('./fixture-config.cjs');

async function cruise(caseName: string, kind: 'positive' | 'negative') {
  if (caseName === 'top-level-only-main') {
    return spawnSync(process.execPath, [resolve(import.meta.dirname, 'check-top-level.mjs'), 'web/src'], {
      cwd: resolve(fixtures, caseName, kind), encoding: 'utf8',
    });
  }
  if (caseName === 'core-no-jsx') {
    return spawnSync(process.execPath, [resolve(import.meta.dirname, 'check-core-platform.mjs'), 'core'], {
      cwd: resolve(fixtures, caseName, kind), encoding: 'utf8',
    });
  }
  const cwd = resolve(fixtures, caseName, kind);
  const inputs = ['core', 'web/src'].filter((input) => existsSync(resolve(cwd, input)));
  const originalCwd = process.cwd();
  process.chdir(cwd);
  try {
    const result = await dependencyCruise(inputs, {
      ...config.options,
      validate: true,
      ruleSet: { forbidden: config.forbidden },
    }, config.options.enhancedResolveOptions);
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
      if (!['core-no-jsx', 'top-level-only-main'].includes(caseName)) {
        expect(negative.stdout + negative.stderr).toContain(caseName);
      }
    });
  }
});
