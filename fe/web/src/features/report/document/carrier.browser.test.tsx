/* A document carrying its maintenance contract in a leading HTML comment, measured: an emptied block is still a grid item holding a `row-gap` row open. The load-bearing rule is `.row:has(> .block:empty)` in `document.module.css`; the row is `display: contents`, so the backlink sidenote is a sibling grid item. */
import { render } from '@testing-library/react';
import { page as browserPage } from 'vitest/browser';
import { afterEach, describe, expect, it } from 'vitest';

/* The whole cascade before the CSS Module: layer order is first-come. */
import '../../../styles/entry.css';

import type { ReportBlock } from '../../../../../core/domain/report.ts';
import { ReportDocument } from './public.tsx';

afterEach(() => { document.body.replaceChildren(); });

const prose = (id: string, markdown: string): ReportBlock => ({ id, kind: 'prose', payload: { markdown } });

/** Multi-line on purpose: a CommonMark HTML block of type 2 is not terminated
 *  by a blank line, which is exactly the property the carrier relies on. */
const CONTRACT = [
  '<!-- 报告维护契约（渲染时被丢弃，读 body 源码的主体看得到）',
  '',
  '这份报告自带的结构就是规则：维护它，不要重写它。',
  '',
  '写作方式：散文正文控制在 1000 字以内。',
  '-->',
  '',
].join('\n');

const SECTION = '# 概要\n\n本轮结论。\n';

function Page({ blocks, backlinkCounts }: {
  blocks: ReportBlock[];
  backlinkCounts?: ReadonlyMap<string, number>;
}) {
  return (
    <div
      data-testid="frame"
      style={{
        inlineSize: 1200,
        ['--document-start' as string]: '160px',
        ['--document-measure' as string]: '600px',
      }}
    >
      <ReportDocument
        report={{ summary: '', body: '', blocks }}
        backlinkCounts={backlinkCounts}
        empty={<p>Nothing yet.</p>}
      />
    </div>
  );
}

/** The element's top, measured from the frame — the frame moves between
 *  renders, so an absolute top would compare two different origins. */
function topInFrame(element: Element): number {
  const frame = document.querySelector('[data-testid="frame"]')!.getBoundingClientRect();
  return Math.round(element.getBoundingClientRect().top - frame.top);
}

describe('a contract block takes no room', () => {
  it('is laid out as nothing at all', async () => {
    await browserPage.viewport(1200, 800);
    render(<Page blocks={[prose('b_1', CONTRACT), prose('b_2', SECTION)]} />);

    const block = document.querySelector('#b_1') as HTMLElement;
    expect(block.childNodes.length).toBe(0);
    // The row carries `display: none`; the block's own computed `display` stays `block` inside a hidden subtree.
    expect(getComputedStyle(block.parentElement!).display).toBe('none');
    expect(block.checkVisibility()).toBe(false);
    expect(block.getBoundingClientRect().height).toBe(0);
  });

  it('does not push the first section down by a row gap', async () => {
    await browserPage.viewport(1200, 800);

    render(<Page blocks={[prose('b_1', CONTRACT), prose('b_2', SECTION)]} />);
    const withContract = topInFrame(document.querySelector('#b_2')!);
    document.body.replaceChildren();

    // The control: the same document without the contract.
    render(<Page blocks={[prose('b_2', SECTION)]} />);
    const withoutContract = topInFrame(document.querySelector('#b_2')!);

    expect(withContract).toBe(withoutContract);
  });

  it('takes its backlink sidenote with it, and still costs the next section nothing', async () => {
    /* A contract block is citable, so the slot can grow a `◂ N` sidenote in column 3 — a grid item in its own right under `display: contents`. */
    await browserPage.viewport(1200, 800);
    const cited = new Map([['b_1', 3]]);

    render(<Page blocks={[prose('b_1', CONTRACT), prose('b_2', SECTION)]} backlinkCounts={cited} />);
    const sidenote = [...document.querySelectorAll('span')]
      .find((span) => span.textContent?.includes('◂'));
    expect(sidenote, 'the sidenote is rendered — this test is about hiding it, not about it being absent').toBeDefined();
    expect(sidenote!.checkVisibility()).toBe(false);
    expect(sidenote!.getBoundingClientRect().height).toBe(0);
    const withContract = topInFrame(document.querySelector('#b_2')!);
    document.body.replaceChildren();

    render(<Page blocks={[prose('b_2', SECTION)]} />);
    expect(withContract).toBe(topInFrame(document.querySelector('#b_2')!));
  });
});
