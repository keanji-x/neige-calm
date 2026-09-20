/*
 * The drawer's claims only a rendering engine can answer: jsdom parses CSS and
 * declines to compute it. `focus()` on a `visibility: hidden` element is a silent
 * no-op, so the seam between `ui/drawer` and `app/shell`'s `[data-nc-panel]` rule is tested here.
 */
import { act, render, waitFor } from '@testing-library/react';
import { page } from 'vitest/browser';
import { afterEach, describe, expect, it, vi } from 'vitest';

/* The whole cascade, before anything that declares a layer of its own: layer
 * registration is first-come, so importing a CSS Module first inverts the order. */
import '../../styles/entry.css';

import type { ReportOutlineItem } from '../../../../core/domain/report.ts';
import reportDocument from '../../features/report/document/document.module.css';
import { ReportOutline } from '../../features/report/outline/public.tsx';
import trackPage from '../../features/track/page/page.module.css';
import { Drawer } from '../../ui/drawer/public.tsx';
import { useState } from '../../ui/state/public.ts';
import shell from './shell.module.css';

afterEach(() => { document.body.replaceChildren(); });

/**
 * The cascade order the document ended up with, read off the first top-level
 * `@layer` rule in sheet order. It does not see `@import ... layer(x)` or a layer
 * opened inside `@media`, which this app's build does not produce.
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

/** Wait until the drawer has finished leaving and unmounted; the DOM is what the component keys its own unmount on. */
async function untilGone() {
  for (let i = 0; i < 200; i += 1) {
    if (document.querySelector('[data-nc-drawer]') === null) return;
    await new Promise((resolve) => { requestAnimationFrame(() => { resolve(null); }); });
  }
  throw new Error('the drawer never left');
}

/** Enough of an element to tell the opener, the page title and `<body>` apart in a failure message. */
function describeFocus(element: Element | null): string {
  if (element === null) return 'null';
  const testid = element.getAttribute('data-testid');
  const text = (element.textContent ?? '').trim().slice(0, 40);
  return `<${element.tagName.toLowerCase()}`
    + (testid === null ? '' : ` data-testid="${testid}"`)
    + `>${text}`;
}

/**
 * Wait until `document.activeElement` is what `target` names. `untilGone()` is not
 * this: the restore runs in the effect the unmounting commit schedules, so the
 * first frame with the drawer gone legitimately still has focus on `<body>`.
 */
async function untilFocused(target: () => Element | null, expected: string) {
  for (let i = 0; i < 200; i += 1) {
    // `!== null` first: `activeElement === null` on a detached document would make the wait vacuously true.
    const element = target();
    if (element !== null && document.activeElement === element) return;
    await new Promise((resolve) => { requestAnimationFrame(() => { resolve(null); }); });
  }
  throw new Error(
    `focus never reached ${expected}; document.activeElement is `
    + describeFocus(document.activeElement),
  );
}

