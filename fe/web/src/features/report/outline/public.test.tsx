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
    const { container } = render(<ReportOutline items={[]} />);
    expect(container.innerHTML).toBe('');
  });

  it('renders only first-level sections, in document order', () => {
    render(<ReportOutline items={ITEMS} />);
    expect(screen.getAllByRole('button').map((row) => row.getAttribute('aria-label'))).toEqual([
      'Valuation conclusion',
      'How the rate is taken',
    ]);
    expect(screen.queryByRole('button', { name: /Comparables/ })).toBeNull();
  });

  it('keeps every first-level label readable to a screen reader while it looks like a dot', () => {
    render(<ReportOutline items={ITEMS} />);
    expect(screen.getByRole('button', { name: /Valuation conclusion/ })).toBeTruthy();
  });

  it('scrolls to the block it names', async () => {
    const onSelect = vi.fn();
    render(<ReportOutline items={ITEMS} onSelect={onSelect} />);
    await userEvent.click(screen.getByRole('button', { name: /Valuation conclusion/ }));
    expect(onSelect).toHaveBeenCalledWith('b-1-h1');
  });

  it('is one tab stop, and moves between rows with the arrow keys', async () => {
    render(<ReportOutline items={ITEMS} />);
    const rows = screen.getAllByRole('button');
    expect(rows.filter((row) => row.tabIndex === 0).length).toBe(1);
    rows[0]?.focus();
    await userEvent.keyboard('{ArrowDown}');
    expect(document.activeElement).toBe(rows[1]);
  });

  it('shows the focused chapter for keyboard navigation and dismisses it on leaving', async () => {
    render(<><ReportOutline items={ITEMS} /><button type="button">After outline</button></>);
    const preview = () => document.querySelector('[data-nc-rail-preview]')?.textContent;
    await userEvent.tab();
    expect(preview()).toBe('Valuation conclusion');
    await userEvent.keyboard('{ArrowDown}');
    expect(preview()).toBe('How the rate is taken');
    await userEvent.tab();
    expect(document.activeElement).toBe(screen.getByRole('button', { name: 'After outline' }));
    expect(preview()).toBeUndefined();
  });

  it('emits no native link', () => {
    const { container } = render(<ReportOutline items={ITEMS} />);
    expect(container.querySelectorAll('a').length).toBe(0);
  });
});
