import { spawnSync } from 'node:child_process';
import { copyFileSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { dirname, join, resolve } from 'node:path';
import { expect, it } from 'vitest';

/** Run the real CLI with isolated token fixtures; the repository's frozen CSS is never edited. */
function check(replacement?: readonly [string, string]) {
  const root = mkdtempSync(join(tmpdir(), 'neige-contrast-'));
  const source = resolve(import.meta.dirname, '../..');
  try {
    for (const path of ['tools/styles/check-contrast.mjs', 'web/src/styles/tokens.css', 'web/src/features/settings/settings.module.css']) {
      mkdirSync(dirname(join(root, path)), { recursive: true });
      copyFileSync(join(source, path), join(root, path));
    }
    if (replacement !== undefined) {
      const path = join(root, 'web/src/styles/tokens.css');
      const [token, value] = replacement;
      const css = readFileSync(path, 'utf8');
      const changed = css.replaceAll(new RegExp(`${token}: [^;]+;`, 'g'), `${token}: ${value};`);
      expect(changed).not.toBe(css);
      writeFileSync(path, changed);
    }
    return spawnSync(process.execPath, [join(root, 'tools/styles/check-contrast.mjs')], { encoding: 'utf8' });
  } finally { rmSync(root, { recursive: true }); }
}

it('accepts the shipped semantic ink recipes', () => {
  const result = check();
  expect(result.status, result.stderr).toBe(0);
});

it.each([
  ['--text-3', 'oklch(99.4% 0.004 90)', ['ghost setting neutral rest']],
  ['--text', 'oklch(99.4% 0.004 90)', ['ghost setting neutral hover', 'ghost setting neutral active/expanded']],
  ['--warn-text', 'oklch(99.4% 0.004 90)', ['ghost setting warning rest', 'ghost setting warning hover', 'ghost setting warning active/expanded']],
  ['--error-text', 'oklch(99.4% 0.004 90)', ['ghost setting danger rest', 'ghost setting danger hover', 'ghost setting danger active/expanded',
    'destructive menu text rest', 'destructive menu text hover', 'destructive menu text active/expanded']],
  ['--overlay-hover', 'var(--text)', ['ghost setting neutral hover']],
  ['--overlay-active', 'var(--text)', ['ghost setting neutral active/expanded']],
] as const)('rejects unreadable settings caused by %s', (token, value, recipes) => {
  const result = check([token, value]);
  expect(result.status).toBe(1);
  for (const recipe of recipes) for (const surface of ['--surface-card', '--paper']) {
    expect(result.stderr).toContain(`light ${recipe} on ${surface}:`);
  }
});
