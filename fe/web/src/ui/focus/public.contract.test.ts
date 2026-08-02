import { describe, expect, it } from 'vitest';
import { findTypeaheadMatch, normalizeTypeaheadLabel } from './public.ts';

describe('focus public contract', () => {
  it('normalizes with trim, lowercase, and prefix matching', () => {
    expect(normalizeTypeaheadLabel('  ALPHA ')).toBe('alpha');
    expect(findTypeaheadMatch(['Alpha', 'Beta', 'Alpine'], 'al', 0)).toBe(0);
    expect(findTypeaheadMatch(['Alpha', 'Beta', 'Alpine'], 'a', 0)).toBe(2);
    expect(findTypeaheadMatch(['Alpha', 'Beta', 'Alpine'], 'b', 2)).toBe(1);
  });

  it('locks each handled and intentionally-unhandled key literal independently', async () => {
    const source = await import('node:fs/promises').then((fs) => fs.readFile(new URL('./public.ts', import.meta.url), 'utf8'));
    for (const key of ['ArrowDown', 'ArrowUp', 'Home', 'End', 'Enter', 'Escape', ' '] as const) expect(source).toContain(`case '${key}'`);
    expect(source).not.toContain("case 'ArrowLeft'");
    expect(source).not.toContain("case 'ArrowRight'");
  });
});
