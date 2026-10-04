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

import type { Conversation, TranscriptEntry } from '../../../../core/domain/conversation.ts';
import type { ReportOutlineItem } from '../../../../core/domain/report.ts';
import { ChatThread } from '../../features/chat/thread/public.tsx';
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

/* #1923 S4: the drawer's desktop reading-width toggle, against the real shell span rules. */
describe('expanded reading width', () => {
  const conversation: Conversation = {
    id: 'c1', trackId: 't1', trackTitle: 'Track', title: null, kind: 'codex', state: 'idle', updatedAt: 1, turns: 1,
  };
  const WIDE_LINE = `const route = ${'resolveRoute(origin, destination).'.repeat(8)}hops;`;
  const WIDE_TABLE = [
    `| ${Array.from({ length: 12 }, (_, index) => `Column ${index + 1}`).join(' | ')} |`,
    `| ${Array.from({ length: 12 }, () => '---').join(' | ')} |`,
    `| ${Array.from({ length: 12 }, (_, index) => `cell-${index + 1}-without-any-break-opportunity`).join(' | ')} |`,
  ].join('\n');
  /** One unformatted paragraph many lines long at either width: every token is distinct, so a word can be followed through the rewrap. */
  const LONG_PARAGRAPH = Array.from({ length: 700 }, (_, index) => `w${index + 1}`).join(' ');
  const REPLY = ['The plan, as code and as a table.', '', '```ts', WIDE_LINE, '```', '', WIDE_TABLE, '', LONG_PARAGRAPH, '',
    ...Array.from({ length: 80 }, (_, index) => `Paragraph ${index + 1} of the long reply, long enough to wrap across the conversation column at either width.`)].join('\n\n');
  const TURNS: readonly TranscriptEntry[] = [
    { id: 'u1', author: 'you', text: 'Show me the plan.', atMs: 1 },
    { id: 'r1', author: 'agent', text: REPLY, atMs: 2 },
  ];

  /** `Page`, with the reading width owned by the page the way the router owns it, and a long reply. */
  function WidthPage({ onClose, initiallyOpen = false, title = 'Chat' }: { onClose?: () => void; initiallyOpen?: boolean; title?: string }) {
    const [open, setOpen] = useState(initiallyOpen);
    const [expanded, setExpanded] = useState(false);
    return (
      <div className={shell.shell}>
        <div aria-hidden="true" />
        <main className={shell.main}>
          <span data-testid="panel-span-probe" style={{ position: 'absolute', inlineSize: 'var(--panel-span)' }} />
          <h1 data-nc-page-title="" tabIndex={-1}>Today</h1>
          <aside data-nc-panel="">
            <button type="button" data-testid="opener" onClick={() => { setOpen(true); }}>Conversation Chat</button>
          </aside>
          <Drawer open={open} title={title} onClose={() => { setOpen(false); onClose?.(); }}
            readingWidth={{ expanded, onExpandedChange: setExpanded }}>
            <ChatThread canContinue conversation={conversation} turns={TURNS} cards={{}} stalled={false} />
          </Drawer>
        </main>
      </div>
    );
  }

  const drawer = () => document.querySelector<HTMLElement>('[data-nc-drawer]')!;
  const scroller = () => document.querySelector<HTMLElement>('[data-nc-drawer-scroll]')!;
  const toggle = () => drawer().querySelector<HTMLElement>('button[aria-pressed]');
  const spanOf = (element: Element) => element.getBoundingClientRect().width / document.querySelector('main')!.getBoundingClientRect().width;
  const scrollsSideways = (box: Element) => ['auto', 'scroll'].includes(getComputedStyle(box).overflowX);
  /** The box that scrolls `element` sideways: Astryx puts it inside a code block and around a table. Never the drawer's own scroller. */
  function sidewaysScroller(element: Element): HTMLElement | null {
    const inside = [element, ...element.querySelectorAll('*')].find(scrollsSideways);
    if (inside !== undefined) return inside as HTMLElement;
    for (let box = element.parentElement; box !== null && box !== scroller(); box = box.parentElement) {
      if (scrollsSideways(box)) return box;
    }
    return null;
  }
  /** The element holding a paragraph's text: Astryx's Markdown paragraphs are not `<p>`. */
  function paragraphStarting(prefix: string): HTMLElement {
    const walker = document.createTreeWalker(drawer(), NodeFilter.SHOW_TEXT);
    for (let node = walker.nextNode(); node !== null; node = walker.nextNode()) {
      if (node.textContent?.startsWith(prefix)) return node.parentElement!;
    }
    throw new Error(`no paragraph starts with ${prefix}`);
  }
  async function settled() {
    await Promise.all(drawer().getAnimations().map((animation) => animation.finished));
    await new Promise((resolve) => { requestAnimationFrame(() => { resolve(null); }); });
  }
  /** Neither the page nor the drawer's pane scrolls sideways; the code block and the table each scroll in their own box. */
  function expectNoSidewaysPage() {
    expect(document.documentElement.scrollWidth).toBeLessThanOrEqual(document.documentElement.clientWidth);
    expect(scroller().scrollWidth).toBeLessThanOrEqual(scroller().clientWidth);
    for (const [name, element] of [['code block', drawer().querySelector('pre')!], ['table', drawer().querySelector('table')!]] as const) {
      const own = sidewaysScroller(element);
      expect(own, name).not.toBeNull();
      expect(own!.scrollWidth, name).toBeGreaterThan(own!.clientWidth);
    }
  }

  it('widens the card and the panel track it covers, and narrows them back', async () => {
    await page.viewport(1400, 900);
    render(<WidthPage />);
    await click(opener());
    const probe = document.querySelector<HTMLElement>('[data-testid="panel-span-probe"]')!;
    expect(spanOf(drawer())).toBeCloseTo(0.4, 2);
    expect(toggle()!.getAttribute('aria-label')).toBe('Expand reading width');

    await click(toggle()!);
    expect(drawer().hasAttribute('data-nc-drawer-expanded')).toBe(true);
    expect(spanOf(drawer())).toBeCloseTo(0.7, 2);
    expect(spanOf(probe)).toBeCloseTo(0.7, 2);
    const laid = getComputedStyle(drawer());
    expect(laid.insetInlineEnd).toBe('24px');
    expect(drawer().getBoundingClientRect().left).toBeGreaterThanOrEqual(document.querySelector('main')!.getBoundingClientRect().left + 24);

    await click(toggle()!);
    expect(toggle()!.getAttribute('aria-label')).toBe('Expand reading width');
    expect(spanOf(drawer())).toBeCloseTo(0.4, 2);
    expect(spanOf(probe)).toBeCloseTo(0.4, 2);
  });

  it('keeps focus on the toggle and the reader where they were in the transcript', async () => {
    await page.viewport(1400, 900);
    render(<WidthPage />);
    await click(opener());
    await settled();
    const pane = scroller();
    expect(pane.scrollHeight).toBeGreaterThan(pane.clientHeight * 2);
    pane.scrollTop = pane.scrollHeight;
    await page.getByRole('button', { name: 'Expand reading width' }).click();
    expect(document.activeElement).toBe(toggle());
    expect(toggle()!.getAttribute('aria-pressed')).toBe('true');
    expect(scroller()).toBe(pane);
    expect(pane.scrollHeight - pane.scrollTop - pane.clientHeight).toBeLessThanOrEqual(1);

    /* At the end it stays at the end when narrowing makes the transcript taller. */
    await page.getByRole('button', { name: 'Restore width' }).click();
    expect(document.activeElement).toBe(toggle());
    expect(pane.scrollHeight - pane.scrollTop - pane.clientHeight).toBeLessThanOrEqual(1);

  });

  /** The box of one token of the long paragraph, wherever the paragraph has wrapped it. */
  function wordBox(word: string): DOMRect {
    const node = paragraphStarting('w1 ').firstChild!;
    const at = node.textContent!.indexOf(` ${word} `) + 1;
    const range = document.createRange();
    range.setStart(node, at);
    range.setEnd(node, at + word.length);
    return range.getBoundingClientRect();
  }
  /** The drawer reads what is under a line just inside the top of its pane, under the header; put `top` (a viewport y) on that line. */
  const READING_LINE_PX = 12;
  const offsetInPane = (top: number) => top - scroller().getBoundingClientRect().top;

  it('keeps the words read in the middle of a long paragraph where they were, both ways', async () => {
    await page.viewport(1400, 900);
    render(<WidthPage />);
    await click(opener());
    await settled();
    const pane = scroller();
    pane.scrollTop += offsetInPane(wordBox('w350').top) - READING_LINE_PX - 2;
    const lineHeight = Number.parseFloat(getComputedStyle(paragraphStarting('w1 ')).lineHeight) + 1;
    for (const name of ['Expand reading width', 'Restore width']) {
      const before = offsetInPane(wordBox('w350').top);
      expect(Math.abs(before - READING_LINE_PX)).toBeLessThanOrEqual(lineHeight);
      await page.getByRole('button', { name }).click();
      expect(document.activeElement).toBe(toggle());
      /* The same words stay on the reading line: at most a line of rewrap drift, never the paragraph's own top. */
      expect(Math.abs(offsetInPane(wordBox('w350').top) - before)).toBeLessThanOrEqual(lineHeight);
    }
  });

  it('keeps the next paragraph in place when the reading line falls in the gap between paragraphs, both ways', async () => {
    await page.viewport(1400, 900);
    render(<WidthPage />);
    await click(opener());
    await settled();
    const pane = scroller();
    /* Early in the reply, so the pane can still scroll far enough to hold it in place at the wider width. */
    const [above, below] = [paragraphStarting('Paragraph 4 '), paragraphStarting('Paragraph 5 ')];
    const gap = below.getBoundingClientRect().top - above.getBoundingClientRect().bottom;
    expect(gap).toBeGreaterThan(0);
    pane.scrollTop += offsetInPane(below.getBoundingClientRect().top) - READING_LINE_PX - gap / 2;
    for (const name of ['Expand reading width', 'Restore width']) {
      const before = offsetInPane(below.getBoundingClientRect().top);
      expect(before).toBeGreaterThan(READING_LINE_PX);
      await page.getByRole('button', { name }).click();
      expect(Math.abs(offsetInPane(below.getBoundingClientRect().top) - before)).toBeLessThanOrEqual(2);
    }
  });

  it('still closes on Escape and on Close while expanded, and returns focus to the opener', async () => {
    await page.viewport(1400, 900);
    const onClose = vi.fn();
    render(<WidthPage onClose={onClose} />);
    opener().focus();
    await click(opener());
    await page.getByRole('button', { name: 'Expand reading width' }).click();
    expect(document.activeElement).toBe(toggle());
    await act(async () => {
      toggle()!.dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape', bubbles: true, composed: true }));
      await Promise.resolve();
    });
    expect(onClose).toHaveBeenCalledTimes(1);
    await untilGone();
    await untilFocused(opener, 'the opener');

    await click(opener());
    expect(drawer().hasAttribute('data-nc-drawer-expanded')).toBe(true);
    expect(spanOf(drawer())).toBeCloseTo(0.7, 2);
    await click(drawer().querySelector<HTMLElement>('button[aria-label="Close conversation"]')!);
    await untilGone();
    await untilFocused(opener, 'the opener');
  });

  it.each([false, true])('scrolls neither the page nor the pane sideways at 1280 (expanded: %s)', async (expanded) => {
    await page.viewport(1280, 800);
    render(<WidthPage initiallyOpen />);
    if (expanded) await click(toggle()!);
    await settled();
    expect(drawer().hasAttribute('data-nc-drawer-expanded')).toBe(expanded);
    expectNoSidewaysPage();
  });

  it('has no toggle on a phone, where the conversation is already the full width', async () => {
    await page.viewport(390, 844);
    try {
      render(<WidthPage initiallyOpen />);
      await settled();
      expect(toggle()).toBeNull();
      expect(drawer().hasAttribute('data-nc-drawer-expanded')).toBe(false);
      expect(drawer().getBoundingClientRect().width).toBe(390);
      expectNoSidewaysPage();
    } finally { await page.viewport(1400, 900); }
  });

  it('ignores an expanded choice made on desktop once the viewport is a phone', async () => {
    await page.viewport(1400, 900);
    try {
      render(<WidthPage initiallyOpen />);
      await click(toggle()!);
      expect(drawer().hasAttribute('data-nc-drawer-expanded')).toBe(true);
      await page.viewport(390, 844);
      await waitFor(() => expect(toggle()).toBeNull());
      expect(drawer().hasAttribute('data-nc-drawer-expanded')).toBe(false);
      expect(drawer().getBoundingClientRect().width).toBe(390);
      expectNoSidewaysPage();
      await page.viewport(1400, 900);
      await waitFor(() => expect(toggle()?.getAttribute('aria-pressed')).toBe('true'));
      expect(spanOf(drawer())).toBeCloseTo(0.7, 2);
    } finally { await page.viewport(1400, 900); }
  });

  const LONG_TITLE = 'Why the resolver drops a hop when two routes share a cache key, and what the backfill has to repair afterwards';
  const heading = () => drawer().querySelector<HTMLElement>('h2')!;

  it('paints the conversation name in a one-line header that ellipsizes and keeps the full name reachable', async () => {
    await page.viewport(1400, 900);
    render(<WidthPage initiallyOpen title={LONG_TITLE} />);
    await settled();
    const title = heading();
    expect(title.textContent).toBe(LONG_TITLE);
    expect(title.checkVisibility()).toBe(true);
    expect(title.getAttribute('title')).toBe(LONG_TITLE);
    expect(title.scrollWidth).toBeGreaterThan(title.clientWidth);
    expect(title.getBoundingClientRect().height).toBeLessThanOrEqual(Number.parseFloat(getComputedStyle(title).lineHeight) + 1);
    const inset = () => title.getBoundingClientRect().left - drawer().getBoundingClientRect().left;
    const before = inset();
    expect(title.getBoundingClientRect().right).toBeLessThanOrEqual(toggle()!.getBoundingClientRect().left);
    await click(toggle()!);
    /* Wider card, same row: the title keeps its place in the card and never meets the controls. */
    expect(inset()).toBeCloseTo(before, 0);
    expect(title.getBoundingClientRect().right).toBeLessThanOrEqual(toggle()!.getBoundingClientRect().left);
    expect(document.documentElement.scrollWidth).toBeLessThanOrEqual(document.documentElement.clientWidth);
  });

  it('keeps the header controls clear of the first transcript line, at either width', async () => {
    await page.viewport(1400, 900);
    render(<WidthPage initiallyOpen />);
    await settled();
    for (const step of ['normal', 'expanded']) {
      if (step === 'expanded') await click(toggle()!);
      /* The transcript follows its newest turn on open; read its first line from the top. */
      scroller().scrollTop = 0;
      const controls = [toggle()!, drawer().querySelector<HTMLElement>('button[aria-label="Close conversation"]')!]
        .map((control) => control.getBoundingClientRect());
      const firstLine = paragraphStarting('Show me the plan.').getBoundingClientRect();
      for (const box of controls) {
        expect(box.bottom, step).toBeLessThanOrEqual(scroller().getBoundingClientRect().top);
        expect(box.bottom <= firstLine.top || box.right <= firstLine.left || box.left >= firstLine.right, step).toBe(true);
      }
    }
  });

  it('labels the region once, by the painted title', async () => {
    await page.viewport(1400, 900);
    render(<WidthPage initiallyOpen title="Planner chat" />);
    await settled();
    expect(drawer().hasAttribute('aria-label')).toBe(false);
    expect(drawer().getAttribute('aria-labelledby')).toBe(heading().id);
    await expect.element(page.getByRole('complementary', { name: 'Planner chat' })).toBeInTheDocument();
    expect(drawer().querySelectorAll('h1, h2, h3, h4, h5, h6')).toHaveLength(1);
    const named = [...drawer().querySelectorAll('*')].filter((element) => element.childElementCount === 0 && element.textContent === 'Planner chat');
    expect(named).toEqual([heading()]);
  });

  it('shows no desktop header on a phone, only the shared mobile header', async () => {
    await page.viewport(390, 844);
    try {
      render(<WidthPage initiallyOpen title="Planner chat" />);
      await settled();
      expect(drawer().hasAttribute('aria-labelledby')).toBe(false);
      const headings = drawer().querySelectorAll('h1, h2, h3, h4, h5, h6');
      expect(headings).toHaveLength(1);
      expect(headings[0].closest('[data-nc-mobile-header]')).not.toBeNull();
      expect(drawer().querySelector('button[aria-label="Close conversation"]')).toBeNull();
      expect(toggle()).toBeNull();
      expect(drawer().getAttribute('aria-label')).toBe('Planner chat');
      await expect.element(page.getByRole('button', { name: 'Back to previous page' })).toBeInTheDocument();
    } finally { await page.viewport(1400, 900); }
  });
});
