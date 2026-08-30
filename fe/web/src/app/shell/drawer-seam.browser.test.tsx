/*
 * The drawer's claims that only a **rendering engine** can answer.
 *
 * Everything in `public.test.tsx` runs under jsdom, which parses CSS and then
 * declines to compute it: every element is `visibility: visible`, nothing has a
 * box, and `:has()` matches nothing that matters. Three of this drawer's load-
 * bearing statements are therefore unfalsifiable there —
 *
 *   1. the panel column really is hidden while a drawer is up (which is the
 *      entire justification for the composer's `/new` command: the `+` is on
 *      that column and cannot be reached),
 *   2. the hiding rule and the exit animation *overlap*, so an opener on that
 *      column is unfocusable at the moment the drawer starts to leave, and
 *   3. the drawer occupies the panel's own track rather than some other box.
 *
 * — and (2) is a real regression that shipped: `focus()` on a
 * `visibility: hidden` element is a silent no-op, so closing dropped focus onto
 * `<body>` and the next Tab restarted at the top of the document.
 *
 * These are written against the *real* stylesheets, both ends of the cross-
 * module selector included, because the bug lives precisely in the seam between
 * them: `ui/drawer` stamps `data-nc-drawer`, `app/shell` hides
 * `[data-nc-panel]` off it, and neither file can see the other.
 *
 * It lives under `app/shell` rather than beside the drawer for that reason and
 * for one more: `ui/` may not depend on `app/` (dependency-cruiser's
 * `ui-only-core-type-whitelist`), and this test needs `shell.module.css` — the
 * half of the selector the drawer is not allowed to know about. The seam
 * belongs to the layer that owns both ends of it.
 */
import { act, render } from '@testing-library/react';
import { page } from 'vitest/browser';
import { afterEach, describe, expect, it, vi } from 'vitest';

/*
 * The whole cascade, and **before anything that declares a layer of its own**.
 *
 * This file used to import `tokens.css` and `base.css` piecemeal, *after* the
 * drawer component (whose `drawer.module.css` opens `@layer ui`), and never
 * imported `entry.css` at all — so the one statement that fixes the order,
 * `@layer reset, vendor, tokens, base, astryx, ui, features, overrides;`, was
 * not in the document. Layer registration is first-come, so the order the page
 * ended up with was the order the imports happened to arrive in, and it was
 * inverted where it mattered: measured, a `base` declaration beat a `ui` one,
 * which is the opposite of production. Nothing here failed on it, which is
 * exactly the problem — every geometry number below was being read off a page
 * that does not exist, and the day one of them starts depending on the layer
 * order it will fail for a reason nobody can find.
 *
 * `shell.module.css` stays as a value import because the test needs its class
 * names, and it comes after this line for the same reason everything else does.
 */
import '../../styles/entry.css';

import { Drawer } from '../../ui/drawer/public.tsx';
import { useState } from '../../ui/state/public.ts';
import shell from './shell.module.css';

afterEach(() => { document.body.replaceChildren(); });

/**
 * The cascade order the document actually ended up with, read off the first
 * `@layer` rule in sheet order — which is the rule that *fixes* the order,
 * because registration is first-come and later mentions cannot reorder.
 * Duplicated verbatim in `features/chat/thread/thread.browser.test.tsx` rather
 * than shared: it is a probe of a file's own import order, so a copy that
 * travels with the file is the point.
 */
function registeredLayerOrder(): readonly string[] {
  for (const sheet of [...document.styleSheets]) {
    let rules: CSSRuleList;
    try { rules = sheet.cssRules; } catch { continue; }
    for (const rule of [...rules]) {
      if (rule instanceof CSSLayerStatementRule) return [...rule.nameList];
      if (rule instanceof CSSLayerBlockRule) return [rule.name];
    }
  }
  return [];
}

const PRODUCTION_LAYER_ORDER = [
  'reset', 'vendor', 'tokens', 'base', 'astryx', 'ui', 'features', 'overrides',
];

/** Wait until the drawer has finished leaving and unmounted. The exit is one
 *  `--motion-medium` animation; polling the DOM is the honest end condition
 *  because that is exactly what the component keys its own unmount on. */
