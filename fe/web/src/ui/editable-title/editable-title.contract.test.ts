import { readFileSync } from 'node:fs';
import { resolve } from 'node:path';
import { describe, expect, it } from 'vitest';

const css = readFileSync(resolve(import.meta.dirname, 'editable-title.module.css'), 'utf8');

describe('editable page-title height contract', () => {
  it('keeps ellipsized titles on the single row counted by --header-h', () => {
    const titleRule = css.match(/\.title\s*\{([\s\S]*?)\}/)?.[1] ?? '';
    expect(titleRule).toMatch(/overflow:\s*hidden/);
    expect(titleRule).toMatch(/text-overflow:\s*ellipsis/);
    expect(titleRule).toMatch(/white-space:\s*nowrap/);
  });
});
