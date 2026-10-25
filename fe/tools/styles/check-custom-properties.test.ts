import { mkdtempSync, mkdirSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';
import { spawnSync } from 'node:child_process';
import { describe, expect, it } from 'vitest';
import { auditCustomProperties, checkCustomProperties } from './check-custom-properties.mjs';

const fixtures = resolve(import.meta.dirname, 'fixtures/custom-properties');
const fixture = (file: string) => ({ file, code: readFileSync(join(fixtures, file), 'utf8') });

describe('custom property existence gate', () => {
  it.each(['positive.css', 'quoted-double.css', 'quoted-single.css', 'escaped-double.css', 'escaped-single.css', 'fallback.css', 'quoted-comments.css', 'comments.css'])('accepts %s', file => {
    expect(auditCustomProperties([fixture(file)])).toEqual([]);
  });
  it.each(['negative.css', 'nested-fallback.css', 'outside-string.css', 'after-escaped-string.css', 'escaped-backslash.css'])('rejects the single undefined reference in %s', file => {
    expect(auditCustomProperties([fixture(file)])).toEqual([
      `${file}: undefined custom property --missing without fallback`,
    ]);
  });
  it('reads loaded vendor CSS, checks CSS Modules recursively and fails the CLI for each single violation', () => {
    const root = mkdtempSync(join(tmpdir(), 'neige-vars-'));
    const put = (file: string, code: string) => { mkdirSync(resolve(root, file, '..'), { recursive: true }); writeFileSync(join(root, file), code); };
    try {
      put('package.json', '{"name":"fixture"}');
      put('web/src/styles/entry.css', '@import "vendor/theme.css";');
      put('node_modules/vendor/package.json', '{"name":"vendor"}');
      put('node_modules/vendor/theme.css', ':root { --vendor: red; } .vendor { color: var(--runtime-vendor); }');
      put('web/src/ui/case.module.css', '.local { --local: 2px; color: var(--vendor); gap: var(--local); }');
      put('web/src/styles/not-loaded.css', ':root { --missing: red; }');
      expect(checkCustomProperties(root)).toEqual([]);
      for (const file of ['negative.css', 'nested-fallback.css', 'outside-string.css', 'after-escaped-string.css', 'escaped-backslash.css']) {
        const target = 'web/src/ui/case.module.css';
        put(target, fixture(file).code);
        const result = spawnSync(process.execPath, [resolve(import.meta.dirname, 'check-custom-properties.mjs'), root], { encoding: 'utf8' });
        expect(result.status).toBe(1);
        expect(result.stderr.trim()).toBe(`${target}: undefined custom property --missing without fallback`);
      }
    } finally { rmSync(root, { recursive: true }); }
  });
  it('accepts the current production tree', () => {
    expect(checkCustomProperties(resolve(import.meta.dirname, '../..'))).toEqual([]);
  });
});