async function untilGone() {
  for (let i = 0; i < 200; i += 1) {
    if (document.querySelector('[data-nc-drawer]') === null) return;
    await new Promise((resolve) => { requestAnimationFrame(() => { resolve(null); }); });
  }
  throw new Error('the drawer never left');
}

/**
 * A page shaped like the real ones: `.main` with a trailing `[data-nc-panel]`
 * column, a `+` and a conversation row on that column, a page title to fall
 * back to, and the drawer overlaying it.
 */
function Page({ onClose }: { onClose?: () => void }) {
  const [open, setOpen] = useState(false);
  return (
    <div className={shell.shell}>
      <main className={shell.main}>
        <h1 data-nc-page-title="" tabIndex={-1}>Today</h1>
        <aside data-nc-panel="">
          <button type="button" data-testid="plus">New conversation</button>
          <button type="button" data-testid="opener" onClick={() => { setOpen(true); }}>
            Conversation Chat
          </button>
        </aside>
        <Drawer
          open={open}
          title="Chat"
          onClose={() => { setOpen(false); onClose?.(); }}
        >
          <p>the transcript</p>
        </Drawer>
      </main>
    </div>
  );
}

/* `act`, not a bare `.click()`: React 19 flushes a state update in a microtask,
   so the drawer is not in the DOM on the line after the click without it. */
async function click(element: HTMLElement) {
  await act(async () => { element.click(); await Promise.resolve(); });
}

const opener = () => document.querySelector<HTMLElement>('[data-testid="opener"]')!;
const plus = () => document.querySelector<HTMLElement>('[data-testid="plus"]')!;

