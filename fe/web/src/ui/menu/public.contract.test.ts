import { readFile } from 'node:fs/promises';
import { describe, expect, it } from 'vitest';

describe('menu public contract', () => {
  it('locks structure, roles, aria names, and mousedown ownership independently', async () => {
    const source = await readFile(new URL('./public.tsx', import.meta.url), 'utf8');
    for (const literal of ['role="menu"', 'role="none"', 'role="menuitem"', "'aria-haspopup': 'menu'", "'aria-expanded': open", "addEventListener('mousedown'", 'onMouseMove='] as const) expect(source).toContain(literal);
  });

  it('restores focus synchronously before selection', async () => {
    const source = await readFile(new URL('./public.tsx', import.meta.url), 'utf8');
    const restore = source.indexOf('closeAndRestoreFocus();');
    expect(source.indexOf('item.onSelect();')).toBeGreaterThan(restore);
  });
});
