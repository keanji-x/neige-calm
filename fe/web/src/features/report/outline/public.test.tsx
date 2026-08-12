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
    // §6.1 — a zero-row section is not rendered, and that applies to its
    // trigger too. A v1 report (no blocks, no anchors) is this case.
    const { container } = render(<ReportOutline items={[]} />);
    expect(container.innerHTML).toBe('');
  });

  it('stays closed until asked', () => {
    render(<ReportOutline items={ITEMS} />);
    expect(screen.getByRole('button', { name: 'Outline' }).getAttribute('aria-expanded')).toBe('false');
    expect(screen.queryByRole('menu')).toBeNull();
  });

  it('numbers sections and leaves children unnumbered', async () => {
    render(<ReportOutline items={ITEMS} />);
    await userEvent.click(screen.getByRole('button', { name: 'Outline' }));
    const rows = screen.getAllByRole('menuitem');
    expect(rows.map((row) => row.textContent)).toEqual([
      '01Valuation conclusion',
      'Comparables',
      '02How the rate is taken',
    ]);
  });

  it('scrolls to the section and closes — one use, then gone', async () => {
    const onSelect = vi.fn();
    render(<ReportOutline items={ITEMS} onSelect={onSelect} />);
    await userEvent.click(screen.getByRole('button', { name: 'Outline' }));
    await userEvent.click(screen.getByRole('menuitem', { name: /Comparables/ }));
    expect(onSelect).toHaveBeenCalledWith('b-comps');
    expect(screen.queryByRole('menu')).toBeNull();
  });

  it('closes on Escape and gives the focus back to its trigger', async () => {
    render(<ReportOutline items={ITEMS} />);
    const trigger = screen.getByRole('button', { name: 'Outline' });
    await userEvent.click(trigger);
    await userEvent.keyboard('{Escape}');
    expect(screen.queryByRole('menu')).toBeNull();
    expect(document.activeElement).toBe(trigger);
  });

  // INV-A11Y-061 covers the whole report subtree, and an index of a document is
  // the most tempting place in the app to reach for `<a href="#...">`.
  it('emits no native link', async () => {
    const { container } = render(<ReportOutline items={ITEMS} />);
    await userEvent.click(screen.getByRole('button', { name: 'Outline' }));
    expect(container.querySelectorAll('a').length).toBe(0);
  });
});
