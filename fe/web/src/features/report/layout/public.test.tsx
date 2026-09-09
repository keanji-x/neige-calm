import { cleanup, render, screen, within } from '@testing-library/react';
import { afterEach, describe, expect, it } from 'vitest';

import type { LayoutTable, ReportLayout } from '../../../../../core/domain/report-layout.ts';
import { ReportLayoutBlock } from './public.tsx';

afterEach(cleanup);

function layout(data: LayoutTable['data']): ReportLayout {
  return {
    version: 1, columns: 1, gap: 'normal', surface: 'plain', items: [{
      kind: 'table', title: '持仓', span: 1, data,
      columns: [
        { key: 'name', label: '股票', format: 'text', digits: 0 },
        { key: 'price', label: '价格', format: 'number', digits: 3 },
        { key: 'event', label: '下次事件', format: 'text', digits: 0 },
      ],
    }],
  };
}

describe('Template table states', () => {
  it.each([
    { label: 'missing resolver', resolveLive: undefined, status: '等待数据来源更新。' },
    { label: 'waiting source', resolveLive: () => undefined, status: '等待数据来源更新。' },
    { label: 'failed source', resolveLive: () => { throw new Error('离线'); }, status: '数据不可用：离线' },
    { label: 'empty source', resolveLive: () => ({ rows: [] }), status: '暂无记录。' },
  ])('keeps configured headers and an in-table status for $label', ({ resolveLive, status }) => {
    render(<ReportLayoutBlock payload={layout({ source: 'neige://plugin/example/positions' })} resolveLive={resolveLive}/>);
    const table = screen.getByRole('table');
    expect(within(table).getAllByRole('columnheader').map(header => header.textContent)).toEqual(['股票', '价格', '下次事件']);
    expect(within(table).getByRole('status').textContent).toBe(status);
    expect(within(table).getByRole('status').closest('td')?.colSpan).toBe(3);
  });

  it('keeps empty inline records in the same table and replaces the status when data arrives', () => {
    const { rerender } = render(<ReportLayoutBlock payload={layout({ rows: [] })}/>);
    expect(within(screen.getByRole('table')).getByRole('status').textContent).toBe('暂无记录。');
    rerender(<ReportLayoutBlock payload={layout({ rows: [{ name: '示例基金', price: 4.637, event: '2026-10-01' }] })}/>);
    const table = screen.getByRole('table');
    expect(within(table).queryByRole('status')).toBeNull();
    expect(within(table).getAllByRole('cell').map(cell => cell.textContent)).toEqual(['示例基金', '4.637', '2026-10-01']);
  });
});
