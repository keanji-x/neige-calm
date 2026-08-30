/*
 * The reference appendix's heading, measured.
 *
 * Two claims, and jsdom can make neither: it computes no layout, so every box
 * it reports is zeroes and every left edge is identical to every other one.
 *
 *   1. `Reference` starts on the same column as the report's own section
 *      titles. It did not: the head was a small sans row whose chevron sat
 *      where the *prose* begins, so the word started one glyph further in than
 *      every heading above it and the document had two left edges.
 *
 *   2. The chevron in the margin has a painted box. It did not, and that is
 *      the reason this file exists rather than a unit test: measured at
 *      **0 × 14** — present in the DOM, `opacity: 1`, the right colour, and
 *      nought pixels wide, because an absolutely positioned box with only an
 *      inline-end inset gets a negative available width, clamps to zero, and
 *      a flex container at zero width shrinks its item to match. Every
 *      assertion a unit test can make about that markup was green.
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

const task = (id: string, key: string): ReportBlock => ({
  id,
  kind: 'task',
  payload: { key, kind: 'codex', declared_by: 'spec', ready: true, goal: `goal for ${key}` },
});

const prose = (id: string, markdown: string): ReportBlock => ({ id, kind: 'prose', payload: { markdown } });

/** The page publishes the two customs the document's grid is built from; without
 *  them the measure column falls back and the columns under test do not exist. */
function Page() {
  return (
    <div
      style={{
        inlineSize: 1200,
        ['--document-start' as string]: '160px',
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
    /* The premise: there is a section to line up with. Without it the equality
       below would hold vacuously over an empty list. */
    expect(sectionHeads.length).toBeGreaterThan(0);

    const referenceWord = reference.querySelector('h2 > span:nth-child(2)')!;
    const wordLeft = Math.round(referenceWord.getBoundingClientRect().left);
    for (const head of sectionHeads) {
      expect(Math.round(head.getBoundingClientRect().left)).toBe(wordLeft);
    }
  });

  /*
   * The chevron is *outside* the measure, in the margin the section numbers
   * hang in — which is the whole reason the word above can start on the text
   * edge. Both halves are asserted: a box with real pixels, and a position
   * clear of the column it was moved out of.
   */
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
    /* And the svg inside it, which is what actually shrank to nothing. */
    const svg = marker.querySelector('svg')!.getBoundingClientRect();
    expect(svg.width).toBeGreaterThanOrEqual(8);

    /* Clear of the text column, with the gutter between them. */
    expect(box.right).toBeLessThan(word.left);
  });
});
