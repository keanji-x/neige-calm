import { cleanup, render, screen } from '@testing-library/react';
import { page } from 'vitest/browser';
import { afterEach, describe, expect, it } from 'vitest';

import '../../../styles/entry.css';
import type { LayoutTable, ReportLayout } from '../../../../../core/domain/report-layout.ts';
import { ReportDocument } from '../document/public.tsx';

afterEach(cleanup);

function transactionLayout(): ReportLayout {
  return {
    version: 1, columns: 1, gap: 'normal', surface: 'plain', items: [{
      kind: 'table', title: '交易日志', span: 1,
      columns: [
        { key: 'date', label: '日期', format: 'text', digits: 0 },
        { key: 'symbol', label: '证券', format: 'text', digits: 0 },
        { key: 'quantity', label: '数量', format: 'number', digits: 0 },
        { key: 'price', label: '价格', format: 'number', digits: 3 },
        { key: 'fee', label: '费用', format: 'number', digits: 2 },
        { key: 'reason', label: '交易原因', format: 'text', digits: 0 },
      ],
      data: { rows: [{ date: '2026-09-09', symbol: 'EXAMPLE.US', quantity: 5000, price: 4.637, fee: 15,
        reason: '补记期初持仓，保留交易依据方便以后复盘。' }] },
    }],
  };
}

function reportTable(item: LayoutTable, width: number) {
  return render(<div style={{ width }}><ReportDocument empty={null} report={{ summary: '', body: '', blocks: [
    { id: 'table', kind: 'layout', payload: { version: 1, columns: 1, gap: 'wide', surface: 'plain', items: [item] } },
  ] }}/></div>);
}

function lineCount(cell: HTMLElement): number {
  const walker = document.createTreeWalker(cell, NodeFilter.SHOW_TEXT);
  const tops = new Set<number>();
  for (let node = walker.nextNode(); node !== null; node = walker.nextNode()) {
    const range = document.createRange();
    range.selectNodeContents(node);
    for (const rect of range.getClientRects()) tops.add(rect.top);
  }
  return tops.size;
}

