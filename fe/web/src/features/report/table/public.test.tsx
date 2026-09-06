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

describe('ReportTableBlock — live tables', () => {
  const LIVE = { source: 'neige://plugin/dev-neige-binance/portfolio.holdings' };

  it('renders the rows the resolver returns, so a plugin push reaches the reader', () => {
    render(<ReportTableBlock payload={LIVE} resolveLive={() => PAYLOAD} />);
    expect(screen.getAllByRole('columnheader').map((cell) => cell.textContent))
      .toEqual(['Company', 'P/E']);
    expect(screen.getByText('600519.SH')).toBeTruthy();
  });

  it('passes the block its own source, not some other block\'s', () => {
    const asked: string[] = [];
    render(<ReportTableBlock payload={LIVE} resolveLive={(source) => { asked.push(source); return PAYLOAD; }} />);
    expect(asked).toEqual(['neige://plugin/dev-neige-binance/portfolio.holdings']);
  });

  it('prefers the pushed payload\'s caption over the block\'s standing one', () => {
    render(<ReportTableBlock
      payload={{ ...LIVE, caption: 'Portfolio' }}
      resolveLive={() => ({ ...PAYLOAD, caption: 'Priced at 12:00Z' })}
    />);
    expect(screen.getByText('Priced at 12:00Z')).toBeTruthy();
    expect(screen.queryByText('Portfolio')).toBeNull();
  });

  it('falls back to the block caption when the pushed payload has none', () => {
    const { caption: _dropped, ...captionless } = PAYLOAD;
    render(<ReportTableBlock payload={{ ...LIVE, caption: 'Portfolio' }} resolveLive={() => captionless} />);
    expect(screen.getByText('Portfolio')).toBeTruthy();
  });

  // The three states an inline table never has. Each says something rather
  // than rendering nothing: a live block that silently disappears reads as a
  // report that never had one.
  it('says so when nothing has been pushed to the source yet', () => {
    render(<ReportTableBlock payload={LIVE} resolveLive={() => undefined} />);
    expect(screen.getByText(/Waiting for neige:\/\/plugin\/dev-neige-binance\/portfolio\.holdings/)).toBeTruthy();
    expect(screen.queryByRole('table')).toBeNull();
  });

  it('says so when the overlay holds something that is not a table', () => {
    render(<ReportTableBlock payload={LIVE} resolveLive={() => ({ columns: 'nope' })} />);
    expect(screen.getByText(/cannot read as a table/)).toBeTruthy();
    expect(screen.queryByRole('table')).toBeNull();
  });

  it('says so on a surface that carries no live data at all', () => {
    render(<ReportTableBlock payload={LIVE} />);
    expect(screen.getByText(/does not carry live data/)).toBeTruthy();
    expect(screen.queryByRole('table')).toBeNull();
  });
});
