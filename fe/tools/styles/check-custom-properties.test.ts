import { mkdtempSync, mkdirSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';
import { spawnSync } from 'node:child_process';
import { describe, expect, it } from 'vitest';
import { auditCustomProperties, checkCustomProperties } from './check-custom-properties.mjs';

const fixtures = resolve(import.meta.dirname, 'fixtures/custom-properties');
// Source fixtures remain text assets; pass their logical extension to the real parser.
const fixture = (file: string) => ({ file, code: readFileSync(join(fixtures, /\.tsx?$/.test(file) ? `${file}.txt` : file), 'utf8') });

describe('custom property existence gate', () => {
  it.each(['positive.css', 'quoted-double.css', 'quoted-single.css', 'escaped-double.css', 'escaped-single.css', 'fallback.css', 'quoted-comments.css', 'comments.css', 'url-literal-var.css', 'url-escaped-close.css', 'url-hex-escape.css', 'string-continuation-double.css', 'string-continuation-single.css', 'string-hex-double.css', 'string-hex-single.css', 'opaque-var-argument.css'])('accepts %s', file => {
    expect(auditCustomProperties([fixture(file)])).toEqual([]);
  });
  it.each(['negative.css', 'nested-fallback.css', 'outside-string.css', 'after-escaped-string.css', 'escaped-backslash.css', 'url-escaped-double.css', 'url-escaped-single.css', 'outside-escaped-quote.css', 'outside-hex-escape.css', 'bad-string-double.css', 'bad-string-single.css', 'comment-quote-before-var.css', 'comment-before-var-name.css'])('rejects the single undefined reference in %s', file => {
    expect(auditCustomProperties([fixture(file)])).toEqual([
      `${file}: undefined custom property --missing without fallback`,
    ]);
  });
  it.each(['property-name.tsx', 'set-property.tsx', 'double-literal.tsx', 'static-template.tsx', 'source.ts'])('accepts CSS uses defined by %s', file => {
    expect(auditCustomProperties([fixture('source-use.css'), fixture(file)])).toEqual([]);
  });
  it.each(['use-only.tsx', 'comment-only.tsx', 'interpolated-template.tsx', 'partial-literal.tsx'])('rejects a CSS use left undefined by %s', file => {
    expect(auditCustomProperties([fixture('source-use.css'), fixture(file)])).toEqual([
      'source-use.css: undefined custom property --from-source without fallback',
    ]);
  });
  it('does not check TSX uses', () => {
    expect(auditCustomProperties([fixture('use-only.tsx')])).toEqual([]);
  });
  it('reads loaded vendor CSS, CSS Modules and production source definitions recursively', () => {
    withSourceTree((root, put) => {
      expect(checkCustomProperties(root)).toEqual([]);
      for (const file of ['property-name.tsx', 'set-property.tsx', 'double-literal.tsx', 'static-template.tsx', 'source.ts']) {
        const target = file.endsWith('.tsx') ? 'web/src/ui/source.tsx' : 'core/source.ts';
        put(target, fixture(file).code);
        put('web/src/ui/case.module.css', fixture('source-use.css').code);
        expect(checkCustomProperties(root)).toEqual([]);
        rmSync(join(root, target));
      }
    });
  });
  it.each(['use-only.tsx', 'comment-only.tsx', 'interpolated-template.tsx', 'partial-literal.tsx'])('fails the CLI for a single CSS violation with %s', file => {
    withSourceTree((root, put) => {
      put('web/src/ui/source.tsx', fixture(file).code);
      put('web/src/ui/case.module.css', fixture('source-use.css').code);
      expectCliViolation(root, '--from-source');
    });
  });
  it.each(['negative.css', 'nested-fallback.css', 'outside-string.css', 'after-escaped-string.css', 'escaped-backslash.css', 'url-escaped-double.css', 'url-escaped-single.css', 'outside-escaped-quote.css', 'outside-hex-escape.css', 'bad-string-double.css', 'bad-string-single.css', 'comment-quote-before-var.css', 'comment-before-var-name.css'])('fails the CLI for the single violation in %s', file => {
    withSourceTree((root, put) => {
      put('web/src/ui/case.module.css', fixture(file).code);
      expectCliViolation(root, '--missing');
    });
  });
  it('accepts the current production tree', () => {
    expect(checkCustomProperties(resolve(import.meta.dirname, '../..'))).toEqual([]);
  });
});

function expectCliViolation(root: string, name: string) {
  const result = spawnSync(process.execPath, [resolve(import.meta.dirname, 'check-custom-properties.mjs'), root], { encoding: 'utf8' });
  expect(result.status).toBe(1);
  expect(result.stderr.trim()).toBe(`web/src/ui/case.module.css: undefined custom property ${name} without fallback`);
}

function withSourceTree(run: (root: string, put: (file: string, code: string) => void) => void) {
  const root = mkdtempSync(join(tmpdir(), 'neige-vars-'));
  const put = (file: string, code: string) => { mkdirSync(resolve(root, file, '..'), { recursive: true }); writeFileSync(join(root, file), code); };
  try {
    mkdirSync(join(root, 'core'));
    put('package.json', '{"name":"fixture"}');
    put('web/src/styles/entry.css', '@import "vendor/theme.css";');
    put('node_modules/vendor/package.json', '{"name":"vendor"}');
    put('node_modules/vendor/theme.css', ':root { --vendor: red; } .vendor { color: var(--runtime-vendor); }');
    put('web/src/ui/case.module.css', '.local { --local: 2px; color: var(--vendor); gap: var(--local); }');
    put('web/src/styles/not-loaded.css', ':root { --missing: red; }');
    put('web/src/ui/source.test.tsx', "const name = '--missing';");
    put('web/src/ui/source.spec.tsx', "const name = '--missing';");
    run(root, put);
  } finally { rmSync(root, { recursive: true }); }
}
