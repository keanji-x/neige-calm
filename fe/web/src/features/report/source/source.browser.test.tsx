/* The source panel's claims that only a rendering engine can answer: jsdom computes no layout. */
import { act, render } from '@testing-library/react';
import { page } from 'vitest/browser';
import { afterEach, describe, expect, it } from 'vitest';

/* The whole cascade, before the CSS Module. */
import '../../../styles/entry.css';

import type { ReportSourceLinkTarget, TrackSourceDetail } from '../../../../../core/domain/report-source.ts';
import { Drawer } from '../../../ui/drawer/public.tsx';
import { useState } from '../../../ui/state/public.ts';
import { ReportSourcePanel } from './public.tsx';

afterEach(() => { document.body.replaceChildren(); });

const FILLER = Array.from({ length: 80 }, (_, index) => `第 ${index + 1} 段。市场在观望，数据在变化，结论暂时不动。`).join('\n\n');
const QUOTE = '9月加息概率接近九成';
const LONG_LINE = `https://example.com/${'a'.repeat(300)}`;
const BODY = `${FILLER}\n\n央行表示，${QUOTE}。\n\n${LONG_LINE}\n\n${FILLER}`;

const SOURCE: TrackSourceDetail = {
  source_id: 'src_2c9e0a1b',
  provenance: 'full_text',
  origin: { kind: 'plugin', plugin_id: 'mcp-wisburg', tool: 'get_article_detail', args_sha256: 'ab', args_canon: 'v1', content_id: '752972' },
  title: 'Mikko 全球市场日志 9-13',
  published_at: '2026-09-13',
  content_id: '752972',
  body_bytes: BODY.length,
  body_sha256: 'cd',
  captured_at: '2026-09-14T08:00:00Z',
  quotes: [{ id: 'q1', text: QUOTE, start: 0, end: 0 }],
  body: BODY,
};

const TARGET: ReportSourceLinkTarget = { destination: 'neige://source/src_2c9e0a1b#q1', sourceId: 'src_2c9e0a1b', quoteId: 'q1' };

/** A positioned, clipped, fixed-height main region for the drawer, spelled inline because a feature test may not import `app/**`. */
function Page() {
  const [open, setOpen] = useState(false);
  return (
    <main style={{ position: 'relative', blockSize: 700, overflow: 'clip', containerType: 'inline-size' }}>
      <h1 data-nc-page-title="" tabIndex={-1}>Rates</h1>
      <button type="button" data-testid="opener" onClick={() => { setOpen(true); }}>Mikko 日志</button>
      <Drawer open={open} title={SOURCE.title} closeLabel="关闭来源" onClose={() => { setOpen(false); }}>
        {open && <ReportSourcePanel target={TARGET} resolution={{ status: 'ok', source: SOURCE }} onRetry={() => undefined} />}
      </Drawer>
    </main>
  );
}

async function click(element: HTMLElement) {
  await act(async () => { element.click(); await Promise.resolve(); });
}

function inside(box: DOMRect, frame: DOMRect): boolean {
  return box.top >= frame.top && box.bottom <= frame.bottom && box.left >= frame.left && box.right <= frame.right;
}

describe('the source panel, as the engine lays it out', () => {
  it('opens in the drawer and scrolls the quote into the drawer\'s own scroller', async () => {
    await page.viewport(1400, 900);
    render(<Page />);
    await click(document.querySelector<HTMLElement>('[data-testid="opener"]')!);

    const drawer = document.querySelector<HTMLElement>('[data-nc-drawer]')!;
    const scroller = drawer.querySelector<HTMLElement>('[data-nc-drawer-scroll]')!;
    const mark = drawer.querySelector<HTMLElement>('mark[data-nc-report-source-quote="q1"]')!;
    expect(mark.textContent).toBe(QUOTE);

    expect(scroller.scrollHeight).toBeGreaterThan(scroller.clientHeight * 2);
    expect(scroller.scrollTop).toBeGreaterThan(0);

    expect(inside(mark.getBoundingClientRect(), scroller.getBoundingClientRect())).toBe(true);
    expect(window.scrollY).toBe(0);
    const markBox = mark.getBoundingClientRect();
    expect(markBox.width).toBeGreaterThan(8);
    expect(markBox.height).toBeGreaterThan(8);
  });

  it('folds a long line inside the card instead of scrolling it sideways', async () => {
    await page.viewport(1400, 900);
    render(<Page />);
    await click(document.querySelector<HTMLElement>('[data-testid="opener"]')!);

    const drawer = document.querySelector<HTMLElement>('[data-nc-drawer]')!;
    const body = drawer.querySelector<HTMLElement>('pre[data-nc-report-source-body]')!;
    expect(body.scrollWidth).toBe(body.clientWidth);
    expect(body.getBoundingClientRect().right).toBeLessThanOrEqual(drawer.getBoundingClientRect().right);
    const badge = drawer.querySelector<HTMLElement>('[data-nc-report-source-provenance="full_text"]')!;
    const title = drawer.querySelector<HTMLElement>('h2')!;
    expect(badge.textContent).toBe('智堡全文');
    expect(badge.getBoundingClientRect().bottom).toBeLessThanOrEqual(title.getBoundingClientRect().top + 1);
  });
});
