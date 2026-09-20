/* The reference appendix's heading, measured in a real browser: jsdom computes no layout. */
import { render } from '@testing-library/react';
import { page as browserPage } from 'vitest/browser';
import { afterEach, describe, expect, it } from 'vitest';

/* The whole cascade, before the CSS Module. */
import '../../../styles/entry.css';

import type { ReportBlock } from '../../../../../core/domain/report.ts';
import { ReportDocument } from './public.tsx';

afterEach(() => { document.body.replaceChildren(); });

const task = (id: string, key: string): ReportBlock => ({
  id,
  kind: 'task',
  payload: { key, kind: 'codex', declared_by: 'spec', ready: true, goal: `goal for ${key}` },
});

const prose = (id: string, markdown: string): ReportBlock => ({ id, kind: 'prose', payload: { markdown } });

/** The page publishes the two customs the document's grid is built from; without
 *  them the measure column falls back and the columns under test do not exist. */
function Page({ gutter = '160px' }: { gutter?: string }) {
  return (
    <div
      data-testid="frame"
      style={{
        inlineSize: 1200,
        overflow: 'clip',
        ['--document-start' as string]: gutter,
        ['--document-measure' as string]: '600px',
      }}
    >
      <ReportDocument
        report={{
          summary: '',
          body: '',
          blocks: [prose('b-1', '# Conclusion\n\nThe number is 606.'), task('b-2', 'alpha')],
        }}
        empty={<p>Nothing yet.</p>}
      />
    </div>
  );
}

describe('the reference heading, as the engine lays it out', () => {
  it('starts its word on the same column as the report\'s own section titles', async () => {
    await browserPage.viewport(1200, 800);
    render(<Page />);

    const reference = document.querySelector('[data-nc-report-reference]')!;
    const sectionHeads = [...document.querySelectorAll('[data-nc-report] h2')]
      .filter((head) => !reference.contains(head));
    expect(sectionHeads.length).toBeGreaterThan(0);

    const referenceWord = reference.querySelector('h2 > span:nth-child(2)')!;
    const wordLeft = Math.round(referenceWord.getBoundingClientRect().left);
    for (const head of sectionHeads) {
      expect(Math.round(head.getBoundingClientRect().left)).toBe(wordLeft);
    }
  });

  it('stays inside the page when the gutter collapses to nothing', async () => {
    await browserPage.viewport(1200, 800);
    render(<Page gutter="0px" />);

    const frame = document.querySelector('[data-testid="frame"]')!.getBoundingClientRect();
    const marker = document
      .querySelector('[data-nc-report-reference] h2 > span:first-child')!
      .getBoundingClientRect();

    expect(marker.width).toBeGreaterThanOrEqual(8);
    expect(marker.left).toBeGreaterThanOrEqual(frame.left);
  });

  it('keeps the word aligned at a gutter too narrow for the full pull', async () => {
    await browserPage.viewport(1200, 800);
    render(<Page gutter="28px" />);

    const reference = document.querySelector('[data-nc-report-reference]')!;
    const sectionHead = [...document.querySelectorAll('[data-nc-report] h2')]
      .find((head) => !reference.contains(head))!;
    const word = reference.querySelector('h2 > span:nth-child(2)')!;

    expect(Math.round(word.getBoundingClientRect().left))
      .toBe(Math.round(sectionHead.getBoundingClientRect().left));
  });

  it('paints the chevron, in the margin rather than in the measure', async () => {
    await browserPage.viewport(1200, 800);
    render(<Page />);

    const reference = document.querySelector('[data-nc-report-reference]')!;
    const marker = reference.querySelector('h2 > span:first-child')!;
    const box = marker.getBoundingClientRect();
    const word = reference.querySelector('h2 > span:nth-child(2)')!.getBoundingClientRect();

    /* Not `> 0`: a hairline-wide glyph is the same defect arriving smaller. */
    expect(box.width).toBeGreaterThanOrEqual(8);
    expect(box.height).toBeGreaterThanOrEqual(8);
    const svg = marker.querySelector('svg')!.getBoundingClientRect();
    expect(svg.width).toBeGreaterThanOrEqual(8);

    expect(box.right).toBeLessThan(word.left);

    const probe = document.createElement('div');
    probe.style.inlineSize = 'var(--space-10)';
    document.body.append(probe);
    const gutterGap = probe.getBoundingClientRect().width;
    probe.remove();

    const sectionHead = [...document.querySelectorAll('[data-nc-report] h2')]
      .find((head) => !reference.contains(head))!;
    const numberColumnRight = sectionHead.getBoundingClientRect().left - gutterGap;
    expect(Math.round(box.right)).toBe(Math.round(numberColumnRight));
  });
});
