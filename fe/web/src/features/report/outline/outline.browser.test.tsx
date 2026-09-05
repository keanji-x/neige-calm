import { fireEvent, render, waitFor } from '@testing-library/react';
import { page } from 'vitest/browser';
import { afterEach, expect, it } from 'vitest';

import '../../../styles/entry.css';
import type { ReportOutlineItem } from '../../../../../core/domain/report.ts';
import documentStyles from '../document/document.module.css';
import { ReportOutline } from './public.tsx';

afterEach(() => { document.body.replaceChildren(); delete document.documentElement.dataset.theme; });

const ITEMS: ReportOutlineItem[] = [
  { blockId: 'one', label: 'First section', number: 1, children: [{ blockId: 'child', label: 'Child block' }] },
  { blockId: 'two', label: 'Second section', number: 2, children: [] },
];

const MANY_ITEMS: ReportOutlineItem[] = Array.from({ length: 100 }, (_, index) => ({
  blockId: `section-${index + 1}`,
  label: `Section ${index + 1}`,
  number: index + 1,
  children: [],
}));

it('centres a dense first-level rail beside the report edge and magnifies the aimed dot', async () => {
  await page.viewport(1400, 900);
  render(
    <div style={{ containerType: 'inline-size', inlineSize: 1200 }}>
      <div className={documentStyles.doc} style={{
        position: 'relative', inlineSize: 900, blockSize: 600,
        ['--document-start' as string]: '130px',
        ['--header-band' as string]: '0px', ['--header-h' as string]: '0px',
      }}>
        <span data-testid="report-edge" style={{ position: 'absolute', insetInlineStart: 130 }} />
        <ReportOutline items={ITEMS} />
        <div className={documentStyles.row}>
          <div className={documentStyles.block}>
            <h2 className={documentStyles.h1}>First section</h2>
          </div>
        </div>
      </div>
    </div>,
  );
  const rail = document.querySelector<HTMLElement>('nav[aria-label="Outline"]')!;
  const list = rail.querySelector<HTMLElement>('ol')!;
  const edge = document.querySelector<HTMLElement>('[data-testid="report-edge"]')!;
  const rows = [...document.querySelectorAll<HTMLElement>('nav[aria-label="Outline"] button')];
  expect(rows).toHaveLength(2);
  expect(edge.getBoundingClientRect().left - rail.getBoundingClientRect().right).toBeCloseTo(4, 0);
  expect(list.getBoundingClientRect().top + list.getBoundingClientRect().height / 2)
    .toBeCloseTo(window.innerHeight / 2, 0);
  expect(rows[0]?.getBoundingClientRect().height).toBeCloseTo(24, 0);
  expect(rows[1].getBoundingClientRect().top - rows[0].getBoundingClientRect().bottom).toBeCloseTo(0, 0);
  const heading = document.querySelector<HTMLElement>('h2')!;
  expect(getComputedStyle(heading, '::before').opacity).toBe('0');

  const first = rows[0];
  const dot = first.querySelector<HTMLElement>('span:first-child')!;
  const restingDot = dot.getBoundingClientRect().width;
  expect(restingDot).toBeCloseTo(4, 0);
  expect(document.querySelector('[data-nc-outline-preview]')).toBeNull();
  await page.getByRole('button', { name: 'First section' }).hover();
  await waitFor(() => {
    expect(document.querySelector('[data-nc-outline-preview]')).not.toBeNull();
    expect(dot.getBoundingClientRect().width).toBeCloseTo(8, 0);
  });
  const preview = document.querySelector<HTMLElement>('[data-nc-outline-preview]')!;
  expect(getComputedStyle(heading, '::before').opacity).toBe('0');
  expect(first.getBoundingClientRect().height).toBeCloseTo(28, 0);
  expect(dot.getBoundingClientRect().width).toBeCloseTo(8, 0);
  expect(rows[1].getBoundingClientRect().top + rows[1].getBoundingClientRect().height / 2
    - (first.getBoundingClientRect().top + first.getBoundingClientRect().height / 2)).toBeGreaterThanOrEqual(24);
  expect(preview.getBoundingClientRect().right).toBeLessThanOrEqual(rail.getBoundingClientRect().left - 3);
});

it('uses the same quiet, non-text contrast ink as the Conversation rail', async () => {
  await page.viewport(1400, 900);
  render(<ReportOutline items={ITEMS} />);
  const dot = document.querySelector<HTMLElement>('nav[aria-label="Outline"] button span:first-child')!;
  const expected = document.createElement('span');
  expected.style.background = 'oklch(58% 0.01 250)';
  const rejected = document.createElement('span');
  rejected.style.background = 'var(--text-4)';
  document.body.append(expected, rejected);
  expect(getComputedStyle(dot).backgroundColor).toBe(getComputedStyle(expected).backgroundColor);
  expect(getComputedStyle(dot).backgroundColor).not.toBe(getComputedStyle(rejected).backgroundColor);

  document.documentElement.dataset.theme = 'dark';
  expected.style.background = 'oklch(56% 0.012 245)';
  await waitFor(() => {
    expect(getComputedStyle(dot).backgroundColor).toBe(getComputedStyle(expected).backgroundColor);
  });
});

it('keeps a long outline viewport-bounded and scrolls the roving end into view', async () => {
  await page.viewport(1400, 900);
  render(
    <div className={documentStyles.doc} style={{
      position: 'relative', inlineSize: 900, blockSize: 2600,
      ['--document-start' as string]: '130px',
      ['--header-band' as string]: '0px', ['--header-h' as string]: '0px',
    }}>
      <ReportOutline items={MANY_ITEMS} />
    </div>,
  );
  const track = document.querySelector<HTMLElement>('[data-nc-outline-track]')!;
  const rows = [...document.querySelectorAll<HTMLElement>('nav[aria-label="Outline"] button')];
  expect(track.getBoundingClientRect().height).toBeLessThanOrEqual(320);
  expect(rows[0].getBoundingClientRect().top).toBeGreaterThanOrEqual(track.getBoundingClientRect().top);

  rows[0].focus();
  fireEvent.keyDown(rows[0], { key: 'End' });
  const last = rows.at(-1)!;
  await waitFor(() => {
    expect(track.scrollTop).toBeGreaterThan(0);
    expect(last.getBoundingClientRect().top).toBeGreaterThanOrEqual(track.getBoundingClientRect().top);
    expect(last.getBoundingClientRect().bottom).toBeLessThanOrEqual(track.getBoundingClientRect().bottom + 1);
  });
});

it('keeps section numbers when the desktop outline is hidden on a compact viewport', async () => {
  await page.viewport(900, 700);
  render(
    <div style={{ containerType: 'inline-size', inlineSize: 700 }}>
      <div className={documentStyles.doc} style={{
        position: 'relative', inlineSize: 700, blockSize: 600,
        ['--document-start' as string]: '100px',
        ['--header-band' as string]: '0px', ['--header-h' as string]: '0px',
      }}>
        <ReportOutline items={ITEMS} />
        <div className={documentStyles.row}>
          <div className={documentStyles.block}>
            <h2 className={documentStyles.h1}>First section</h2>
          </div>
        </div>
      </div>
    </div>,
  );
  const rail = document.querySelector<HTMLElement>('nav[aria-label="Outline"]')!;
  const heading = document.querySelector<HTMLElement>('h2')!;
  expect(getComputedStyle(rail).display).toBe('none');
  expect(getComputedStyle(heading, '::before').opacity).toBe('1');
});