/** A page shaped like the real ones: `.main` with a trailing `[data-nc-panel]` column, a page title to fall back to, and the drawer overlaying it. */
function Page({ onClose }: { onClose?: () => void }) {
  const [open, setOpen] = useState(false);
  return (
    <div className={shell.shell}>
      {/* The shell's navigation owns column one; keep that track occupied so `.main`
                resolves its cqi against the real second column. */}
      <div aria-hidden="true" />
      <main className={shell.main}>
        <span data-testid="panel-span-probe" style={{ position: 'absolute', inlineSize: 'var(--panel-span)' }} />
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

const OUTLINE_ITEMS: ReportOutlineItem[] = [
  { blockId: 'section', label: 'Section', number: 1, children: [] },
];

function TrackGeometryPage({ open = true, withOutline = true }: { open?: boolean; withOutline?: boolean }) {
  return (
    <div className={shell.shell}>
      <div aria-hidden="true" />
      <main className={shell.main}>
        <section className={trackPage.page}>
          <header />
          <div className={trackPage.workspace}>
            <div className={trackPage.content}>
              <div className={trackPage.doc}>
                <article
                  className={reportDocument.doc}
                  data-nc-report=""
                  style={{
                    blockSize: 600,
                  }}
                >
                  {withOutline && <ReportOutline items={OUTLINE_ITEMS} />}
                  <div className={reportDocument.row}>
                    <div className={reportDocument.block} data-testid="report-measure">
                      <h2 className={reportDocument.h1} data-testid="section-heading">Section</h2>
                    </div>
                  </div>
                </article>
              </div>
              <aside className={trackPage.panel} data-nc-panel="" />
            </div>
          </div>
        </section>
        <Drawer open={open} title="Chat" onClose={() => undefined}>
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
  it('reclaims the report left margin when the wider conversation opens', async () => {
    await page.viewport(1400, 900);
    render(<Page />);
    const main = document.querySelector('main')!;
    const probe = document.querySelector<HTMLElement>('[data-testid="panel-span-probe"]')!;
    expect(probe.getBoundingClientRect().width / main.getBoundingClientRect().width).toBeCloseTo(0.25, 2);
    await click(opener());
    expect(probe.getBoundingClientRect().width / main.getBoundingClientRect().width).toBeCloseTo(0.4, 2);
  });

  it('keeps the report clear of a wide conversation on narrow desktop widths', async () => {
    await page.viewport(1024, 800);
    render(<TrackGeometryPage />);
    const report = document.querySelector<HTMLElement>('[data-testid="report-measure"]')!;
    const drawer = document.querySelector<HTMLElement>('[data-nc-drawer]')!;
    const outline = document.querySelector<HTMLElement>('nav[aria-label="Outline"]')!;
    const heading = document.querySelector<HTMLElement>('[data-testid="section-heading"]')!;
    expect(report.getBoundingClientRect().right).toBeLessThanOrEqual(drawer.getBoundingClientRect().left);
    expect(getComputedStyle(outline).display).toBe('none');
    expect(getComputedStyle(heading, '::before').opacity).toBe('1');

    await page.viewport(1200, 800);
    expect(report.getBoundingClientRect().right).toBeLessThanOrEqual(drawer.getBoundingClientRect().left);
    expect(getComputedStyle(outline).display).toBe('none');
    expect(getComputedStyle(heading, '::before').opacity).toBe('1');

    await page.viewport(1440, 900);
    expect(report.getBoundingClientRect().width).toBeCloseTo(568, 0);
    expect(getComputedStyle(outline).display).toBe('none');
    expect(getComputedStyle(heading, '::before').opacity).toBe('1');

    await page.viewport(1600, 900);
    expect(getComputedStyle(outline).display).not.toBe('none');
    await waitFor(() => expect(getComputedStyle(heading, '::before').opacity).toBe('0'));
    await page.getByRole('button', { name: 'Section' }).hover();
    await waitFor(() => expect(document.querySelector('[data-nc-rail-preview]')).not.toBeNull());
    const preview = document.querySelector<HTMLElement>('[data-nc-rail-preview]')!;
    const main = document.querySelector('main')!;
    expect(preview.getBoundingClientRect().left).toBeGreaterThanOrEqual(main.getBoundingClientRect().left);
  });

  it('shows the report outline at 1366px when no conversation consumes its gutter', async () => {
    await page.viewport(1366, 800);
    render(<TrackGeometryPage open={false} />);
    const outline = document.querySelector<HTMLElement>('nav[aria-label="Outline"]')!;
    const heading = document.querySelector<HTMLElement>('[data-testid="section-heading"]')!;
    expect(getComputedStyle(outline).display).not.toBe('none');
    expect(getComputedStyle(heading, '::before').opacity).toBe('0');
  });

  it('keeps section numbers on an ultrawide drawer report that has no outline', async () => {
    await page.viewport(1600, 900);
    render(<TrackGeometryPage withOutline={false} />);
    const heading = document.querySelector<HTMLElement>('[data-testid="section-heading"]')!;
    expect(document.querySelector('nav[aria-label="Outline"]')).toBeNull();
    expect(getComputedStyle(heading, '::before').opacity).toBe('1');
  });

  /* The premise every claim below rests on. */
  it('registers the cascade in the production order', () => {
    expect(registeredLayerOrder()).toEqual(PRODUCTION_LAYER_ORDER);
  });

  /* `plus.closest('[data-nc-panel]')` stays true with the `:has()` rule deleted;
   * here the claim is what the reader can actually do. */
  it('hides the panel column, `+` and all, for as long as a drawer is up', async () => {
    await page.viewport(1400, 900);
    render(<Page />);
    expect(getComputedStyle(plus()).visibility).toBe('visible');

    await click(opener());
    expect(document.querySelector('[data-nc-drawer]')).not.toBeNull();
    /* The inherited computed value on the `+` itself, which is what decides whether
           it can be clicked or focused. */
    expect(getComputedStyle(plus()).visibility).toBe('hidden');
    plus().focus();
    expect(document.activeElement).not.toBe(plus());

    await untilGone.call(null).catch(() => undefined);
  });

  /* The opener is on the column the drawer hides, so a restore that fires during
   * the exit animation aims at a `visibility: hidden` element and loses focus to `<body>`. */
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
    /* `untilFocused` is green for the opener and nothing else, so `<body>` fails by timing out. */
    await untilFocused(opener, 'the opener');
  });

  /* An opener that left the document has nothing to go back to, so focus goes to
       the page title, never `<body>`. */
  it('falls back to the page title when the opener is gone for good', async () => {
    await page.viewport(1400, 900);
    render(<Page />);
    opener().focus();
    await click(opener());
    const drawer = document.querySelector<HTMLElement>('[data-nc-drawer]')!;
    opener().remove();

    await click(drawer.querySelector<HTMLElement>('button[aria-label="Close conversation"]')!);
    await untilGone();

    await untilFocused(
      () => document.querySelector('[data-nc-page-title]'),
      'the page title',
    );
  });

  /* `display` does not inherit: a button inside a `display: none` subtree still
   * computes `display: inline-block` on itself. The opener is hidden by `display`
   * alone, on a host outside the panel column, conditionally on the drawer being
   * up, so an early fallback and a waited restore end in different places. */
  it('waits out the retraction for an opener only `display` was hiding, and lands on it', async () => {
    await page.viewport(1400, 900);
    render(<Page />);
    const main = document.querySelector('main')!;

    /* A host outside `[data-nc-panel]`, so `display` is the only thing in play; hidden
           by a rule keyed on the drawer's marker so it un-hides in the commit the drawer
           unmounts in. */
    const host = main.appendChild(document.createElement('div'));
    host.dataset.testid = 'host';
    const hiddenOpener = host.appendChild(document.createElement('button'));
    hiddenOpener.textContent = 'Opener on a display-hidden host';
    const sheet = document.head.appendChild(document.createElement('style'));
    sheet.textContent = 'main:has([data-nc-drawer]) [data-testid="host"] { display: none }';

    hiddenOpener.focus();
    await click(opener());
    const drawer = document.querySelector<HTMLElement>('[data-nc-drawer]')!;

    /* The opener's own computed display, visibility and connectedness all say
           "focusable" about an element `focus()` cannot reach. */
    const hiddenStyle = getComputedStyle(hiddenOpener);
    expect(getComputedStyle(host).display).toBe('none');
    expect(hiddenStyle.display).not.toBe('none');
    expect(hiddenStyle.visibility).toBe('visible');
    expect(hiddenOpener.isConnected).toBe(true);
    hiddenOpener.focus();
    expect(document.activeElement).not.toBe(hiddenOpener);

    await click(drawer.querySelector<HTMLElement>('button[aria-label="Close conversation"]')!);

    /* Mid-retraction; with no live `closing` frame the next line would pass vacuously. */
    expect(document.querySelector('[data-nc-drawer]')).not.toBeNull();
    const pageTitle = document.querySelector('[data-nc-page-title]');
    expect(document.activeElement).not.toBe(pageTitle);

    await untilGone();

    /* The host is back, so the reader lands on the control they left from; the
           early fallback is caught by the mid-retraction line above. */
    expect(getComputedStyle(host).display).not.toBe('none');
    await untilFocused(() => hiddenOpener, 'the display-hidden opener');

    sheet.remove();
    host.remove();
  });

  /* The card's geometry, read off the painted boxes relative to `.main`, the
   * containing block the `position: absolute` resolves against. */
  it('insets the card from the main region by the amounts the stylesheet claims', async () => {
    await page.viewport(1400, 900);
    render(<Page />);
    await click(opener());
    const card = document.querySelector<HTMLElement>('[data-nc-drawer]')!;
    const box = card.getBoundingClientRect();
    const mainBox = card.closest('main')!.getBoundingClientRect();
    /* 40% of the main region at this viewport; the trailing panel stays on its 25% track underneath. */
    expect(box.width / mainBox.width).toBeCloseTo(0.4, 2);
    expect(box.height).toBeGreaterThan(0);
    /* Used values: red both if the `inset-block` line goes away (`auto`) and if a
           spacing token stops meaning what the stylesheet claims. */
    const laid = getComputedStyle(card);
    expect(laid.position).toBe('absolute');
    expect(laid.insetBlockStart).toBe('20px');
    expect(laid.insetBlockEnd).toBe('28px');
    expect(laid.insetInlineEnd).toBe('24px');
    await untilGone.call(null).catch(() => undefined);
  });

  /* Escape during IME composition belongs to the IME, not to the drawer. */
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
