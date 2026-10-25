import { mkdtempSync, mkdirSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';
import { spawnSync } from 'node:child_process';
import { describe, expect, it } from 'vitest';
import { auditCustomProperties, checkCustomProperties } from './check-custom-properties.mjs';

const fixtures = resolve(import.meta.dirname, 'fixtures/custom-properties');
const fixture = (file: string) => ({ file, code: readFileSync(join(fixtures, file.endsWith('.tsx') ? `${file}.fixture` : file), 'utf8') });

describe('custom property existence gate', () => {
  it('accepts CSS, TSX local declarations, setProperty and explicit fallbacks', () => {
    expect(auditCustomProperties([fixture('positive.css'), fixture('positive.tsx')])).toEqual([]);
  });
  it.each(['negative.css', 'negative.tsx', 'nested-fallback.css'])('rejects the single undefined reference in %s', file => {
    expect(auditCustomProperties([fixture(file)])).toEqual([
      `${file}: undefined custom property --missing without fallback`,
    ]);
  });
  it('reads loaded vendor CSS, checks CSS/TSX recursively and fails the CLI for one violation', () => {
    const root = mkdtempSync(join(tmpdir(), 'neige-vars-'));
    const put = (file: string, code: string) => { mkdirSync(resolve(root, file, '..'), { recursive: true }); writeFileSync(join(root, file), code); };
    try {
      put('package.json', '{"name":"fixture"}');
      put('web/src/styles/entry.css', '@import "vendor/theme.css";');
      put('node_modules/vendor/package.json', '{"name":"vendor"}');
      put('node_modules/vendor/theme.css', ':root { --vendor: red; } .vendor { color: var(--runtime-vendor); }');
      put('web/src/ui/case.module.css', '.local { --local: 2px; color: var(--vendor); gap: var(--local); }');
      put('web/src/ui/case.tsx', 'const color = "var(--vendor)";');
      put('web/src/styles/not-loaded.css', ':root { --missing: red; }');
      put('web/src/ui/ignored.test.tsx', 'const color = "var(--test-only)";');
      put('web/src/ui/ignored.spec.tsx', 'const color = "var(--test-only)";');
      expect(checkCustomProperties(root)).toEqual([]);
      for (const file of ['case.module.css', 'case.tsx']) {
        const target = `web/src/ui/${file}`;
        const original = readFileSync(join(root, target), 'utf8');
        put(target, file.endsWith('.css') ? '.bad { color: var(--missing); }' : 'const color = "var(--missing)";');
        const result = spawnSync(process.execPath, [resolve(import.meta.dirname, 'check-custom-properties.mjs'), root], { encoding: 'utf8' });
        expect(result.status).toBe(1);
        expect(result.stderr.trim()).toBe(`${target}: undefined custom property --missing without fallback`);
        put(target, original);
      }
    } finally { rmSync(root, { recursive: true }); }
  });
  it('accepts the current production tree', () => {
    expect(checkCustomProperties(resolve(import.meta.dirname, '../..'))).toEqual([]);
  });
});
