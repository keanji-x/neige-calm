import { readFile } from 'node:fs/promises';
import { describe, expect, it } from 'vitest';

describe('dialog public contract', () => {
  it('locks inert cleanup before focus restore by declaration order', async () => {
    const source = await readFile(new URL('./public.tsx', import.meta.url), 'utf8');
    const inert = source.indexOf("element.setAttribute('inert', '')");
    const restore = source.indexOf('const target = restoreFocusRef?.current');
    expect(inert).toBeGreaterThan(-1);
    expect(restore).toBeGreaterThan(inert);
  });

  it('locks role and aria attribute names as independent literals', async () => {
    const source = await readFile(new URL('./public.tsx', import.meta.url), 'utf8');
    for (const literal of ['role="presentation"', 'role="dialog"', 'aria-modal="true"', 'aria-label=', 'tabIndex={-1}'] as const) expect(source).toContain(literal);
    expect(source).not.toContain('<dialog');
  });
});
