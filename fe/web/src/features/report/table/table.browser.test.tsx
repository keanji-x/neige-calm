/*
 * A table cell's citation, as the engine lays it out (#1687).
 *
 * jsdom answers whether the cell became a button; it cannot answer whether
 * that button is a painted thing a reader can hit, nor whether the panel it
 * opens scrolls the quote into its own scroller. Those are the report's
 * promise: the 「来源」 column is the same citation as the prose, and clicking
 * it lands on the highlighted sentence — the way `source.browser.test.tsx`
 * proves it for the prose.
 */
import { act, render } from '@testing-library/react';
import { page } from 'vitest/browser';
import { afterEach, describe, expect, it } from 'vitest';

/* The whole cascade, before the CSS Module — see the import-order note in
   `features/chat/thread/thread.browser.test.tsx`. */
import '../../../styles/entry.css';

import type { ReportSourceLinkTarget, TrackSourceDetail } from '../../../../../core/domain/report-source.ts';
import { Drawer } from '../../../ui/drawer/public.tsx';
import { useState } from '../../../ui/state/public.ts';
import { ReportSourcePanel } from '../source/public.tsx';
import { ReportTableBlock } from './public.tsx';

afterEach(() => { document.body.replaceChildren(); });

const FILLER = Array.from({ length: 80 }, (_, index) => `第 ${index + 1} 段。市场在观望，数据在变化，结论暂时不动。`).join('\n\n');
const QUOTE = '布伦特原油收于每桶 67.4 美元';
const BODY = `${FILLER}\n\n据美联社，${QUOTE}。\n\n${FILLER}`;

const SOURCE: TrackSourceDetail = {
  source_id: 'src_ddef99cc',
  provenance: 'web_page',
  origin: { kind: 'plugin', plugin_id: 'mcp-wisburg', tool: 'get_article_detail', args_sha256: 'ab', args_canon: 'v1', content_id: '752980' },
  title: 'AP：OPEC+ 增产后油价走势',
  published_at: '2026-09-14',
  content_id: '752980',
  body_bytes: BODY.length,
  body_sha256: 'cd',
  captured_at: '2026-09-15T02:00:00Z',
  quotes: [{ id: 'q1', text: QUOTE, start: 0, end: 0 }],
  body: BODY,
};

const PAYLOAD = {
  caption: '关键数据',
  columns: [
    { key: 'metric', label: '指标' },
    { key: 'value', label: '数值', align: 'right' as const },
    { key: 'source', label: '来源' },
  ],
  rows: [
    { metric: '布伦特收盘', value: '67.4', source: '[AP](neige://source/src_ddef99cc#q1)' },
    { metric: 'OPEC+ 增产', value: '13.7 万桶/日', source: '见 [AP](neige://source/src_ddef99cc#q1) 收盘' },
  ],
};

/**
 * A main region for the drawer to be positioned in, and the same wiring the
 * app gives the document: the cell's handler sets the target, the target
 * opens the drawer with the panel. Spelled inline because a feature test may
 * not import `app/**` — the same fence the feature itself is behind.
 */
function Page() {
  const [target, setTarget] = useState<ReportSourceLinkTarget | null>(null);
  return (
    <main style={{ position: 'relative', blockSize: 700, overflow: 'clip', containerType: 'inline-size' }}>
      <h1 data-nc-page-title="" tabIndex={-1}>Brent</h1>
      <ReportTableBlock payload={PAYLOAD} onOpenSourceLink={setTarget} />
      <Drawer open={target !== null} title={SOURCE.title} closeLabel="关闭来源" onClose={() => { setTarget(null); }}>
        {target !== null && (
          <ReportSourcePanel target={target} resolution={{ status: 'ok', source: SOURCE }} onRetry={() => undefined} />
        )}
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

describe('a source citation in a table cell, as the engine lays it out', () => {
  it('is a painted control in its cell, and opens the panel with the quote highlighted', async () => {
    await page.viewport(1400, 900);
    render(<Page />);

    const cell = document.querySelector<HTMLElement>('tbody tr:nth-child(1) td:nth-child(3)')!;
    const citation = cell.querySelector<HTMLElement>('button[data-nc-report-source-link]')!;
    expect(citation.textContent).toBe('AP');
    /* Painted inside its own cell, not merely present: a control with no box
       is one nobody can click. */
    const box = citation.getBoundingClientRect();
    expect(box.width).toBeGreaterThan(8);
    expect(box.height).toBeGreaterThan(8);
    expect(inside(box, cell.getBoundingClientRect())).toBe(true);
    /* The row beneath holds the same link with prose around it, and stays text. */
    const mixed = document.querySelector<HTMLElement>('tbody tr:nth-child(2) td:nth-child(3)')!;
    expect(mixed.querySelector('button, a')).toBeNull();
    expect(mixed.textContent).toBe('见 [AP](neige://source/src_ddef99cc#q1) 收盘');

    await click(citation);

    const drawer = document.querySelector<HTMLElement>('[data-nc-drawer]')!;
    const scroller = drawer.querySelector<HTMLElement>('[data-nc-drawer-scroll]')!;
    const mark = drawer.querySelector<HTMLElement>('mark[data-nc-report-source-quote="q1"]')!;
    expect(mark.textContent).toBe(QUOTE);
    /* The premise: the quote is far enough down that nothing short of a
       scroll shows it. */
    expect(scroller.scrollHeight).toBeGreaterThan(scroller.clientHeight * 2);
    expect(scroller.scrollTop).toBeGreaterThan(0);
    expect(inside(mark.getBoundingClientRect(), scroller.getBoundingClientRect())).toBe(true);
    expect(window.scrollY).toBe(0);
  });
});
