// @vitest-environment jsdom
import { cleanup, render, screen } from '@testing-library/react';
import { afterEach, describe, expect, it } from 'vitest';

import { ReportTableBlock } from './public.tsx';

afterEach(cleanup);

const PAYLOAD = {
  caption: 'Comparables',
  highlight: '600519.SH',
  columns: [
    { key: 'name', label: 'Company' },
    { key: 'pe', label: 'P/E', align: 'right' as const },
  ],
  rows: [
    { name: '600519.SH', pe: '28.4' },
    { name: '000858.SZ', pe: null },
  ],
};

describe('ReportTableBlock', () => {
  it('renders header cells as headers, so the table is navigable by column', () => {
    render(<ReportTableBlock payload={PAYLOAD} />);
    expect(screen.getAllByRole('columnheader').map((cell) => cell.textContent))
      .toEqual(['Company', 'P/E']);
  });

  it('highlights the row the report is talking about, addressed by its first column', () => {
    const { container } = render(<ReportTableBlock payload={PAYLOAD} />);
    const highlighted = [...container.querySelectorAll('tbody tr')]
      .filter((row) => row.className !== '');
    expect(highlighted.length).toBe(1);
    expect(highlighted[0]?.textContent).toContain('600519.SH');
  });

  // A missing number is a blank cell, not the string "null" and not a zero:
  // the row still exists, and inventing a value would be worse than a gap.
  it('renders a null cell as empty', () => {
    const { container } = render(<ReportTableBlock payload={PAYLOAD} />);
    const cells = container.querySelectorAll('tbody tr:nth-child(2) td');
    expect(cells[1]?.textContent).toBe('');
  });
});
