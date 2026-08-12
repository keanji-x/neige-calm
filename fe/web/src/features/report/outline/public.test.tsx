// @vitest-environment jsdom
import { cleanup, render, screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, describe, expect, it, vi } from 'vitest';

import type { ReportOutlineItem } from '../../../../../core/domain/report.ts';
import { ReportOutline } from './public.tsx';

afterEach(cleanup);

const ITEMS: ReportOutlineItem[] = [
  {
    blockId: 'b-1-h1',
    label: 'Valuation conclusion',
    number: 1,
    children: [{ blockId: 'b-comps', label: 'Comparables' }],
  },
  { blockId: 'b-2-h1', label: 'How the rate is taken', number: 2, children: [] },
];

describe('ReportOutline', () => {
  it('renders nothing at all when the report has no sections', () => {
    // §6.1 — a zero-row section is not rendered, and that applies to this rail
    // too. A v1 report (no blocks, no anchors) is this case.
    const { container } = render(<ReportOutline items={[]} />);
    expect(container.innerHTML).toBe('');
  });

  it('is one row per outline entry, numbered sections and unnumbered children', () => {
    render(<ReportOutline items={ITEMS} />);
    expect(screen.getAllByRole('button').map((row) => row.textContent)).toEqual([
      '01Valuation conclusion',
      'Comparables',
      '02How the rate is taken',
    ]);
  });

  // The label is in the DOM at rest and only *looks* like a dot: collapsing it
  // by not rendering it would take the accessible name away with it.
  it('keeps every label readable to a screen reader while it looks like a dot', () => {
    render(<ReportOutline items={ITEMS} />);
    expect(screen.getByRole('button', { name: /Comparables/ })).toBeTruthy();
  });

  it('scrolls to the block it names', async () => {
    const onSelect = vi.fn();
    render(<ReportOutline items={ITEMS} onSelect={onSelect} />);
    await userEvent.click(screen.getByRole('button', { name: /Comparables/ }));
    expect(onSelect).toHaveBeenCalledWith('b-comps');
  });

  // One tab stop for the whole rail, then arrows inside it — the same roving
  // contract every other list of rows in the app uses.
  it('is one tab stop, and moves between rows with the arrow keys', async () => {
    render(<ReportOutline items={ITEMS} />);
    const rows = screen.getAllByRole('button');
    expect(rows.filter((row) => row.tabIndex === 0).length).toBe(1);
    rows[0]?.focus();
    await userEvent.keyboard('{ArrowDown}');
    expect(document.activeElement).toBe(rows[1]);
  });

  // INV-A11Y-061 covers the whole report subtree, and an index of a document is
  // the most tempting place in the app to reach for `<a href="#...">`.
  it('emits no native link', () => {
    const { container } = render(<ReportOutline items={ITEMS} />);
    expect(container.querySelectorAll('a').length).toBe(0);
  });
});
