// @vitest-environment jsdom
import { cleanup, render, screen } from '@testing-library/react';
import { afterEach, describe, expect, it, vi } from 'vitest';

import type { ReportSourceLinkTarget } from '../../../../../core/domain/report-source.ts';
import { SOURCE_PANEL_COPY } from '../source/public.tsx';
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
    const captionless = { columns: PAYLOAD.columns, rows: PAYLOAD.rows, highlight: PAYLOAD.highlight };
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

/*
 * #1687 — a cell that is exactly one `[label](neige://source/…)` link is the
 * same citation the prose paints: a button with the typed callback when a
 * panel can open, the badge-plus-label form when none can. Everything else a
 * cell might hold stays the text it was; this is not a Markdown renderer.
 */
describe('ReportTableBlock — a cell that is one source citation', () => {
  function table(...sources: (string | null)[]) {
    return {
      columns: [
        { key: 'metric', label: '指标' },
        { key: 'source', label: '来源' },
      ],
      rows: sources.map((source, index) => ({ metric: `m${index + 1}`, source })),
    };
  }

  it('renders the cell as the citation control and hands the typed target to the callback', () => {
    const onOpenSourceLink = vi.fn<(target: ReportSourceLinkTarget) => void>();
    const { container } = render(
      <ReportTableBlock
        payload={table('[AP](neige://source/src_ddef99cc#q1)', '[智堡所载UBS摘要](neige://source/src_04d04fc3)')}
        onOpenSourceLink={onOpenSourceLink}
      />,
    );
    expect(container.querySelectorAll('a').length).toBe(0);
    expect(container.innerHTML).not.toContain('neige://source');
    screen.getByRole('button', { name: 'AP' }).click();
    screen.getByRole('button', { name: '智堡所载UBS摘要' }).click();
    expect(onOpenSourceLink.mock.calls.map(([target]) => target)).toEqual([
      { destination: 'neige://source/src_ddef99cc#q1', sourceId: 'src_ddef99cc', quoteId: 'q1' },
      { destination: 'neige://source/src_04d04fc3', sourceId: 'src_04d04fc3', quoteId: null },
    ]);
    expect(container.querySelectorAll('td [data-nc-report-source-link]').length).toBe(2);
  });

  it('renders the badge-plus-label form, not a control, when no handler is injected', () => {
    const { container } = render(
      <ReportTableBlock payload={table('[AP](neige://source/src_ddef99cc#q1)')} />,
    );
    expect(container.querySelectorAll('button, a').length).toBe(0);
    const citation = container.querySelector('td [data-nc-report-source-citation]');
    expect(citation).not.toBeNull();
    expect(citation?.textContent).toBe(`${SOURCE_PANEL_COPY.citationBadge}AP`);
    expect(container.innerHTML).not.toContain('neige://source');
  });

  it('keeps a citation with prose around it as the text it was', () => {
    const onOpenSourceLink = vi.fn();
    const { container } = render(
      <ReportTableBlock
        payload={table('见 [AP](neige://source/src_ddef99cc#q1) 收盘')}
        onOpenSourceLink={onOpenSourceLink}
      />,
    );
    expect(container.querySelectorAll('button, a').length).toBe(0);
    expect(container.querySelector('[data-nc-report-source-link], [data-nc-report-source-citation]')).toBeNull();
    expect(container.querySelector('td:nth-child(2)')?.textContent).toBe('见 [AP](neige://source/src_ddef99cc#q1) 收盘');
  });

  it('keeps two citations in one cell as text', () => {
    const onOpenSourceLink = vi.fn();
    const text = '[AP](neige://source/src_ddef99cc#q1) [UBS](neige://source/src_04d04fc3#q2)';
    const { container } = render(
      <ReportTableBlock payload={table(text)} onOpenSourceLink={onOpenSourceLink} />,
    );
    expect(container.querySelectorAll('button, a').length).toBe(0);
    expect(container.querySelector('td:nth-child(2)')?.textContent).toBe(text);
  });

  it('leaves a link under any other scheme as text — the cell is not a link renderer', () => {
    const onOpenSourceLink = vi.fn();
    const { container } = render(
      <ReportTableBlock
        payload={table('[x](neige://report/b_1#s1)', '[x](https://example.com/a)', '<a href="https://example.com">x</a>')}
        onOpenSourceLink={onOpenSourceLink}
      />,
    );
    expect(container.querySelectorAll('button, a').length).toBe(0);
    expect([...container.querySelectorAll('td:nth-child(2)')].map((cell) => cell.textContent)).toEqual([
      '[x](neige://report/b_1#s1)',
      '[x](https://example.com/a)',
      '<a href="https://example.com">x</a>',
    ]);
  });

  // Same rule as the prose: the panel is where "来源缺失" is said, so a
  // citation that will not parse must still be reachable.
  it('keeps a malformed source id clickable so the panel can say it is missing', () => {
    const onOpenSourceLink = vi.fn<(target: ReportSourceLinkTarget) => void>();
    render(
      <ReportTableBlock payload={table('[坏链接](neige://source/src_zz)')} onOpenSourceLink={onOpenSourceLink} />,
    );
    screen.getByRole('button', { name: '坏链接' }).click();
    expect(onOpenSourceLink).toHaveBeenCalledWith({
      destination: 'neige://source/src_zz', sourceId: null, quoteId: null,
    });
  });

  it('tolerates whitespace around the link, and nothing else', () => {
    const onOpenSourceLink = vi.fn();
    render(
      <ReportTableBlock payload={table('  [AP](neige://source/src_ddef99cc#q1)\n')} onOpenSourceLink={onOpenSourceLink} />,
    );
    expect(screen.getByRole('button', { name: 'AP' })).toBeTruthy();
  });

  it('still addresses the highlighted row by its raw first-column text', () => {
    const { container } = render(
      <ReportTableBlock
        payload={{
          columns: [{ key: 'source', label: '来源' }, { key: 'v', label: 'v' }],
          rows: [{ source: '[AP](neige://source/src_ddef99cc#q1)', v: '1' }, { source: 'x', v: '2' }],
          highlight: '[AP](neige://source/src_ddef99cc#q1)',
        }}
        onOpenSourceLink={vi.fn()}
      />,
    );
    const highlighted = [...container.querySelectorAll('tbody tr')].filter((row) => row.className !== '');
    expect(highlighted.length).toBe(1);
    expect(highlighted[0]?.querySelector('[data-nc-report-source-link]')).not.toBeNull();
  });
});
