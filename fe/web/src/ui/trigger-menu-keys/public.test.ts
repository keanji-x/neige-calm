import { afterEach, describe, expect, it } from 'vitest';

import { triggerMenuKeyRoute } from './public.ts';

afterEach(() => { document.body.replaceChildren(); });

/** A field and the listbox it controls, in the attribute shape Astryx's `useTriggerMenu` publishes. */
function field(attributes: Readonly<Record<string, string>>, listbox = ''): Element {
  document.body.innerHTML = `<div contenteditable="true"></div><div id="menu" role="listbox">${listbox}</div>`;
  const element = document.body.firstElementChild!;
  for (const [name, value] of Object.entries(attributes)) element.setAttribute(name, value);
  return element;
}

const typing = { isComposing: false };
const open = { 'aria-expanded': 'true', 'aria-controls': 'menu' };

describe('triggerMenuKeyRoute', () => {
  it('leaves a composing key to the IME, whatever the menu shows', () => {
    expect(triggerMenuKeyRoute({ isComposing: true }, field({ ...open, 'aria-activedescendant': 'row' },
      '<div id="row" role="option"></div>'))).toBe('composing');
  });

  it('gives the key to the menu for a rendered highlighted option', () => {
    expect(triggerMenuKeyRoute(typing, field({ ...open, 'aria-activedescendant': 'row' },
      '<div id="row" role="option"></div>'))).toBe('menu');
  });

  it('drops the key while the menu is searching, even with a stale highlight', () => {
    expect(triggerMenuKeyRoute(typing, field({ ...open, 'aria-activedescendant': 'gone' },
      '<div role="status">Searching…</div>'))).toBe('swallow');
  });

  it('leaves the key to the composer when the menu is closed or has nothing to pick', () => {
    expect(triggerMenuKeyRoute(typing, field({ 'aria-expanded': 'false' }))).toBe('composer');
    expect(triggerMenuKeyRoute(typing, field({}))).toBe('composer');
    expect(triggerMenuKeyRoute(typing, field(open, '<div>No results</div>'))).toBe('composer');
  });
});
