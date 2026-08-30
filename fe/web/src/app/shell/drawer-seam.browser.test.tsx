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

import { Drawer } from '../../ui/drawer/public.tsx';
import { useState } from '../../ui/state/public.ts';
import shell from './shell.module.css';
import '../../styles/tokens.css';
import '../../styles/base.css';

afterEach(() => { document.body.replaceChildren(); });

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