describe('Template table reading in a Report', () => {
  it('keeps dates and symbols readable on a phone while scrolling only the table', async () => {
    await page.viewport(390, 844);
    render(<ReportDocument empty={null} report={{ summary: '', body: '', blocks: [
      { id: 'log', kind: 'layout', payload: transactionLayout() },
    ] }}/>);
    const table = screen.getByRole('table');
    const scroller = table.parentElement!;
    const date = screen.getByRole('cell', { name: '2026-09-09' });
    const symbol = screen.getByRole('cell', { name: 'EXAMPLE.US' });
    // Measure the rendered text, not a CSS declaration or just the cell's box.
    for (const cell of [date, symbol]) {
      expect(lineCount(cell)).toBe(1);
    }
    expect(scroller.scrollWidth).toBeGreaterThan(scroller.clientWidth);
    expect(document.documentElement.scrollWidth).toBeLessThanOrEqual(window.innerWidth);
    await page.screenshot({ path: '../../../../../test-results/template-table-mobile.png' });
    scroller.scrollLeft = scroller.scrollWidth;
    expect(scroller.scrollLeft).toBeGreaterThan(0);
    const note = screen.getByRole('cell', { name: '补记期初持仓，保留交易依据方便以后复盘。' });
    expect(note.getBoundingClientRect().right).toBeLessThanOrEqual(scroller.getBoundingClientRect().right + 1);
  });

  it('keeps the unframed desktop table within the report measure', async () => {
    await page.viewport(1280, 900);
    render(<div style={{ width: 960 }}><ReportDocument empty={null} report={{ summary: '', body: '', blocks: [
      { id: 'log', kind: 'layout', payload: transactionLayout() },
    ] }}/></div>);
    const table = screen.getByRole('table');
    const scroller = table.parentElement!;
    expect(scroller.scrollWidth).toBe(scroller.clientWidth);
    expect(getComputedStyle(table).backgroundColor).toBe('rgba(0, 0, 0, 0)');
    expect(table.getBoundingClientRect().width).toBeLessThanOrEqual(960);
    await page.screenshot({ path: '../../../../../test-results/template-table-desktop.png' });
  });

  it('fits the actual five-column holdings template on a phone without unnecessary scrolling', async () => {
    await page.viewport(390, 844);
    // Columns from portfolio.md; values from the actual UX trial's market
    // overlay. In particular, an absent event must not reserve a prose width.
    reportTable({ kind: 'table', title: '', span: 1,
      columns: [
        { key: 'name', label: '股票 / 研究档案', format: 'text', digits: 0, fallbackKey: 'asset', suffixKey: 'venue', linkKey: 'track' },
        { key: 'price', label: '现价', format: 'number', digits: 2, suffixKey: 'currency' },
        { key: 'change', label: '当日涨跌', format: 'percent', digits: 2 },
        { key: 'value', label: '仓位', format: 'share', digits: 1 },
        { key: 'nextEvent', label: '下次事件', format: 'text', digits: 0 },
      ], exclude: { key: 'asset', value: 'Total' }, total: { row: { key: 'asset', value: 'Total' }, key: 'value' },
      data: { rows: [
        { asset: '00700', venue: 'HK', price: 433.6, currency: 'HKD', value: 37086.46 },
        { asset: '510300', venue: 'SH', price: 4.64, currency: 'CNY', value: 23185 },
        { asset: 'AAPL', venue: 'US', price: 316.22, currency: 'USD', value: 42434.19 },
        { asset: 'Total', currency: 'CNY', value: 102705.65 },
      ] },
    }, 358);
    const table = screen.getByRole('table');
    expect(table.parentElement!.scrollWidth).toBe(table.parentElement!.clientWidth);
    for (const cell of screen.getAllByRole('cell')) expect(lineCount(cell)).toBe(1);
    await page.screenshot({ path: '../../../../../test-results/template-holdings-mobile.png' });
  });

  it('sizes the actual nine-column journal by its content and wraps only longer prose', async () => {
    await page.viewport(1440, 900);
    // Exact journal shape and fictitious values saved during the UX trial.
    reportTable({ kind: 'table', title: '虚构交易记录', span: 1,
      columns: [
        { key: 'date', label: '日期', format: 'text', digits: 0 },
        { key: 'name', label: '股票', format: 'text', digits: 0, linkKey: 'track' },
        { key: 'side', label: '操作', format: 'text', digits: 0 },
        { key: 'quantity', label: '数量', format: 'number', digits: 4 },
        { key: 'price', label: '成交价', format: 'number', digits: 2, suffixKey: 'currency' },
        { key: 'fee', label: '手续费', format: 'number', digits: 2, suffixKey: 'currency' },
        { key: 'amount', label: '成交金额', format: 'number', digits: 2, suffixKey: 'currency' },
        { key: 'outflow', label: '含费支出', format: 'number', digits: 2, suffixKey: 'currency' },
        { key: 'reason', label: '当时的理由', format: 'text', digits: 0 },
      ], data: { rows: [{ date: '2026-09-09', name: '苹果（US:AAPL）', side: '买入（虚构）', quantity: 20,
        price: 200, fee: 1, amount: 4000, outflow: 4001, currency: 'USD', reason: '测试跨市场持仓记录；已验证模板保存' }] },
    }, 568);
    const table = screen.getByRole('table');
    expect(table.getBoundingClientRect().width).toBeLessThan(800);
    for (const name of ['2026-09-09', '苹果（US:AAPL）', '买入（虚构）']) {
      expect(lineCount(screen.getByRole('cell', { name }))).toBe(1);
    }
    expect(lineCount(screen.getByRole('cell', { name: '测试跨市场持仓记录；已验证模板保存' }))).toBeGreaterThan(1);
    expect(document.documentElement.scrollWidth).toBeLessThanOrEqual(window.innerWidth);
    await page.screenshot({ path: '../../../../../test-results/template-journal-desktop.png' });
  });

  it('does not reserve a prose-sized column for short asset types and currency codes', async () => {
    await page.viewport(1440, 900);
    reportTable({ kind: 'table', title: '虚构资产登记', span: 1,
      columns: [
        { key: 'type', label: '资产类别', format: 'text', digits: 0 },
        { key: 'name', label: '资产', format: 'text', digits: 0 },
        { key: 'code', label: '代码', format: 'text', digits: 0 },
        { key: 'quantity', label: '金额 / 数量', format: 'number', digits: 0, suffixKey: 'unit' },
        { key: 'currency', label: '币种', format: 'text', digits: 0 },
        { key: 'status', label: '数据说明', format: 'text', digits: 0 },
      ], data: { rows: [
        { type: '现金', name: '人民币现金', code: 'CNY', quantity: 100000, unit: '元', currency: 'CNY', status: '现金单列于 Report，不登记为证券' },
        { type: 'ETF', name: '沪深300 ETF', code: 'SH:510300', quantity: 5000, unit: '份', currency: 'CNY', status: '已同步并回读核验数量；已取得行情及人民币估值' },
        { type: '股票', name: '腾讯', code: 'HK:00700', quantity: 100, unit: '股', currency: 'HKD', status: '已同步并回读核验数量；已取得行情及人民币估值' },
        { type: '股票', name: '苹果', code: 'US:AAPL', quantity: 20, unit: '股', currency: 'USD', status: '已同步并回读核验数量；已取得行情及人民币估值' },
      ] },
    }, 568);
    const table = screen.getByRole('table');
    expect(table.parentElement!.scrollWidth).toBe(table.parentElement!.clientWidth);
    for (const name of ['人民币现金', '沪深300 ETF', 'SH:510300']) expect(lineCount(screen.getByRole('cell', { name }))).toBe(1);
  });
});