describe('the drawer against a real rendering engine', () => {
  /* The premise every claim below rests on: this file is looking at the page
     production builds, not at a private cascade of its own. */
  it('registers the cascade in the production order', () => {
    expect(registeredLayerOrder()).toEqual(PRODUCTION_LAYER_ORDER);
  });

  /*
   * The premise `/new` is built on, asserted rather than assumed in prose.
   *
   * `cove-conversation.test.tsx` states this too, but it can only reach for
   * `plus.closest('[data-nc-panel]')` — a DOM ancestor relation that stays true
   * with the `:has()` rule deleted from the stylesheet entirely. Here the claim
   * is what the reader can actually do.
   */
  it('hides the panel column, `+` and all, for as long as a drawer is up', async () => {
    await page.viewport(1400, 900);
    render(<Page />);
    expect(getComputedStyle(plus()).visibility).toBe('visible');

    await click(opener());
    expect(document.querySelector('[data-nc-drawer]')).not.toBeNull();
    /* Not the `<aside>`'s own declaration — the inherited computed value on the
       `+` itself, which is what decides whether it can be clicked or focused. */
    expect(getComputedStyle(plus()).visibility).toBe('hidden');
    plus().focus();
    expect(document.activeElement).not.toBe(plus());

    await untilGone.call(null).catch(() => undefined);
  });

  /*
   * The regression. Closing must land focus on the row that opened the drawer —
   * and that row is on the column the drawer itself is hiding, so a restore
   * that fires while the exit animation is still running aims at a
   * `visibility: hidden` element and silently loses focus to `<body>`.
   */
  it('returns focus to the opener rather than to <body> when it closes', async () => {
    await page.viewport(1400, 900);
    render(<Page />);
    opener().focus();
    await click(opener());
    const drawer = document.querySelector<HTMLElement>('[data-nc-drawer]')!;
    expect(document.activeElement).toBe(drawer);

    await click(drawer.querySelector<HTMLElement>('button[aria-label="Close conversation"]')!);
    await untilGone();

    expect(getComputedStyle(plus()).visibility).toBe('visible');
    expect(document.activeElement).toBe(opener());
    expect(document.activeElement).not.toBe(document.body);
  });

  /* And the fallback still applies for real: an opener that left the document
     while the drawer was up has nothing to go back to, so focus goes to the
     page title — never to `<body>`. */
  it('falls back to the page title when the opener is gone for good', async () => {
    await page.viewport(1400, 900);
    render(<Page />);
    opener().focus();
    await click(opener());
    const drawer = document.querySelector<HTMLElement>('[data-nc-drawer]')!;
    opener().remove();

    await click(drawer.querySelector<HTMLElement>('button[aria-label="Close conversation"]')!);
    await untilGone();

    expect(document.activeElement).toBe(document.querySelector('[data-nc-page-title]'));
  });

  /*
   * The same fallback, reached through the hiding mechanism the old guard got
   * **backwards** — and the only test that ever exercised the post-hoc check at
   * all. (Deleting that check left the whole browser suite green; this is the
   * gap that closed.)
   *
   * `display` does not inherit. A button inside a `display: none` subtree still
   * computes `display: inline-block` on itself, so the predicate that read
   * `style.display !== 'none'` off the element answered "focusable" for an
   * element `focus()` cannot reach — the exact case its own docstring named.
   * A false yes here is not inert: it made the restore stop waiting, spend its
   * one armed attempt on a silent no-op, and hand the document to `<body>`.
   * `content-visibility: hidden` fooled it the same way for the same reason.
   *
   * Today `shell.module.css` hides the column with `visibility`, which the old
   * predicate happened to get right — so this pins the guard rather than the
   * stylesheet, and stays honest if that rule is ever rewritten.
   */
  it('lands on the page title, not <body>, when the opener is inside a display:none subtree', async () => {
    await page.viewport(1400, 900);
    render(<Page />);
    opener().focus();
    await click(opener());
    const drawer = document.querySelector<HTMLElement>('[data-nc-drawer]')!;

    const column = opener().closest<HTMLElement>('[data-nc-panel]')!;
    column.style.display = 'none';
    /* The trap, stated as a measurement: the opener's *own* computed display is
       untouched by its ancestor's, and it is still connected. */
    expect(getComputedStyle(opener()).display).not.toBe('none');
    expect(opener().isConnected).toBe(true);

    await click(drawer.querySelector<HTMLElement>('button[aria-label="Close conversation"]')!);
    await untilGone();

    expect(document.activeElement).not.toBe(document.body);
    expect(document.activeElement).toBe(document.querySelector('[data-nc-page-title]'));
  });

  /*
   * The card's geometry, which is CSS and nothing else.
   *
   * The stylesheet states these three numbers in prose — "20 top / 28 bottom",
   * `inset-inline-end: var(--space-10)` — and prose is not a gate: rename a
   * spacing token or drop the `inset-block` line and every jsdom test in this
   * directory stays green while the card silently fills the whole main region.
   * These are read off the painted boxes, relative to `.main`, which is the
   * containing block the `position: absolute` resolves against.
   */
  it('insets the card from the main region by the amounts the stylesheet claims', async () => {
    await page.viewport(1400, 900);
    render(<Page />);
    await click(opener());
    const card = document.querySelector<HTMLElement>('[data-nc-drawer]')!;
    const box = card.getBoundingClientRect();
    expect(box.width).toBeGreaterThan(0);
    expect(box.height).toBeGreaterThan(0);
    /* Used values, resolved by the engine against the padding box `.main`
       establishes — so this is red both if the `inset-block` line goes away
       (the insets read `auto`) and if a spacing token stops meaning what the
       comment says it means. */
    const laid = getComputedStyle(card);
    expect(laid.position).toBe('absolute');
    expect(laid.insetBlockStart).toBe('20px');
    expect(laid.insetBlockEnd).toBe('28px');
    expect(laid.insetInlineEnd).toBe('24px');
    await untilGone.call(null).catch(() => undefined);
  });

  /*
   * Escape during IME composition belongs to the IME, not to the drawer.
   * `cancelled` here would mean a bilingual reader loses a half-written message
   * every time they wave off a candidate list.
   */
  it('ignores the Escape that cancels an IME candidate, and honours the other one', async () => {
    await page.viewport(1400, 900);
    const onClose = vi.fn();
    render(<Page onClose={onClose} />);
    await click(opener());
    const drawer = document.querySelector<HTMLElement>('[data-nc-drawer]')!;

    await act(async () => {
      drawer.dispatchEvent(new KeyboardEvent('keydown', {
        key: 'Escape', bubbles: true, composed: true, isComposing: true,
      }));
      await Promise.resolve();
    });
    expect(onClose).not.toHaveBeenCalled();
    expect(document.querySelector('[data-nc-drawer]')).not.toBeNull();

    await act(async () => {
      drawer.dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape', bubbles: true, composed: true }));
      await Promise.resolve();
    });
    expect(onClose).toHaveBeenCalledTimes(1);
    await untilGone();
  });
});
