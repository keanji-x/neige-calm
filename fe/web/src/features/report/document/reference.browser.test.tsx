/* The reference appendix's heading, measured in a real browser: jsdom computes no layout. */
import { cleanup, render, waitFor } from '@testing-library/react';
import { page as browserPage } from 'vitest/browser';
import { afterEach, describe, expect, it } from 'vitest';

/* The whole cascade, before the CSS Module. */
import '../../../styles/entry.css';

import type { ReportBlock } from '../../../../../core/domain/report.ts';
import { ReportDocument } from './public.tsx';
import { revealReportAnchor } from '../anchor/public.ts';

afterEach(() => { cleanup(); document.body.replaceChildren(); });

const task = (id: string, key: string): ReportBlock => ({
  id,
  kind: 'task',
  payload: { key, kind: 'codex', declared_by: 'spec', ready: true, goal: `goal for ${key}` },
});

const prose = (id: string, markdown: string): ReportBlock => ({ id, kind: 'prose', payload: { markdown } });

it('admits a cold task destination before scrolling to its real browser geometry', async () => {
  render(<>
    <button onClick={() => revealReportAnchor('cold-task')}>Jump to deferred task</button>
    <ReportDocument empty={null} report={{ summary: '', body: '', blocks: [
      prose('intro', '# Document\n\n' + 'A visible paragraph before the task.\n\n'.repeat(80)),
      task('cold-task', 'cold target'),
    ] }} />
  </>);
  expect(document.querySelector('[data-nc-task-state]')).toBeNull();
  await browserPage.getByRole('button', { name: 'Jump to deferred task' }).click();
  await waitFor(() => {
    expect(document.querySelector<HTMLDetailsElement>('[data-nc-report-reference]')?.open).toBe(true);
    expect(document.querySelector<HTMLDetailsElement>('[data-nc-task-state]')?.open).toBe(true);
    expect(document.getElementById('cold-task')?.textContent).toContain('goal for cold target');
  });
  const box = document.getElementById('cold-task')!.getBoundingClientRect();
  expect(box.bottom).toBeGreaterThan(0);
  expect(box.top).toBeLessThan(window.innerHeight);
});

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
    // A 14px marker plus the 16px row gap still fits below the full 38px pull.
    render(<Page gutter="30px" />);

    const reference = document.querySelector('[data-nc-report-reference]')!;
    const sectionHead = [...document.querySelectorAll('[data-nc-report] h2')]
      .find((head) => !reference.contains(head))!;
    const word = reference.querySelector('h2 > span:nth-child(2)')!;
    const marker = reference.querySelector('h2 > span:first-child')!.getBoundingClientRect();
    const frame = document.querySelector('[data-testid="frame"]')!.getBoundingClientRect();
    expect(Math.round(marker.left)).toBe(Math.round(frame.left));

    expect(Math.round(word.getBoundingClientRect().left))
      .toBe(Math.round(sectionHead.getBoundingClientRect().left));
  });

  it('paints the chevron, in the margin rather than in the measure', async () => {
    await browserPage.viewport(1200, 800);
    render(<Page />);

    const reference = document.querySelector('[data-nc-report-reference]')!;
    const marker = reference.querySelector('h2 > span:first-child')!;
    expect(marker.hasAttribute('data-nc-spring-rotation')).toBe(true);
    expect(getComputedStyle(marker).transitionProperty).toBe('none');
    const box = marker.getBoundingClientRect();
    const word = reference.querySelector('h2 > span:nth-child(2)')!.getBoundingClientRect();

    /* Not `> 0`: a hairline-wide glyph is the same defect arriving smaller. */
    expect(box.width).toBeGreaterThanOrEqual(8);
    expect(box.height).toBeGreaterThanOrEqual(8);
    const svg = marker.querySelector('svg')!.getBoundingClientRect();
    expect(svg.width).toBeGreaterThanOrEqual(8);

    expect(box.right).toBeLessThan(word.left);

    const probe = document.createElement('div');
    probe.style.inlineSize = 'var(--space-5)';
    document.body.append(probe);
    const gutterGap = probe.getBoundingClientRect().width;
    probe.remove();

    const sectionHead = [...document.querySelectorAll('[data-nc-report] h2')]
      .find((head) => !reference.contains(head))!;
    const numberColumnRight = sectionHead.getBoundingClientRect().left - gutterGap;
    expect(Math.round(box.right)).toBe(Math.round(numberColumnRight));
  });
});
