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
   * **And it survives the gutter collapsing**, which is the case that broke it.
   *
   * `--document-start` is a `max(0px, …)` of whatever is left over, so below
   * about 1100px it is `0px`. With the marker absolutely positioned one gutter
   * to the left of the measure, that put it outside `.main` — which is
   * `overflow: clip` — and the heading lost the only thing saying it opens.
   * Measured at 900px before the fix: marker left edge 30, main region starting
   * at 44.
   *
   * The frame here is `overflow: clip` for the same reason `.main` is, so a
   * marker that walks off the edge is genuinely gone rather than merely
   * negative.
   */
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

  /*
   * **The intermediate gutter**, which is the regime a constant gap got wrong.
   *
   * The pull is clamped to the gutter, so at 28px it survives at 28 while the
   * trailing gap, written as a fixed `--space-10 − --space-7`, stayed 24 — and
   * the word landed 10px right of every section title. Alignment is the thing
   * this rule exists for, so it is the last thing allowed to go: it has to hold
   * at every gutter wide enough for the glyph and its row gap, not only at a
   * full one.
   */
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

    /*
     * **And on the section numbers' own column**, which is the claim the
     * stylesheet makes and nothing checked. `.h1::before` hangs at
     * `inset-inline-end: calc(100% + --space-10)` — its trailing edge one
     * `--space-10` before the measure — so the chevron's trailing edge has to
     * land there too, or the margin holds two things ten pixels apart while the
     * comment says "one column". Measured against a probe carrying the token
     * rather than the number, so changing `--space-10` moves both.
     */
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
