/*
 * A document that carries its own maintenance contract, measured (#1185).
 *
 * A report may keep its policy — which sections it has, how to write them — in
 * a leading HTML comment: dropped wherever the document is rendered, readable
 * to everything that reads the body source. `sanitizeAstPolicy` already removes
 * the node, so jsdom can prove the *text* never appears (see `public.test.tsx`).
 *
 * What jsdom cannot prove is the part a reader actually sees. The block is a
 * grid item in a `row-gap` grid, so an emptied block still holds a row open and
 * every report opens with a band of blank space. That is a layout claim, and
 * jsdom computes no layout: `display`, `row-gap` and every box it reports are
 * inert. Hence this file.
 *
 * Note for whoever changes `ProseBlock`: returning `null` for an empty AST and
 * returning an empty fragment produce *the same DOM* — React emits no children
 * either way, so that early return carries nothing and deleting it leaves every
 * assertion here green. The load-bearing production change on this front end is
 * the `.row:has(> .block:empty)` rule in `document.module.css`, and this is the
 * test that covers it.
 *
 * The rule is on the ROW because the row is `display: contents`: the block and
 * its backlink sidenote are siblings in the same grid, so hiding only the block
 * leaves a lone `◂ N` in the gutter beside nothing. The third case below is
 * that one.
 */
import { render } from '@testing-library/react';
import { page as browserPage } from 'vitest/browser';
import { afterEach, describe, expect, it } from 'vitest';

/* The whole cascade, before the CSS Module — see the import-order note in
   `features/chat/thread/thread.browser.test.tsx`. */
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
    // The row is what carries `display: none`; the block's own computed
    // `display` stays `block` inside a hidden subtree, which is why this asks
    // the row and then asks the engine whether the block renders at all.
    expect(getComputedStyle(block.parentElement!).display).toBe('none');
    expect(block.checkVisibility()).toBe(false);
    expect(block.getBoundingClientRect().height).toBe(0);
  });

  it('does not push the first section down by a row gap', async () => {
    await browserPage.viewport(1200, 800);

    render(<Page blocks={[prose('b_1', CONTRACT), prose('b_2', SECTION)]} />);
    const withContract = topInFrame(document.querySelector('#b_2')!);
    document.body.replaceChildren();

    // The control: the same document without the contract. The reader must not
    // be able to tell the two apart, and before the `:empty` rule they differed
    // by one `row-gap` (`--space-8`).
    render(<Page blocks={[prose('b_2', SECTION)]} />);
    const withoutContract = topInFrame(document.querySelector('#b_2')!);

    expect(withContract).toBe(withoutContract);
  });

  it('takes its backlink sidenote with it, and still costs the next section nothing', async () => {
    /*
     * The orphan-marker case. A contract block is citable — it keeps a stable
     * id in `cove.outline` — so another report can link straight at it and the
     * slot grows a `◂ N` sidenote in column 3. `.row` is `display: contents`,
     * so that sidenote is a grid item in its own right: a rule that hid only
     * the block would leave the marker alone on an otherwise blank row.
     */
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

    // The same control as above: a cited contract block must be as invisible as
    // an uncited one, including in where it leaves the next section.
    render(<Page blocks={[prose('b_2', SECTION)]} />);
    expect(withContract).toBe(topInFrame(document.querySelector('#b_2')!));
  });
});
